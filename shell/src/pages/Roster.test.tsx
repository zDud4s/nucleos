import { describe, expect, it, vi } from "vitest";
import { act, fireEvent, screen, within } from "@testing-library/react";

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
    .map((link) => link.querySelector("[data-name]")?.textContent ?? "");
}

/** The row for one project, inside whichever group holds it (the sidebar's link is not a row). */
function card(id: string): HTMLElement {
  const link = document.querySelector(
    ["Needs you", "Working", "Quiet"]
      .map((name) => `section[aria-label="${name}"] a[href$="/projects/${id}/state"]`)
      .join(", "),
  );
  if (link === null) throw new Error(`no row for ${id}`);
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

  it("orders the quiet group by mode and then by name, switched-off projects included whatever their folder says", async () => {
    await openRoster([
      fine("zulu"),
      project({ project_id: "asleep", mode: "off", project_root: null, root_exists: null }),
      fine("Mike"),
      project({ project_id: "moved", mode: "off", project_root: "C:/x", root_exists: false }),
    ]);

    await screen.findByRole("region", { name: "Quiet" });
    expect(names("Quiet")).toEqual(["Mike", "zulu", "asleep", "moved"]);
    expect(screen.queryByRole("region", { name: "Needs you" })).toBeNull();
    // One list on shared columns, not a sub-group per mode: the mode is each row's own column.
    expect(within(group("Quiet")).queryByRole("group")).toBeNull();
    const modes = within(group("Quiet"))
      .getAllByRole("link")
      .map((link) => link.querySelector("[data-mode]")?.textContent);
    expect(modes).toEqual(["shadow", "shadow", "off", "off"]);
    // A switched-off project's folder is said in the grey of switched off — never the red of a fault.
    const unnamed = within(card("asleep")).getByText("no folder named");
    expect(unnamed.className).toContain("text-tone-off-fg");
    const gone = within(card("moved")).getByText("folder gone");
    expect(gone.className).toContain("text-tone-off-fg");
    expect(gone.className).not.toContain("danger");
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
    expect(within(card("busy")).getByText("1 job and 2 runs")).toBeTruthy();
    expect(within(card("busy")).getByText("in flight")).toBeTruthy();
    expect(within(card("busy")).getByText("Open")).toBeTruthy();
    // A project that needs somebody says its work in flight too, beside the facts that put it there.
    expect(within(card("asking")).getByText("1 run")).toBeTruthy();
  });

  /** Each row says why it is there, in its own column, and what opening it is for. */
  it("gives every row its facts and the verb for the first thing to do", async () => {
    await openRoster([
      fine("asking", { open_review_items: 3, open_proposals: 2, open_shadow_decisions: 1 }),
      fine("broken", { last_gate: "failed", last_gate_at: "2026-09-20T14:05:00Z" }),
      fine("unrun", { last_gate: "errored" }),
      fine("moved", { root_exists: false }),
      project({ project_id: "empty", mode: "shadow", project_root: null, root_exists: null }),
    ]);
    await screen.findByRole("region", { name: "Needs you" });

    // The count, where the items are waiting, then the word a reader with no column head hears.
    expect(within(card("asking")).getByText("3")).toBeTruthy();
    expect(within(card("asking")).getByText("2 proposals and 1 shadow decision")).toBeTruthy();
    expect(within(card("asking")).getByText("to review")).toBeTruthy();
    expect(within(card("asking")).getByText("Open")).toBeTruthy();

    const failed = within(card("broken")).getByText("failed");
    expect(failed.getAttribute("data-tone")).toBe("danger");
    // The last gate run is its own column, said on the row and not only on hover.
    expect(within(card("broken")).getByText(/^20 Sept? 2026/)).toBeTruthy();
    expect(within(card("broken")).getByText("Open")).toBeTruthy();

    // Info and not danger: the gate never measured the code.
    const unrun = within(card("unrun")).getByText("could not run");
    expect(unrun.getAttribute("data-tone")).toBe("info");
    expect(screen.queryByText("errored")).toBeNull();

    const gone = within(card("moved")).getByText("folder gone");
    expect(gone.getAttribute("title")).toBe("C:/Projects/moved is not on this disk");
    expect(gone.className).toContain("danger");
    expect(within(card("moved")).getByText("C:/Projects/moved is not on this disk")).toBeTruthy();
    expect(within(card("moved")).getByText("Open")).toBeTruthy();

    // For a project that is meant to be running, a folder nobody named is unfinished: amber.
    const unset = within(card("empty")).getByText("no folder named");
    expect(unset.className).toContain("pending");
  });

  /** A folder that is fine is the path under the name; a gate that passed is a quiet word. */
  it("shows the path under the name, and a quiet word for a gate that passed", async () => {
    await openRoster([fine("calm", { last_gate: "passed" }), fine("bare")]);
    await screen.findByRole("region", { name: "Quiet" });

    expect(within(card("calm")).getByText("C:/Projects/calm")).toBeTruthy();
    expect(within(card("calm")).getByText("passed")).toBeTruthy();
    expect(within(card("bare")).queryByText("passed")).toBeNull();
  });

  it("says the bare total to review when the daemon gave no split", async () => {
    await openRoster([fine("asking", { open_review_items: 5 })]);
    await screen.findByRole("region", { name: "Needs you" });
    expect(within(card("asking")).getByText("5")).toBeTruthy();
    expect(within(card("asking")).getByText("to review")).toBeTruthy();
    // No split to give: nothing is claimed about proposals or shadow decisions.
    expect(within(card("asking")).queryByText(/proposal|shadow decision/)).toBeNull();
  });

  /** The mode word on the row is the map's, beside a dot in the rail's recipe. */
  it("marks every row with its mode, in the map's word", async () => {
    await openRoster([fine("acting", { mode: "active", open_review_items: 1 })]);
    await screen.findByRole("region", { name: "Needs you" });

    const mark = card("acting").querySelector('[data-mode="active"]');
    expect(mark?.textContent).toBe("active");
    expect(mark?.querySelector('span[aria-hidden="true"]')).not.toBeNull();
  });

  /**
   * Into the workspace and not back into a file tree: a project is opened on State, the mode that
   * answers the question somebody arrives with. The whole row is the one link.
   */
  it("opens a project on its State mode, from a row of any group", async () => {
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
   * same words the rows use for the same number.
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

  it("says the same thing about folders as its own rows do", async () => {
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
    expect(screen.getByText(/^as of \d\d:\d\d:\d\d: 2 projects · 2 items to review$/)).toBeDefined();
    expect((needs.parentElement as HTMLElement).className).toContain("text-text-muted");
    expect(names("Needs you")).toEqual(["alpha"]);
    expect(names("Quiet")).toEqual(["beta"]);
  });

  it("says so plainly when the núcleo knows of no project, with the way to add one", async () => {
    await openRoster([]);

    const heading = await screen.findByRole("heading", { name: /no project has been registered/i });
    expect(screen.queryByRole("region", { name: "Quiet" })).toBeNull();
    const teach = heading.parentElement as HTMLElement;
    expect(within(teach).getByRole("link", { name: "Add a project" }).getAttribute("href")).toBe(
      "/projects/new",
    );
  });

  it("says only the error, with a retry, when the roster never answered", async () => {
    const state = daemonState({ projects: [] });
    const base = daemonFetch(state);
    let answering = false;
    daemon.apiFetch.mockImplementation(async (path: string, init?: RequestInit) => {
      if (path === "/jobs?live=true" || path.startsWith("/runs?live=true")) return [];
      if (path === "/projects" && init?.method === undefined && !answering) throw new Error("down");
      return await base(path, init);
    });
    daemon.apiText.mockImplementation(daemonText(state));
    daemon.probeHealth.mockResolvedValue(true);
    await renderApp({ initialPath: "/projects" });

    const alert = await screen.findByText(/Could not load the projects/);
    expect(screen.queryByText(/Loading the projects/)).toBeNull();
    expect(screen.queryByRole("status", { name: /Loading the projects/ })).toBeNull();
    answering = true;
    await act(async () => {
      within(alert.closest("p") as HTMLElement).getByRole("button", { name: "Retry" }).click();
    });
    expect(await screen.findByRole("heading", { name: /no project has been registered/i })).toBeTruthy();
  });

  it("filters the groups by name and opens the only match on Enter", async () => {
    await openRoster([fine("alpha", { open_review_items: 1 }), fine("beta"), fine("alps")]);
    await screen.findByRole("region", { name: "Quiet" });
    const box = screen.getByRole("searchbox", { name: "Search projects" });

    await act(async () => {
      fireEvent.change(box, { target: { value: "AL" } });
    });
    expect(names("Needs you")).toEqual(["alpha"]);
    expect(names("Quiet")).toEqual(["alps"]);

    await act(async () => {
      fireEvent.change(box, { target: { value: "zzz" } });
    });
    expect(screen.getByText(/No project is named like "zzz"/)).toBeTruthy();

    await act(async () => {
      fireEvent.change(box, { target: { value: "bet" } });
    });
    await act(async () => {
      fireEvent.keyDown(box, { key: "Enter" });
    });
    // The workspace opened: its mode links replace the roster.
    expect(await screen.findByRole("link", { name: "Map" })).toBeTruthy();
  });

  /** A search that matches nothing says what to do next: clear it, or add the project. */
  it("offers the way out of a search that matches nothing", async () => {
    await openRoster([fine("alpha"), fine("beta")]);
    await screen.findByRole("region", { name: "Quiet" });
    const box = screen.getByRole("searchbox", { name: "Search projects" }) as HTMLInputElement;

    await act(async () => {
      fireEvent.change(box, { target: { value: "zzz" } });
    });
    expect(screen.getByRole("link", { name: "add a project" }).getAttribute("href")).toBe("/projects/new");
    await act(async () => {
      screen.getByRole("button", { name: "clear the search" }).click();
    });
    expect(box.value).toBe("");
    expect(names("Quiet")).toEqual(["alpha", "beta"]);
  });

  it("offers a new project from the header", async () => {
    await openRoster([fine("alpha")]);
    await screen.findByRole("region", { name: "Quiet" });
    expect(screen.getByRole("link", { name: "New project" }).getAttribute("href")).toBe(
      "/projects/new",
    );
  });
});
