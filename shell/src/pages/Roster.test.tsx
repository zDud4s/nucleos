import { describe, expect, it, vi } from "vitest";
import { act, screen, within } from "@testing-library/react";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
const daemon = vi.hoisted(() => ({ apiFetch: vi.fn(), apiText: vi.fn(), probeHealth: vi.fn() }));
vi.mock("../data/client", async (original) => ({
  ...(await original<typeof import("../data/client")>()),
  ...daemon,
}));

import {
  daemonFetch,
  daemonState,
  daemonText,
  project,
  renderApp,
  type DaemonState,
} from "../test/harness";
import { ApiRefusal } from "../data/client";
import { keys } from "../data/keys";
import type { ProjectSummary } from "../data/system";

interface Live {
  jobs?: { project_id: string }[];
  runs?: { project_id: string | null }[];
}

/**
 * The roster over a daemon holding these projects, with the live listings the harness does not
 * answer answered here — empty unless a test says otherwise, since "nothing in flight" is the
 * ordinary case and the one that sends a calm project to *Quiet*.
 */
async function openRoster(
  projects: ProjectSummary[],
  overrides: Partial<DaemonState> = {},
  live: Live = {},
) {
  const state = daemonState({ projects, ...overrides });
  const base = daemonFetch(state);
  daemon.apiFetch.mockImplementation(async (path: string, init?: RequestInit) => {
    if (path === "/jobs?live=true") return live.jobs ?? [];
    if (path.startsWith("/runs?live=true")) return live.runs ?? [];
    return await base(path, init);
  });
  daemon.apiText.mockImplementation(daemonText(state));
  daemon.probeHealth.mockResolvedValue(true);
  const rendered = await renderApp({ initialPath: "/projects" });
  return { state, rendered };
}

/** A project whose folder is where it says it is — the uninteresting case, to vary from. */
function fine(id: string, overrides: Partial<ProjectSummary> = {}): ProjectSummary {
  return project({
    project_id: id,
    mode: "shadow",
    project_root: `C:/Projects/${id}`,
    root_exists: true,
    ...overrides,
  });
}

/** One group, by the region name it answers to. */
function group(name: string): HTMLElement {
  return screen.getByRole("region", { name });
}

/**
 * The projects in a group, in the order it draws them, by the name each link carries.
 *
 * Scoped to the group because the SIDEBAR also lists every project by name while you are in the
 * projects area; an unscoped query would be ambiguous, and the ambiguity is the two agreeing.
 */
function names(name: string): string[] {
  return within(group(name))
    .getAllByRole("link")
    .map((link) => link.querySelector(".rs-card-name, .rs-quiet-name")?.textContent ?? "");
}

/** The card for one project, inside whichever group holds it. */
function card(id: string): HTMLElement {
  const link = document.querySelector(`a.rs-card[href$="/projects/${id}/state"]`);
  if (link === null) throw new Error(`no card for ${id}`);
  return link as HTMLElement;
}

describe("the roster", () => {
  /**
   * **The order the table had, made into a partition.** What needs somebody is a group of its own,
   * in attention order; the projects that ask nothing are a group of their own, by name, because a
   * quiet project is looked up rather than read about.
   */
  it("puts what needs somebody in its own group, in attention order", async () => {
    await openRoster([
      fine("ANSup"),
      fine("zeta", { open_review_items: 4, last_gate: "passed" }),
      fine("beta", { last_gate: "failed" }),
      fine("gamma", { root_exists: false }),
      fine("delta", { last_gate: "errored" }),
    ]);

    await screen.findByRole("region", { name: "Needs you" });
    expect(names("Needs you")).toEqual(["gamma", "beta", "zeta", "delta"]);
    expect(names("Quiet")).toEqual(["ANSup"]);
    // The count beside the heading, bare and mono, not a pill.
    const count = within(group("Needs you")).getByRole("heading").querySelector(".ui-count");
    expect(count?.textContent).toBe("4");
  });

  it("orders the quiet group by state and then by name, switched-off projects included whatever their folder says", async () => {
    await openRoster([
      fine("zulu"),
      project({ project_id: "asleep", mode: "off", project_root: null, root_exists: null }),
      fine("Mike"),
      project({ project_id: "moved", mode: "off", project_root: "C:/x", root_exists: false }),
    ]);

    await screen.findByRole("region", { name: "Quiet" });
    expect(names("Quiet")).toEqual(["Mike", "zulu", "asleep", "moved"]);
    expect(screen.queryByRole("region", { name: "Needs you" })).toBeNull();
    // Each state is its own labelled group, so "off" is said once over its projects.
    const off = within(group("Quiet")).getByRole("group", { name: "off" });
    expect(within(off).getAllByRole("link").map((link) => link.textContent)).toEqual(["asleep", "moved"]);
    const shadow = within(group("Quiet")).getByRole("group", { name: "shadow" });
    expect(within(shadow).getAllByRole("link").map((link) => link.textContent)).toEqual(["Mike", "zulu"]);
  });

  /** A group with nothing in it is not drawn — an empty heading is a sentence about nothing. */
  it("omits every group that has nothing in it", async () => {
    await openRoster([fine("alpha"), fine("beta")]);

    await screen.findByRole("region", { name: "Quiet" });
    expect(screen.queryByRole("region", { name: "Needs you" })).toBeNull();
    expect(screen.queryByRole("region", { name: "Working" })).toBeNull();
  });

  /**
   * **Working is the calm project with work in flight.** Nothing asked of anybody, so not *Needs
   * you*; but somebody looking for what is happening should not have to find it among the quiet.
   * Jobs and runs are named apart, because a job's runs are live runs too and one sum would count
   * the same work twice.
   */
  it("puts a calm project with live work under Working, and says what is in flight", async () => {
    await openRoster([fine("busy"), fine("idle"), fine("asking", { open_review_items: 1 })], {}, {
      jobs: [{ project_id: "busy" }],
      runs: [{ project_id: "busy" }, { project_id: "busy" }, { project_id: "asking" }],
    });

    await screen.findByRole("region", { name: "Working" });
    expect(names("Working")).toEqual(["busy"]);
    expect(names("Quiet")).toEqual(["idle"]);
    expect(names("Needs you")).toEqual(["asking"]);
    expect(within(card("busy")).getByText("1 job and 2 runs in flight")).toBeTruthy();
    expect(within(card("busy")).getByText("Open")).toBeTruthy();
    // A project that needs somebody says its work in flight too, after the reasons it is there.
    expect(within(card("asking")).getByText("1 run in flight")).toBeTruthy();
  });

  /** Each card says why it is there, and what opening it is for. */
  it("gives every card its reasons and the verb for the first thing to do", async () => {
    await openRoster([
      fine("asking", { open_review_items: 3, open_proposals: 2, open_shadow_decisions: 1 }),
      fine("broken", { last_gate: "failed", last_gate_at: "2026-09-20T14:05:00Z" }),
      fine("unrun", { last_gate: "errored" }),
      fine("moved", { root_exists: false }),
      project({ project_id: "empty", mode: "shadow", project_root: null, root_exists: null }),
    ]);
    await screen.findByRole("region", { name: "Needs you" });

    expect(within(card("asking")).getByText("2 proposals and 1 shadow decision to review")).toBeTruthy();
    expect(within(card("asking")).getByText("Review")).toBeTruthy();

    const failed = within(card("broken")).getByText("gate failed");
    expect(failed.getAttribute("title")).toMatch(/^last run 20 Sept? 2026/);
    expect(failed.querySelector('[data-mark="fault"]')).not.toBeNull();
    expect(within(card("broken")).getByText("See the gate")).toBeTruthy();

    // Info and not danger: the gate never measured the code.
    const unrun = within(card("unrun")).getByText("gate could not run");
    expect(unrun.querySelector('[data-mark="unmeasured"]')).not.toBeNull();
    expect(screen.queryByText("errored")).toBeNull();

    const gone = within(card("moved")).getByText("folder gone");
    expect(gone.getAttribute("title")).toBe("C:/Projects/moved is not on this disk");
    expect(within(card("moved")).getByText("Point at the folder")).toBeTruthy();

    const unset = within(card("empty")).getByText("no folder named");
    expect(unset.querySelector('[data-mark="unnamed"]')).not.toBeNull();
  });

  it("says the bare total to review when the daemon gave no split", async () => {
    await openRoster([fine("asking", { open_review_items: 5 })]);
    await screen.findByRole("region", { name: "Needs you" });
    expect(within(card("asking")).getByText("5 to review")).toBeTruthy();
  });

  /** The mode word on the card is the map's, beside a dot in the rail's recipe. */
  it("marks every card with its mode, in the map's word", async () => {
    await openRoster([fine("acting", { mode: "active", open_review_items: 1 })]);
    await screen.findByRole("region", { name: "Needs you" });

    const acting = card("acting");
    expect(acting.querySelector(".rs-card-mode")?.textContent).toBe("active");
    expect(acting.querySelector('.rs-dot[data-mode="active"]')).not.toBeNull();
  });

  /**
   * Into the workspace and not back into a file tree: a project is opened on State, the mode that
   * answers the question somebody arrives with. The whole card is the one link.
   */
  it("opens a project on its State mode, from a card or a quiet item", async () => {
    const { rendered } = await openRoster([fine("nucleos", { open_review_items: 1 }), fine("calm")]);
    await screen.findByRole("region", { name: "Needs you" });

    const links = within(group("Needs you")).getAllByRole("link");
    expect(links).toHaveLength(1);
    expect(links[0].getAttribute("href")).toBe("/projects/nucleos/state");
    expect(within(group("Quiet")).getByRole("link", { name: /calm/ }).getAttribute("href")).toBe(
      "/projects/calm/state",
    );
    expect(rendered.router.state.location.pathname).toBe("/projects");
  });

  /** Removal lives inside the project now; the roster offers nothing that acts on one. */
  it("offers no remove control", async () => {
    await openRoster([fine("alpha", { open_review_items: 1 }), fine("beta")]);
    await screen.findByRole("region", { name: "Needs you" });

    expect(screen.queryByRole("button", { name: /remove/ })).toBeNull();
    expect(screen.queryByRole("table")).toBeNull();
  });

  /**
   * **The headline says it once, and nothing says it again in a box.** Only non-zero facts, in the
   * same words the cards use for the same number.
   */
  it("says the review count in the headline, with no strip of cards repeating it", async () => {
    await openRoster([fine("alpha", { open_review_items: 3 }), fine("beta", { open_review_items: 2 })]);
    await screen.findByRole("region", { name: "Needs you" });

    expect(screen.getByText("2 projects · 5 items to review")).toBeDefined();
    expect(screen.queryByRole("article")).toBeNull();
  });

  it("says nothing about review when nothing is waiting", async () => {
    await openRoster([fine("alpha"), fine("beta")]);
    await screen.findByRole("region", { name: "Quiet" });

    expect(screen.getByText("2 projects")).toBeDefined();
    expect(screen.queryByText(/to review/)).toBeNull();
  });

  it("says the same thing about folders as its own cards do", async () => {
    await openRoster([fine("here"), fine("moved", { root_exists: false })]);

    expect(await screen.findByText(/1 with the folder gone/)).toBeTruthy();
    expect(within(card("moved")).getByText("folder gone")).toBeTruthy();
  });

  /** A stale roster says so first, and keeps every project it last knew — muted, not dropped. */
  it("leads with the stale note, dates the headline, and mutes the groups", async () => {
    const state = daemonState({ projects: [fine("alpha", { open_review_items: 2 }), fine("beta")] });
    let answering = true;
    const fetchFake = daemonFetch(state);
    daemon.apiFetch.mockImplementation(async (path: string, init?: RequestInit) => {
      if (!answering && path === "/projects" && init?.method === undefined) {
        throw new ApiRefusal(503, "unavailable", "");
      }
      if (path === "/jobs?live=true" || path.startsWith("/runs?live=true")) return [];
      return await fetchFake(path, init);
    });
    daemon.apiText.mockImplementation(daemonText(state));
    daemon.probeHealth.mockResolvedValue(true);
    const { queryClient } = await renderApp({ initialPath: "/projects" });
    await screen.findByRole("region", { name: "Needs you" });

    answering = false;
    await act(async () => {
      await queryClient.refetchQueries({ queryKey: keys.projects.all, exact: true });
    });

    const note = await screen.findByText(/view is stale — last good read/);
    const needs = group("Needs you");
    expect(note.compareDocumentPosition(needs) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
    expect(screen.getByText(/^as of \d\d:\d\d:\d\d — 2 projects · 2 items to review$/)).toBeDefined();
    expect((needs.parentElement as HTMLElement).className).toContain("text-text-muted");
    expect(names("Needs you")).toEqual(["alpha"]);
    expect(names("Quiet")).toEqual(["beta"]);
  });

  it("says so plainly when the núcleo knows of no project, with the way to add one", async () => {
    await openRoster([]);

    const heading = await screen.findByRole("heading", { name: /no project has been registered/i });
    expect(screen.queryByRole("region", { name: "Quiet" })).toBeNull();
    const teach = heading.parentElement as HTMLElement;
    expect(within(teach).getByRole("link", { name: "Add a project…" }).getAttribute("href")).toBe(
      "/projects/new",
    );
  });

  it("offers a new project from the header", async () => {
    await openRoster([fine("alpha")]);
    await screen.findByRole("region", { name: "Quiet" });
    expect(screen.getByRole("link", { name: "New project" }).getAttribute("href")).toBe(
      "/projects/new",
    );
  });
});
