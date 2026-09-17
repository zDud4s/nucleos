import { describe, expect, it, vi } from "vitest";
import { fireEvent, screen, waitFor, within } from "@testing-library/react";

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
import type { ProjectSummary } from "../data/system";
import { statesOf } from "../ui/state-map";

async function openRoster(projects: ProjectSummary[], overrides: Partial<DaemonState> = {}) {
  const state = daemonState({ projects, ...overrides });
  daemon.apiFetch.mockImplementation(daemonFetch(state));
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

/**
 * The rows, top to bottom, by the project each header cell links to.
 *
 * The link and not the cell's text: the header also carries the mode badge now, so a row for a
 * project that is acting reads `alphaactive` as raw text.
 */
function order(): string[] {
  return screen
    .getAllByRole("row")
    .slice(1) // the header
    .map((row) => within(row).getAllByRole("rowheader")[0])
    .map((cell) => within(cell).getAllByRole("link")[0].textContent ?? "");
}

describe("the roster", () => {
  /**
   * **The complaint this page was rebuilt to answer.** Twenty-five equal pills, each repeating the
   * `shadow` that twenty-four of them share, with the two readings that actually differ mixed in
   * among them. The order is the fix: what needs somebody is at the top, and the sidebar keeps
   * answering "where is X" alphabetically for anybody who wants that instead.
   */
  it("puts what needs somebody at the top, whatever it is called", async () => {
    await openRoster([
      fine("ANSup"),
      fine("zeta", { open_review_items: 4, last_gate: "passed" }),
      fine("beta", { last_gate: "failed" }),
      fine("gamma", { root_exists: false }),
    ]);

    await screen.findByRole("table");
    expect(order()).toEqual(["gamma", "beta", "zeta", "ANSup"]);
  });

  /**
   * The card names proposals.
   *
   * `Waiting on you` over `open proposals across the roster` made the label a promise the
   * detail then took back. The bare phrase counts the six decision lists at `/waiting`; this
   * number is `open_review_items` summed over the rows, so the label says that and the detail is
   * left to say only where they are.
   */
  it("the card names proposals", async () => {
    await openRoster([
      fine("alpha", { open_review_items: 3 }),
      fine("beta", { open_review_items: 2 }),
    ]);
    await screen.findByRole("table");

    const card = screen.getByRole("article", { name: "To review" });
    expect(within(card).getByText("5")).toBeDefined();
    expect(within(card).getByText("across the roster")).toBeDefined();
    expect(screen.queryByRole("article", { name: "Waiting on you" })).toBeNull();
  });

  /** Zero keeps the sentence it already had: nothing has stopped to ask, not `across` nothing. */
  it("says nothing has stopped to ask when no proposal is open", async () => {
    await openRoster([fine("alpha"), fine("beta")]);
    await screen.findByRole("table");

    const card = screen.getByRole("article", { name: "To review" });
    expect(within(card).getByText("nothing has stopped to ask")).toBeDefined();
  });

  /**
   * **The rule the table wrote down for itself and applied to half a column.**
   *
   * *A column of identical badges is a column of noise* is why `Mode` drew the word `shadow` in
   * grey rather than as a badge. With twenty-four of twenty-five saying shadow, the whole column
   * was the noise — so the mode stops being a column and becomes a mark on the name, drawn only
   * when it departs from the default. `shadow` is not written anywhere: it is what the absence of a
   * mark means, and a page that says the default out loud twenty-four times has said nothing.
   */
  it("marks the mode on the name, and only when it is not the shadow everything else is", async () => {
    await openRoster([fine("quiet"), fine("acting", { mode: "active" })]);

    await screen.findByRole("table");
    expect(screen.queryByRole("columnheader", { name: "Mode" })).toBeNull();
    expect(screen.queryByText("shadow")).toBeNull();

    // Beside the name, inside the row's own header cell — a fact about the project, not a column.
    const activeRow = screen.getByRole("rowheader", { name: /acting/ });
    expect(within(activeRow).getByText("active").className).toContain("ui-badge");
  });

  /**
   * **Two columns left because they are already somewhere better.**
   *
   * The shadow-exit bar and the ceiling are both drawn in the project's own Settings panel, with
   * room for the sentence that makes them mean something. Here they were jargon in a narrow column,
   * blank three times out of four, and neither answers the question this page exists to answer.
   */
  it("keeps the four readings that decide whether a project needs somebody, and no others", async () => {
    await openRoster([fine("nucleos", { wip_limit: 2, classes_ready: 2, classes_total: 5 })]);

    await screen.findByRole("table");
    expect(screen.getAllByRole("columnheader").map((cell) => cell.textContent)).toEqual([
      "Project",
      "Waiting",
      "Gate",
      "Folder",
      // The way out has no heading worth reading; the column exists for the control in it.
      "Leaving",
    ]);
    expect(screen.queryByText("2/5")).toBeNull();
  });

  /**
   * **The headline used to contradict the rows it sat above.** It counted projects with a recorded
   * root and said "all with a folder"; the rows probed the folder and said `folder gone`. One
   * source answers both now, and the daemon is that source.
   */
  it("says the same thing about folders as its own rows do", async () => {
    await openRoster([fine("here"), fine("moved", { root_exists: false })]);

    expect(await screen.findByText(/1 with the folder gone/)).toBeTruthy();
    const movedRow = screen.getByRole("rowheader", { name: "moved" }).closest("tr") as HTMLElement;
    expect(within(movedRow).getByText("gone")).toBeTruthy();
  });

  /**
   * The three states §12 asks for, on the one reading this page owns. `not named` is a setting
   * nobody filled in and `gone` is a folder that moved — they send a person to two different
   * places, and the page that drew them the same way sent half of them to the wrong one.
   */
  it("distinguishes a folder that moved from one that was never named", async () => {
    await openRoster([
      fine("moved", { root_exists: false }),
      project({ project_id: "empty", mode: "shadow", project_root: null, root_exists: null }),
    ]);

    await screen.findByRole("table");
    const emptyRow = screen.getByRole("rowheader", { name: "empty" }).closest("tr") as HTMLElement;
    expect(within(emptyRow).getByText("not named")).toBeTruthy();
    expect(within(emptyRow).queryByText("gone")).toBeNull();
  });

  /**
   * **And a folder nobody named is only a fault while the project is meant to be doing something.**
   *
   * The same claim `rankOf` makes about the order, made here about the colour: a badge on a
   * dormant project would go on shouting exactly what the ordering stopped shouting. The words do
   * not change — `not named` either way, because it is still true — and a folder that has *gone*
   * stays a fault whatever the mode, since something moved a directory that was named.
   */
  it("does not colour a switched-off project's absent folder as a fault", async () => {
    await openRoster([
      project({ project_id: "asleep", mode: "off", project_root: null, root_exists: null }),
      project({ project_id: "unfinished", mode: "shadow", project_root: null, root_exists: null }),
      project({ project_id: "moved", mode: "off", project_root: "C:/x", root_exists: false }),
    ]);

    await screen.findByRole("table");
    const row = (name: string) =>
      within(screen.getByRole("rowheader", { name: new RegExp(name) }).closest("tr") as HTMLElement);

    expect(row("asleep").getByText("not named").className).not.toContain("ui-badge");
    expect(row("unfinished").getByText("not named").className).toContain("ui-badge");
    expect(row("moved").getByText("gone").className).toContain("ui-badge-danger");
  });

  it("keeps no folder reading nobody reads", async () => {
    expect(statesOf("folder").sort()).toEqual(["missing", "unset"]);

    await openRoster([fine("here")]);
    await screen.findByRole("table");
    const row = within(screen.getByRole("rowheader", { name: "here" }).closest("tr") as HTMLElement);
    const cell = row.getByText("ok");
    expect(cell.className).not.toContain("ui-badge");
    expect(cell.getAttribute("title")).toBe("C:/Projects/here");
  });

  /**
   * Into the workspace and not back into a file tree: a roster row is a project, and the question
   * somebody arrives at a project with is what Estado answers. This is the whole reason the
   * inspector moved off this page.
   */
  it("opens a project in its workspace", async () => {
    const { rendered } = await openRoster([fine("nucleos")]);

    /*
      Scoped to the table, because the SIDEBAR also lists this project by name while you are in the
      projects area — which is the arrangement the rail was changed to on the same day. An unscoped
      query here would be ambiguous, and the ambiguity is the two surfaces agreeing.
    */
    const table = await screen.findByRole("table");
    const link = within(table).getByRole("link", { name: "nucleos" });
    expect(link.getAttribute("href")).toContain("/projects/nucleos/state");
    expect(rendered.router.state.location.pathname).toBe("/projects");
  });

  /** The map already reads `job.gate_errored` as info; Roster was the last surface saying otherwise. */
  it("keeps a gate that could not run apart from one that said no", async () => {
    await openRoster([fine("broken", { last_gate: "failed" }), fine("unrun", { last_gate: "errored" })]);

    await screen.findByRole("table");
    const brokenRow = screen.getByRole("rowheader", { name: "broken" }).closest("tr") as HTMLElement;
    const unrunRow = screen.getByRole("rowheader", { name: "unrun" }).closest("tr") as HTMLElement;

    expect(within(brokenRow).getByText("failed").className).toContain("ui-badge-danger");
    expect(within(unrunRow).getByText("errored").className).toContain("ui-badge-info");
  });

  it("a gate that could not run is a fact, not a ceiling", async () => {
    await openRoster([fine("unrun", { last_gate: "errored" })]);
    await screen.findByRole("table");
    const cell = screen.getByText("errored");
    expect(cell.className).toContain("ui-badge-info");
    expect(cell.textContent).toBe("errored");
  });

  it("says so plainly when the núcleo knows of no project", async () => {
    await openRoster([]);

    expect(await screen.findByText(/no project has been registered/i)).toBeTruthy();
    expect(screen.queryByRole("table")).toBeNull();
  });
});

/** The remove control on one row, opened. */
async function openRemove(name: string) {
  const row = screen.getByRole("rowheader", { name }).closest("tr") as HTMLElement;
  fireEvent.click(within(row).getByRole("button", { name: "remove" }));
  return within(await screen.findByRole("group", { name: `Remove ${name} from NucleOS` }));
}

describe("a project leaving the roster", () => {
  /**
   * **One project could be added and none could leave.** So the roster only ever grew — a folder
   * somebody moved, a repository they finished with, a project added to try something once — and
   * every one of them stayed, polled every three seconds, on a page ordered by trouble.
   *
   * The reassurance about the folder is asserted, not just the request: `remove` on a page full of
   * paths reads as *delete that* until something says otherwise, and the sentence that says
   * otherwise has to arrive before the button does.
   */
  it("removes a project and leaves its folder alone", async () => {
    const { state } = await openRoster([fine("spent"), fine("kept")]);
    await screen.findByRole("table");

    const panel = await openRemove("spent");
    expect(panel.getByText(/stays exactly where it is/)).toBeTruthy();
    expect(panel.getByText("C:/Projects/spent")).toBeTruthy();

    fireEvent.click(panel.getByRole("button", { name: "remove" }));
    await waitFor(() => expect(state.removed.length).toBe(1));
    expect(state.removed[0]).toEqual({ projectId: "spent", forgetHistory: false });

    // And the page follows the roster rather than its own optimism: the row goes because the
    // removal invalidated the query, not because the component hid it.
    await waitFor(() => expect(screen.queryByRole("rowheader", { name: "spent" })).toBeNull());
    expect(screen.getByRole("rowheader", { name: "kept" })).toBeTruthy();
  });

  /**
   * **The history is kept unless somebody says otherwise, at the moment they say it.**
   *
   * And the number is beside the checkbox because that is what makes it a decision: "forget the
   * history too" over nothing asks a person to agree to lose an amount they cannot see.
   */
  it("offers the record beside the checkbox, and forgets only when it is ticked", async () => {
    const { state } = await openRoster([fine("spent")], {
      record: {
        forgets: { runs: 312, jobs: 0, proposals: 8, decisions: 0, stamps: 40, commands: 0, feed: 0 },
        holds: { slots: 0, worktrees: 0 },
      },
    });
    await screen.findByRole("table");

    const panel = await openRemove("spent");
    expect(await panel.findByText(/312 runs, 8 proposals and 40 stamps/)).toBeTruthy();

    fireEvent.click(panel.getByRole("checkbox"));
    fireEvent.click(panel.getByRole("button", { name: "remove and forget" }));
    await waitFor(() => expect(state.removed.length).toBe(1));
    expect(state.removed[0]).toEqual({ projectId: "spent", forgetHistory: true });
  });

  /** Nothing on record is nothing to decide about, so there is no checkbox to leave unticked. */
  it("offers no checkbox for a project that has nothing on record", async () => {
    await openRoster([fine("fresh")]);
    await screen.findByRole("table");

    const panel = await openRemove("fresh");
    expect(await panel.findByText(/has nothing on record/)).toBeTruthy();
    expect(panel.queryByRole("checkbox")).toBeNull();
  });

  /**
   * **Why the button is off, said before it is pressed.**
   *
   * The daemon refuses this too, and writes its own sentence — in the past tense, about a removal
   * that did not happen. Somebody should not have to press a button in order to be told they could
   * not, so the same fact is read off `/record` while the panel is open.
   */
  it("says what is holding a project rather than letting somebody press remove", async () => {
    const { state } = await openRoster([fine("busy")], {
      record: {
        forgets: { runs: 4, jobs: 0, proposals: 0, decisions: 0, stamps: 0, commands: 0, feed: 0 },
        holds: { slots: 1, worktrees: 2 },
      },
    });
    await screen.findByRole("table");

    const panel = await openRemove("busy");
    expect(await panel.findByText(/1 slot in flight and 2 worktrees checked out/)).toBeTruthy();
    expect(panel.getByRole("button", { name: "remove" })).toHaveProperty("disabled", true);

    fireEvent.click(panel.getByRole("button", { name: "remove" }));
    expect(state.removed).toEqual([]);
  });

  /**
   * A refusal leaves the panel where it is, with its own words in it.
   *
   * Closing on a refusal would draw the sentence explaining it and unmount it in the same tick,
   * which is the same as never having said anything.
   */
  it("keeps the panel open and says why when the núcleo refuses", async () => {
    await openRoster([fine("busy")], {
      removeRefusal: { status: 409, code: "in_flight", detail: "1 slot still working here" },
    });
    await screen.findByRole("table");

    const panel = await openRemove("busy");
    fireEvent.click(panel.getByRole("button", { name: "remove" }));

    expect(await panel.findByText(/work is still in flight here/)).toBeTruthy();
    expect(screen.getByRole("rowheader", { name: "busy" })).toBeTruthy();
  });

  /**
   * **Inline, and never a dialog.** `ConfirmButton` wrote the argument for the whole app: a modal
   * asking *are you sure?* trains people to click through it and takes the decision away from the
   * control that caused it. The workflows guard is the same shape, and this is asserted the same
   * way it is there.
   *
   * One open at a time for a sharper reason: two panels open would be two folder paths and two
   * counts on screen with a `remove` button each, which is how somebody removes the project they
   * were reading about rather than the one they meant.
   */
  it("opens inline rather than as a dialog, and only one at a time", async () => {
    const { rendered } = await openRoster([fine("one"), fine("two")]);
    await screen.findByRole("table");

    await openRemove("one");
    expect(rendered.container.querySelector("dialog")).toBeNull();

    await openRemove("two");
    expect(screen.queryByRole("group", { name: "Remove one from NucleOS" })).toBeNull();
    expect(screen.getByRole("group", { name: "Remove two from NucleOS" })).toBeTruthy();
  });
});

describe("Roster - map-authored readings", () => {
  it("the mode mark on the name comes from the map", async () => {
    await openRoster([fine("alpha", { mode: "active" })]);
    const row = await screen.findByRole("rowheader", { name: /alpha/ });
    const badge = within(row).getByText("active");
    expect(badge.textContent).toBe("active");
    expect(badge.className).toContain("ui-badge-active");
  });
});
