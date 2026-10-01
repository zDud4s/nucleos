import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, screen, waitFor, within } from "@testing-library/react";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

const daemon = vi.hoisted(() => ({ apiFetch: vi.fn(), apiText: vi.fn(), probeHealth: vi.fn() }));
vi.mock("../data/client", async (original) => ({
  ...(await original<typeof import("../data/client")>()),
  ...daemon,
}));

import { Autopilot } from "./Autopilot";
import { ApiRefusal } from "../data/client";
import type { ClassTally, ScopedKill, ShadowDecision } from "../data/autopilot";
import type { FeedEntry } from "../data/feed";
import type { Job } from "../data/fleet";
import type { AutopilotMode, ProjectSummary, Proposal } from "../data/system";
import { daemonState, project, proposal, renderApp, renderWithRouter } from "../test/harness";
import {
  MODE_MEANING,
  MODE_SENTENCES,
  PROMOTION_ACTING,
  PROMOTION_EARNED,
  prerequisiteText,
  promotionBlocker,
  promotionConsequence,
} from "../lib/mode";

beforeEach(() => {
  daemon.apiFetch.mockReset();
  daemon.apiText.mockReset();
  daemon.probeHealth.mockReset();

  // Up and authorising, for the one case below that mounts the whole app and so
  // has to get past the connection gate.
  daemon.probeHealth.mockResolvedValue(true);
  daemon.apiText.mockResolvedValue("daemon running");
  localStorage.clear();
});

/**
 * The page inside a real router, and nothing else.
 *
 * `renderApp` would mount the gate, the rail and its three live queries around
 * every one of these assertions — a cost per test that buys nothing here, and
 * one this machine cannot pay five times over without pushing other suites past
 * their timeouts. The single case that needs the whole app is the one that
 * proves the route is registered, and it says so where it does it.
 */
function renderCockpit() {
  return renderWithRouter(<Autopilot />, { initialPath: "/autopilot" });
}

/* ------------------------------------------------------------- fixtures -- */

function tally(overrides: Partial<ClassTally> = {}): ClassTally {
  return {
    mode: "shadow",
    action_class: "read-local",
    total: 12,
    would_allow: 12,
    would_pend: 0,
    would_deny: 0,
    reviewed: 11,
    agree: 11,
    disagree: 0,
    ...overrides,
  };
}

function decision(overrides: Partial<ShadowDecision> = {}): ShadowDecision {
  return {
    id: 301,
    run_id: 44,
    tool_name: "Bash",
    tool_input: JSON.stringify({ command: "git push origin main" }),
    decision: "pending_approval",
    reason: "pushing is outside what this run was asked to do",
    action_class: "push-merge-deploy",
    classifier_version: 7,
    human_verdict: null,
    reviewed_at: null,
    created_at: "2026-08-17T09:00:00Z",
    ...overrides,
  };
}

/* ------------------------------------------------------------ the daemon -- */

interface CockpitWorld {
  projects: ProjectSummary[];
  proposals: Proposal[];
  scopedKills: ScopedKill[];
  decisions: ShadowDecision[];
  scoreboard: ClassTally[];
  jobs: Job[];
  feed: FeedEntry[];
  /** What `POST /autopilot/state` refuses with, when it refuses. */
  refuseMode: ApiRefusal | null;
}

function cockpitWorld(overrides: Partial<CockpitWorld> = {}): CockpitWorld {
  return {
    projects: [],
    proposals: [],
    scopedKills: [],
    decisions: [],
    scoreboard: [],
    jobs: [],
    feed: [],
    refuseMode: null,
    ...overrides,
  };
}

/**
 * The cockpit's routes, over mutable state.
 *
 * A local switch rather than an edit to `test/harness.tsx`, for the reason the
 * queue's suite gives: the harness is the shared floor, and a page that teaches
 * it nine routes of its own makes every other suite carry them.
 *
 * The POST arm is the interesting half. `POST /autopilot/state` answers **200
 * with a body** on success and a **bare 422 with an empty body** when a
 * prerequisite is missing, and both of those are what the page is built around
 * — so the responder models both rather than resolving `undefined` for every
 * write.
 */
function cockpitFetch(world: CockpitWorld): (path: string, init?: RequestInit) => Promise<unknown> {
  const shared = daemonState();
  return async (path, init) => {
    if (init?.method === "POST") {
      if (path === "/autopilot/state") {
        if (world.refuseMode !== null) throw world.refuseMode;
        const body = JSON.parse(String(init.body)) as { project_id: string; mode: AutopilotMode };
        world.projects = world.projects.map((row) =>
          row.project_id === body.project_id ? { ...row, mode: body.mode } : row,
        );
        return { project_id: body.project_id, mode: body.mode };
      }
      if (path === "/autopilot/kill/scoped") {
        const body = JSON.parse(String(init.body)) as ScopedKill;
        world.scopedKills = [
          ...world.scopedKills.filter(
            (row) => !(row.scope_type === body.scope_type && row.scope_id === body.scope_id),
          ),
          body,
        ];
        return undefined;
      }
      // 204 everywhere else on this page — the verdict door included.
      return undefined;
    }

    if (path === "/autopilot/kill") return shared.kill;
    if (path === "/autopilot/budget") return shared.budget;
    if (path === "/projects") return world.projects;
    if (path === "/proposals") return world.proposals;
    if (path === "/autopilot/kill/scoped") return world.scopedKills;
    if (path.startsWith("/scoreboard")) return world.scoreboard;
    if (path.startsWith("/shadow-decisions")) return world.decisions;
    if (path.startsWith("/jobs")) return world.jobs;
    if (path.startsWith("/feed")) return world.feed;
    if (path.startsWith("/autopilot/judge-resolve")) {
      const project = new URLSearchParams(path.split("?")[1] ?? "").get("project_id") ?? "";
      return { project_id: project, judge_resolve: "off" };
    }
    return undefined;
  };
}

/** The list item a named switch belongs to. */
function switchFor(label: string): HTMLElement {
  const list = screen.getByRole("list", { name: "Trigger brakes" });
  const row = within(list).getByText(label).closest("li");
  if (row === null) throw new Error(`no switch row for ${label}`);
  return row;
}

/* ------------------------------------ the carousel: one project at a time -- */

/**
 * A roster shaped like the preview's, handed over in an order that is NOT the urgent one.
 *
 * bravo acts with a full queue (an exception), alpha has earned the third setting, delta is still
 * short of the bar and charlie is off — so most urgent first is bravo, alpha, delta, charlie, while
 * the roster itself says alpha, bravo, charlie, delta. A page that drew the roster as it was sent
 * would open on alpha, and every case that reads the order would say so.
 */
function previewRoster(): ProjectSummary[] {
  return [
    project({
      project_id: "alpha",
      mode: "shadow",
      project_root: "C:/repos/alpha",
      promotable: true,
      classes_ready: 5,
      classes_total: 5,
      withheld_classes_ready: 2,
      open_review_items: 3,
      wip_limit: 4,
    }),
    project({
      project_id: "bravo",
      mode: "active",
      project_root: "C:/repos/bravo",
      queue_full: true,
      classes_ready: 3,
      classes_total: 3,
      withheld_classes_ready: 1,
      open_review_items: 4,
      wip_limit: 4,
    }),
    project({ project_id: "charlie", mode: "off", project_root: "C:/repos/charlie" }),
    project({
      project_id: "delta",
      mode: "shadow",
      project_root: "C:/repos/delta",
      classes_ready: 2,
      classes_total: 5,
      withheld_classes_ready: 0,
    }),
  ];
}

const ROSTER_IDS = ["alpha", "bravo", "charlie", "delta"];

function rowOf(roster: readonly ProjectSummary[], id: string): ProjectSummary {
  const found = roster.find((row) => row.project_id === id);
  if (found === undefined) throw new Error(`no fixture row for ${id}`);
  return found;
}

/** The index's tabs, in the order they are drawn. */
function tabs(): HTMLElement[] {
  const index = screen.getByRole("tablist", { name: "Projects, most urgent first" });
  return within(index).getAllByRole("tab");
}

/**
 * Which project each tab names, in order.
 *
 * A tab's text STARTS with its project's id (`describeProject`), whatever the sentence says after
 * it, so the id is read off the front rather than matched against the whole description.
 */
function tabOrder(): string[] {
  return tabs().map(
    (tab) =>
      ROSTER_IDS.find((id) => tab.textContent?.startsWith(id) === true) ??
      `unnamed: ${tab.textContent ?? ""}`,
  );
}

/** The one selected tab — exactly one, or the index is lying about what the panel shows. */
function selectedTab(): HTMLElement {
  const chosen = tabs().filter((tab) => tab.getAttribute("aria-selected") === "true");
  expect(chosen).toHaveLength(1);
  return chosen[0];
}

/** The focus panel under the fan: the selected project's setting, and nobody else's. */
function panel(): HTMLElement {
  return screen.getByRole("tabpanel");
}

/** The panel's gate. Always one, whatever the project's mode. */
function gate(): HTMLElement {
  const found = panel().querySelectorAll<HTMLElement>(".ap-fan-gate");
  expect(found).toHaveLength(1);
  return found[0];
}

/** A key pressed on whatever has focus, the way a keyboard presses it. */
function press(key: string): void {
  const target = document.activeElement;
  if (target === null) throw new Error(`nothing has focus to press ${key} on`);
  fireEvent.keyDown(target, { key });
}

/**
 * Focus and selection land together on the tab at `index`.
 *
 * Selection follows focus in this index: the arrow that moves the ring also moves what the panel
 * is about, so a tab that took focus without being selected would be the one mismatch a keyboard
 * user could not see from where they are.
 */
async function landsOn(index: number): Promise<void> {
  await waitFor(() => {
    const all = tabs();
    expect(document.activeElement).toBe(all[index]);
    expect(all[index].getAttribute("aria-selected")).toBe("true");
    expect(all.map((tab) => tab.getAttribute("tabindex"))).toEqual(
      all.map((_, at) => (at === index ? "0" : "-1")),
    );
  });
}

describe("Autopilot - the carousel sets one project at a time, most urgent first", () => {
  /**
   * The index above the fan is the keyboard's way in; the stage never takes focus.
   *
   * One tab stop and not four: the selected tab is `tabIndex` 0 and every other is -1, so Tab
   * enters the index once and leaves it on the next press, and the arrows are what walk it. Three
   * projects or more are a ring, so the arrows wrap; Home and End go to the ends.
   */
  it("the index is a tablist with one tab stop, and the arrows, home and end walk it", async () => {
    daemon.apiFetch.mockImplementation(cockpitFetch(cockpitWorld({ projects: previewRoster() })));

    await renderCockpit();

    const index = await screen.findByRole("tablist", { name: "Projects, most urgent first" });
    expect(within(index).getAllByRole("tab")).toHaveLength(4);
    expect(tabs().map((tab) => tab.getAttribute("tabindex"))).toEqual(["0", "-1", "-1", "-1"]);
    expect(selectedTab()).toBe(tabs()[0]);

    // Every tab controls the one panel, and the panel is labelled by whichever tab is selected.
    const shown = panel();
    expect(shown.id).not.toBe("");
    for (const tab of tabs()) {
      expect(tab.id).not.toBe("");
      expect(tab.getAttribute("aria-controls")).toBe(shown.id);
    }
    expect(shown.getAttribute("aria-labelledby")).toBe(tabs()[0].id);

    tabs()[0].focus();
    press("ArrowRight");
    await landsOn(1);
    expect(panel().getAttribute("aria-labelledby")).toBe(tabs()[1].id);

    press("ArrowLeft");
    await landsOn(0);
    // A ring: left of the first is the last, and right of the last is the first again.
    press("ArrowLeft");
    await landsOn(3);
    press("ArrowRight");
    await landsOn(0);

    press("End");
    await landsOn(3);
    press("Home");
    await landsOn(0);
  });

  /**
   * The fan opens on what needs you, not on whatever the roster happened to list first.
   *
   * An exception (a full queue, or a setting the núcleo refused) outranks acting, acting outranks
   * earned, earned outranks watching, and off comes last — ties keep the roster's order. The card
   * in focus wears the map's word for its mode: `active`, the daemon's literal, never "acting".
   */
  it("orders the roster most urgent first and opens on the most urgent project, whose card wears the map's word", async () => {
    daemon.apiFetch.mockImplementation(cockpitFetch(cockpitWorld({ projects: previewRoster() })));

    await renderCockpit();

    await screen.findByRole("tablist", { name: "Projects, most urgent first" });
    expect(tabOrder()).toEqual(["bravo", "alpha", "delta", "charlie"]);
    expect(selectedTab().textContent?.startsWith("bravo")).toBe(true);
    expect(panel().querySelector(".ap-fan-focus-title strong")?.textContent).toBe("bravo");

    // The stage is drawing, not controls: hidden from assistive technology as a whole, so the
    // focused card is read off the page by its class and not by any role.
    const card = document.querySelector(".ap-fan-card-focus");
    expect(card).not.toBeNull();
    expect(card?.closest('[aria-hidden="true"]')).not.toBeNull();
    expect(card?.textContent).toContain("bravo");
    const words = Array.from(card?.querySelectorAll(".ui-badge") ?? []).map((badge) => badge.textContent);
    expect(words).toContain("active");
    expect(words).not.toContain("acting");

    // And the panels below follow the project the fan opened on.
    expect(await screen.findByText("nothing is waiting for a verdict on bravo.")).toBeDefined();
  });

  /**
   * Re-ranking after a change would move the card somebody just acted on out from under them.
   *
   * alpha turned off ranks last on a fresh sort; held, it keeps its place and stays selected. And
   * the change is not confirmed by a new line on screen — that would push the page down under the
   * hand that made it — only by the pressed segment, and aloud.
   */
  it("does not reorder the index after a mode change and says the change only aloud", async () => {
    const world = cockpitWorld({ projects: previewRoster() });
    daemon.apiFetch.mockImplementation(cockpitFetch(world));

    await renderCockpit();

    await screen.findByRole("tablist", { name: "Projects, most urgent first" });
    tabs()[0].focus();
    press("ArrowRight");
    await landsOn(1);
    expect(selectedTab().textContent?.startsWith("alpha")).toBe(true);

    fireEvent.click(within(panel()).getByRole("button", { name: "Turn off" }));

    await waitFor(() => {
      expect(
        within(panel()).getByRole("button", { name: "Turn off" }).getAttribute("aria-pressed"),
      ).toBe("true");
    });
    expect(rowOf(world.projects, "alpha").mode).toBe("off");

    expect(tabOrder()).toEqual(["bravo", "alpha", "delta", "charlie"]);
    expect(selectedTab().textContent?.startsWith("alpha")).toBe(true);

    const said = `alpha is now off — ${MODE_MEANING.off}`;
    await waitFor(() => {
      const regions = Array.from(document.querySelectorAll('[aria-live="polite"]'));
      expect(regions.map((region) => region.textContent)).toContain(said);
    });
    for (const element of screen.getAllByText(said)) {
      expect(element.closest(".sr-only")).not.toBeNull();
    }
  });

  /**
   * The gate is a line every project has, so the panel never grows or shrinks by one as the fan
   * turns. Acting: how to stop it. Earned: that it has. Otherwise: why not, in the núcleo's terms.
   * Armed: what confirming would do — in the same line, which "Let it act" is described by.
   */
  it("states the gate for every project, an acting one included, and shows the consequence in it while armed", async () => {
    const roster = previewRoster();
    daemon.apiFetch.mockImplementation(cockpitFetch(cockpitWorld({ projects: roster })));

    await renderCockpit();

    await screen.findByRole("tablist", { name: "Projects, most urgent first" });

    // bravo acts: the gate is passed, and the line says how to take it back.
    expect(gate().textContent).toBe(PROMOTION_ACTING);
    expect(gate().className).not.toContain("ap-fan-gate-locked");
    expect(gate().id).not.toBe("");

    const describedGate = () =>
      document.getElementById(
        within(panel()).getByRole("button", { name: "Let it act" }).getAttribute("aria-describedby") ??
          "",
      );

    tabs()[0].focus();
    press("ArrowRight");
    await landsOn(1);
    expect(gate().textContent).toBe(PROMOTION_EARNED);
    expect(gate().className).not.toContain("ap-fan-gate-locked");
    expect(describedGate()).toBe(gate());

    press("ArrowRight");
    await landsOn(2);
    const delta = rowOf(roster, "delta");
    expect(gate().textContent).toBe(promotionBlocker(delta, delta.withheld_classes_ready ?? 0));
    expect(gate().className).toContain("ap-fan-gate-locked");
    expect(describedGate()).toBe(gate());

    press("ArrowRight");
    await landsOn(3);
    const charlie = rowOf(roster, "charlie");
    expect(gate().textContent).toBe(promotionBlocker(charlie, charlie.withheld_classes_ready ?? 0));
    expect(gate().className).toContain("ap-fan-gate-locked");
    expect(describedGate()).toBe(gate());

    // Back to alpha, and arm it: the gate now says what confirming would do. Read through the
    // description and not by text — while armed the sentence is also in the interlock's live
    // region, and a text query would fail on the ambiguity rather than on anything being wrong.
    press("Home");
    await landsOn(0);
    press("ArrowRight");
    await landsOn(1);
    fireEvent.click(within(panel()).getByRole("button", { name: "Let it act" }));

    const armed = within(panel()).getByRole("button", { name: "Let alpha act" });
    const described = document.getElementById(armed.getAttribute("aria-describedby") ?? "");
    expect(described).toBe(gate());
    expect(gate().textContent).toContain(promotionConsequence(rowOf(roster, "alpha")));
  });

  /**
   * Locked and still reachable. A native `disabled` takes the button out of the tab order, and
   * with it the one sentence that says why it is locked; `aria-disabled` keeps both, and the
   * press does nothing.
   */
  it("keeps let it act focusable and inert while the núcleo says the project is not ready", async () => {
    const beta = project({
      project_id: "beta",
      mode: "shadow",
      project_root: "C:/repos/beta",
      promotable: false,
      classes_ready: 0,
      classes_total: 0,
      withheld_classes_ready: 0,
    });
    daemon.apiFetch.mockImplementation(cockpitFetch(cockpitWorld({ projects: [beta] })));

    await renderCockpit();

    const offer = (await screen.findByRole("button", { name: "Let it act" })) as HTMLButtonElement;
    expect(offer.getAttribute("aria-disabled")).toBe("true");
    expect(offer.disabled).toBe(false);
    offer.focus();
    expect(document.activeElement).toBe(offer);

    const why = document.getElementById(offer.getAttribute("aria-describedby") ?? "");
    expect(why).not.toBeNull();
    expect(why?.className).toContain("ap-fan-gate");
    expect(why?.className).toContain("ap-fan-gate-locked");
    expect(why?.textContent).toBe(promotionBlocker(beta, 0));

    fireEvent.click(offer);
    expect(screen.queryByRole("button", { name: "Let beta act" })).toBeNull();
    expect(offer.getAttribute("aria-disabled")).toBe("true");
    expect(daemon.apiFetch.mock.calls.some(([path]) => path === "/autopilot/state")).toBe(false);
  });

  /**
   * The named 422: a project nobody onboarded. The page says so — no workflow path anywhere — and
   * offers onboarding in place; confirming it sends the gate as shown and asks for the mode again.
   */
  it("a project that was not onboarded is offered onboarding, and the change is retried after it", async () => {
    const world = cockpitWorld({
      projects: [project({ project_id: "alpha", mode: "off", project_root: "C:/repos/alpha" })],
      refuseMode: new ApiRefusal(422, "not_onboarded", "not_onboarded"),
    });
    const base = cockpitFetch(world);
    const sent: unknown[] = [];
    const view = (onboarded: boolean) => ({
      project_id: "alpha",
      root: "C:/repos/alpha",
      onboarded: onboarded
        ? {
            onboarded_at: "now",
            project_root: "C:/repos/alpha",
            gate_command: "make ci",
            harnesses: [],
            hook_installed: true,
            migrated: false,
          }
        : null,
      marker_path: "~/.nucleos/projects/alpha/onboarded.yaml",
      harnesses: [{ path: ".claude", what: "skills", files: 3 }],
      proposed_gate: { command: "make ci", source: "Makefile" },
      configured_gate: null,
      hook_wired: onboarded,
    });
    daemon.apiFetch.mockImplementation(async (path: string, init?: RequestInit) => {
      if (path.startsWith("/projects/alpha/onboard")) {
        if (init?.method === "POST") {
          sent.push(JSON.parse(String(init.body)));
          world.refuseMode = null;
          return view(true);
        }
        return view(false);
      }
      return base(path, init);
    });

    await renderCockpit();
    fireEvent.click(await screen.findByRole("button", { name: "Watch in shadow" }));

    const note = await waitFor(() => {
      const found = document.querySelector<HTMLElement>(".ap-fan-refusal");
      expect(found).not.toBeNull();
      return found as HTMLElement;
    });
    expect(note.textContent).toContain("has not been onboarded");
    expect(note.textContent).not.toMatch(/\.ai\/|workflow\.md/);
    expect(screen.queryByLabelText("Folder for alpha")).toBeNull();

    fireEvent.click(screen.getByRole("button", { name: "Onboard alpha" }));
    const gate = (await screen.findByLabelText("Gate command")) as HTMLInputElement;
    expect(gate.value).toBe("make ci");
    fireEvent.click(screen.getByRole("button", { name: "onboard it" }));

    await waitFor(() => expect(sent).toEqual([{ project_root: "C:/repos/alpha", gate_command: "make ci" }]));
    await waitFor(() => {
      expect(daemon.apiFetch).toHaveBeenCalledWith("/autopilot/state", {
        method: "POST",
        body: JSON.stringify({ project_id: "alpha", mode: "shadow", project_root: "C:/repos/alpha" }),
      });
      expect(world.projects[0].mode).toBe("shadow");
    });
  });

  /**
   * The bare 422: three prerequisites share one status with an empty body, so the page lists them
   * all — each path drawn as a path — and admits it does not know which is missing. Then it hands
   * focus to the one of them the shell can supply, and the retry sends the refused mode again with
   * the folder that was typed.
   */
  it("a bare 422 lists every prerequisite, moves focus to the folder field, and the retry carries the folder", async () => {
    const world = cockpitWorld({
      projects: [
        project({
          project_id: "alpha",
          mode: "shadow",
          project_root: null,
          promotable: true,
          classes_ready: 2,
          classes_total: 2,
          withheld_classes_ready: 1,
        }),
      ],
      // Exactly what `client.ts` builds out of a 422 with an empty body: the
      // status text, which is the code spelled with capital letters.
      refuseMode: new ApiRefusal(422, "unprocessable", "Unprocessable Entity"),
    });
    daemon.apiFetch.mockImplementation(cockpitFetch(world));

    await renderCockpit();

    expect(screen.queryByLabelText("Folder for alpha")).toBeNull();
    fireEvent.click(await screen.findByRole("button", { name: "Let it act" }));

    // Clicks inside the 300 ms dwell are swallowed and leave the control armed,
    // so retrying until it disarms is safe.
    const ARMED = "Let alpha act";
    await waitFor(() => {
      const armed = screen.queryByRole("button", { name: ARMED });
      if (armed !== null) fireEvent.click(armed);
      expect(screen.queryByRole("button", { name: ARMED })).toBeNull();
    });

    await waitFor(() => {
      expect(document.querySelector(".ap-fan-refusal")).not.toBeNull();
    });
    const refusal = document.querySelector<HTMLElement>(".ap-fan-refusal");
    expect(refusal?.className).toContain("ui-note-refusal");
    expect(refusal?.getAttribute("role")).toBe("status");
    expect(refusal?.querySelector(".ui-note-code")?.textContent).toBe("unprocessable");
    expect(refusal?.textContent).toContain(MODE_SENTENCES.unprocessable.lead);
    expect(refusal?.textContent).not.toContain("Unprocessable Entity");

    const every = [...MODE_SENTENCES.unprocessable.items, MODE_SENTENCES.unprocessable.plus];
    const listed = Array.from(refusal?.querySelectorAll("ul.ap-fan-prereqs > li") ?? []);
    expect(listed.map((item) => item.textContent)).toEqual(every.map(prerequisiteText));
    expect(
      Array.from(refusal?.querySelectorAll("ul.ap-fan-prereqs code") ?? []).map((path) => path.textContent),
    ).toEqual(every.flatMap((item) => (item.path === undefined ? [] : [item.path])));

    const input = screen.getByLabelText("Folder for alpha");
    await waitFor(() => {
      expect(document.activeElement).toBe(input);
    });

    world.refuseMode = null;
    fireEvent.change(input, { target: { value: "C:/repos/alpha" } });
    fireEvent.click(screen.getByRole("button", { name: "Try again with this folder" }));

    await waitFor(() => {
      expect(daemon.apiFetch).toHaveBeenCalledWith("/autopilot/state", {
        method: "POST",
        body: JSON.stringify({
          project_id: "alpha",
          mode: "active",
          project_root: "C:/repos/alpha",
        }),
      });
    });
  });

  it("says 1 project on the roster, not 1 projects", async () => {
    daemon.apiFetch.mockImplementation(
      cockpitFetch(cockpitWorld({ projects: [project({ project_id: "alpha", mode: "shadow" })] })),
    );

    await renderCockpit();

    const card = await screen.findByRole("article", { name: "Acting on their own" });
    await waitFor(() => {
      expect(card.querySelector(".ui-stat-detail")?.textContent).toContain("1 project on the roster");
    });
    expect(card.textContent).not.toContain("1 projects");

    // One project is the whole index: nothing to choose between, so no tablist is drawn.
    expect(screen.queryByRole("tablist")).toBeNull();
  });

  /**
   * Names only for what a glance must find — a full queue, and whatever is acting — and only while
   * two of them fit; everything else is a count. Acting, shadow and off partition the roster, so
   * every project is counted once: the full queue and "ready to be let out" are clauses about
   * projects already counted, never a fourth or fifth group.
   */
  it("the headline names the full queue and the acting project and still counts every project once", async () => {
    const roster = previewRoster();
    daemon.apiFetch.mockImplementation(cockpitFetch(cockpitWorld({ projects: roster })));

    const first = await renderCockpit();

    const said =
      "bravo’s queue is full; bravo acts on its own; 2 watching in shadow, 1 of them ready to be let out; 1 off";
    await waitFor(() => {
      expect(document.querySelector(".ap-headline")?.textContent).toBe(said);
    });
    // 1 acting + 2 in shadow + 1 off is every project on the roster, once.
    expect(1 + 2 + 1).toBe(roster.length);
    first.unmount();

    // Three acting is more than a sentence carries by name, so they become a count.
    const many = ["echo", "foxtrot", "golf"].map((id) => project({ project_id: id, mode: "active" }));
    daemon.apiFetch.mockImplementation(cockpitFetch(cockpitWorld({ projects: many })));

    await renderCockpit();

    await waitFor(() => {
      expect(document.querySelector(".ap-headline")?.textContent).toBe("3 projects act on their own");
    });
  });

  it("the page sheet holds the gate, the headline and the stat detail at two lines", () => {
    // Read the way `sheet-layout.test.ts` reads a page sheet: comments stripped, one rule's body.
    const css = readFileSync(join(dirname(fileURLToPath(import.meta.url)), "autopilot.css"), "utf8").replace(
      /\/\*[\s\S]*?\*\//g,
      "",
    );
    const body = (rule: RegExp) => rule.exec(css)?.[1] ?? "";

    // The gate is always two lines tall, one sentence or two, so the switch below it never moves
    // as the fan turns from a short gate to a long one.
    expect(body(/\.ap-fan-gate\s*\{([^}]*)\}/)).toMatch(/min-height:\s*calc\(\s*2lh\s*\+/);
    expect(body(/\.ap-headline\s*\{([^}]*)\}/)).toMatch(/min-height:\s*2lh\b/);
    expect(body(/\.ap-stats\s+\.ui-stat-detail\s*\{([^}]*)\}/)).toMatch(/min-height:\s*2lh\b/);
  });

  it("the budget period is a word and not a stem", async () => {
    const world = cockpitWorld();
    daemon.apiFetch.mockImplementation(cockpitFetch(world));

    await renderCockpit();

    expect(await screen.findByText(/of \$5\.00 per day/)).toBeDefined();
    expect(screen.queryByText(/per dai/)).toBeNull();
  });
});

/* ----------------------------------------- A17: the brake nobody reads -- */

describe("Autopilot - the trigger brakes say which of them the núcleo reads", () => {
  it("offers the team trigger brake as a live switch", async () => {
    const world = cockpitWorld({
      projects: [project({ project_id: "alpha" })],
      scopedKills: [{ scope_type: "trigger", scope_id: "scheduled", engaged: false }],
    });
    daemon.apiFetch.mockImplementation(cockpitFetch(world));

    await renderCockpit();

    await screen.findByRole("list", { name: "Trigger brakes" });
    const team = switchFor("Team triggers");

    // Read now, so a real state rather than the fixed "not read" label — and a
    // button, where before there was none.
    expect(within(team).getByText("released")).toBeDefined();
    expect(within(team).getByRole("button", { name: "Hold team triggers" })).toBeDefined();
    // The hedge said engaging this brake would stop nothing. It is no longer
    // true and must no longer be on screen.
    expect(within(team).queryByText(/nothing in it reads the value/)).toBeNull();
  });

  /**
   * A job with a team and a job without one ran the same list line, and they are
   * not the same thing: one is a queue in a single checkout, the other is a
   * checkout per item with several moving at once. The ceiling travels with the
   * name because the name alone does not say what having a team buys.
   */
  it("says which jobs in flight a team is directing, and how wide they may go", async () => {
    const world = cockpitWorld({
      projects: [project({ project_id: "alpha" })],
      jobs: [
        {
          id: 41,
          project_id: "alpha",
          rule_name: null,
          status: "implementing",
          wait_reason: null,
          max_items: 4,
          created_at: "2026-08-21T09:00:00Z",
          completed_at: null,
          slot: 0,
          round: 0,
          max_rounds: 3,
          team_id: "infra",
          team_name: "Infra",
          team_max_parallel: 3,
        },
      ],
    });
    daemon.apiFetch.mockImplementation(cockpitFetch(world));

    await renderCockpit();

    const list = await screen.findByRole("list", { name: "Jobs in flight" });
    expect(within(list).getByText(/Infra, up to 3 at once/)).toBeDefined();
  });

  it("holds and releases the team trigger scope", async () => {
    const world = cockpitWorld({ projects: [project({ project_id: "alpha" })] });
    daemon.apiFetch.mockImplementation(cockpitFetch(world));

    await renderCockpit();

    await screen.findByRole("list", { name: "Trigger brakes" });
    const team = switchFor("Team triggers");
    fireEvent.click(within(team).getByRole("button", { name: "Hold team triggers" }));

    await waitFor(() => {
      expect(daemon.apiFetch).toHaveBeenCalledWith("/autopilot/kill/scoped", {
        method: "POST",
        body: JSON.stringify({ scope_type: "trigger", scope_id: "team", engaged: true }),
      });
    });

    // An absent row means *not engaged*, so the switch had to be able to add one
    // rather than only patch one — same as the other three scopes.
    expect(await within(team).findByRole("button", { name: "Release team triggers" })).toBeDefined();

    fireEvent.click(within(team).getByRole("button", { name: "Release team triggers" }));

    await waitFor(() => {
      expect(daemon.apiFetch).toHaveBeenCalledWith("/autopilot/kill/scoped", {
        method: "POST",
        body: JSON.stringify({ scope_type: "trigger", scope_id: "team", engaged: false }),
      });
    });
  });

  it("engages a scope that is read, and sends the scope the núcleo checks", async () => {
    const world = cockpitWorld({ projects: [project({ project_id: "alpha" })] });
    daemon.apiFetch.mockImplementation(cockpitFetch(world));

    await renderCockpit();

    fireEvent.click(await screen.findByRole("button", { name: "Hold repo triggers" }));

    await waitFor(() => {
      expect(daemon.apiFetch).toHaveBeenCalledWith("/autopilot/kill/scoped", {
        method: "POST",
        body: JSON.stringify({ scope_type: "trigger", scope_id: "repo", engaged: true }),
      });
    });

    // An absent row means *not engaged*, so the switch had to be able to add one
    // rather than only patch one — the first engagement is the common case.
    expect(await screen.findByRole("button", { name: "Release repo triggers" })).toBeDefined();
  });
});

/* --------------------------------------------------------- the empty panels -- */

describe("Autopilot - the empty panels", () => {
  it("names the selected project in both empty sentences", async () => {
    const world = cockpitWorld({
      projects: [project({ project_id: "alpha", mode: "shadow" })],
      decisions: [],
      scoreboard: [],
    });
    daemon.apiFetch.mockImplementation(cockpitFetch(world));

    await renderCockpit();

    expect(await screen.findByText("nothing is waiting for a verdict on alpha.")).toBeDefined();
    expect(screen.getByText("alpha has recorded no classified decision yet.")).toBeDefined();
    expect(screen.queryByText(/on this project/)).toBeNull();
  });
});

/* --------------------------------------------------- the scoreboard's honesty -- */

describe("Autopilot - the scoreboard is read-only and says what it is not", () => {
  it("separates shadow evidence from enforced decisions and disclaims its own counts", async () => {
    const world = cockpitWorld({
      projects: [project({ project_id: "alpha", mode: "shadow", classes_ready: 1, classes_total: 2 })],
      decisions: [decision()],
      scoreboard: [
        tally({ mode: "shadow", action_class: "read-local" }),
        tally({ mode: "worktree", action_class: "vcs-local", reviewed: 0, agree: 0 }),
      ],
    });
    daemon.apiFetch.mockImplementation(cockpitFetch(world));

    await renderCockpit();

    expect(await screen.findByRole("table", { name: "Shadow evidence" })).toBeDefined();
    expect(screen.getByRole("table", { name: "Enforced decisions" })).toBeDefined();

    // The distinction that makes this panel honest rather than decorative: the
    // bar counts reviews distinct by tool and arguments, this table counts rows,
    // and the two are not the same number.
    expect(screen.getByText(/the count that decides it is not the count below/)).toBeDefined();
    // The authority stays the daemon's own figure.
    expect(screen.getByText("1/2 classes ready")).toBeDefined();
  });
});

/* -------------------------------------------------------- the stat cards -- */

describe("Autopilot - the stat cards name what they count", () => {
  it("the card names proposals", async () => {
    const world = cockpitWorld({
      projects: [
        project({ project_id: "alpha", mode: "active" }),
        project({ project_id: "beta", mode: "shadow" }),
      ],
      proposals: [1, 2, 3, 4, 5].map((id) => proposal({ id })),
    });
    daemon.apiFetch.mockImplementation(cockpitFetch(world));

    await renderCockpit();

    // "Waiting on you" is the one queue's phrase, and it belongs to Home and
    // the rail. This card counts open proposals across the roster, which is a
    // different number from the queue's six decision lists, so it says which.
    const card = await screen.findByRole("article", { name: "To review" });
    expect(within(card).getByText("5")).toBeDefined();
    expect(within(card).getByText("across the roster")).toBeDefined();
    expect(screen.queryByRole("article", { name: "Waiting on you" })).toBeNull();

    // What changed is the card no longer promising to BE the queue; the link
    // that actually goes there is untouched.
    expect(await screen.findByRole("link", { name: "Go to the queue" })).toBeDefined();
  });
});

/* ---------------------------------------------------------- the ask form -- */

describe("Autopilot - the ask form", () => {
  /**
   * One label rank above the field, not two.
   *
   * "ASK FOR ONE" and "WHAT SHOULD ALPHA DO?" were two 11px uppercase labels on consecutive
   * lines above one textarea, which makes the reader work out which of them names the box. The
   * field's label carries the question; the section had nothing of its own to add.
   */
  it("the ask form has one label above its field", async () => {
    daemon.apiFetch.mockImplementation(cockpitFetch(cockpitWorld()));

    await renderCockpit();

    expect(screen.queryByText("ask for one")).toBeNull();
    const field = screen.getByLabelText(/^What should .+ do\?$/);
    // Written as the attribute's own serialisation, not a bare string: the id
    // is also a kebab token from the `ap-` family, and a stray literal reads
    // to `css-contract.mjs` as a class nobody styled (the same shape of false
    // positive as `chats-zoom`/`fleet-exclusion`, one abbreviated attribute
    // reference away from being masked out).
    expect(field.outerHTML).toContain('id="ap-job-prompt"');
  });
});

/* --------------------------------------------------------------- the route -- */

describe("Autopilot - the route", () => {
  it("is registered, so the rail reaches the page and not the placeholder", async () => {
    daemon.apiFetch.mockImplementation(cockpitFetch(cockpitWorld()));

    // The whole app here, and only here: a stubbed destination would prove
    // nothing about whether `/autopilot` is in the real tree.
    const { router } = await renderApp({ initialPath: "/autopilot" });

    expect(await screen.findByRole("heading", { level: 1, name: "Autopilot" })).toBeDefined();
    expect(router.state.location.pathname).toBe("/autopilot");
    expect(screen.queryByText("Autopilot is not built yet")).toBeNull();
  });
});

describe("Autopilot - map-authored readings", () => {
  it("a trigger brake that is not engaged reads released, not running", async () => {
    const world = cockpitWorld({ projects: [project({ project_id: "alpha" })], scopedKills: [{ scope_type: "trigger", scope_id: "scheduled", engaged: false }] });
    daemon.apiFetch.mockImplementation(cockpitFetch(world));
    await renderCockpit();
    const team = switchFor("Team triggers");
    const reading = within(team).getByText("released");
    expect(reading.textContent).toBe("released");
    expect(reading.className).toContain("ui-badge-off");
    expect(within(team).queryByText("running")).toBeNull();
  });
});
