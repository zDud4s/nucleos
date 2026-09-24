import { describe, expect, it, vi } from "vitest";
import { act, fireEvent, screen, waitFor, within } from "@testing-library/react";

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
   * **The headline says it once, and nothing says it again in a box.**
   *
   * A strip of four stat cards used to sit under the headline and repeat it figure for figure —
   * projects, acting, failing, to review — so the largest mass on the page was a repetition, and
   * "4 on the roster" weighed what "1 failing the gate" weighed. The column carries the headline's
   * own words for the same number: `To review`, not `Waiting`, which the rail uses for another
   * count.
   */
  it("says the review count in the headline's words, with no strip of cards repeating it", async () => {
    await openRoster([
      fine("alpha", { open_review_items: 3 }),
      fine("beta", { open_review_items: 2 }),
    ]);
    await screen.findByRole("table");

    expect(screen.getByText("2 projects · 5 items to review")).toBeDefined();
    expect(screen.queryByRole("article")).toBeNull();
    expect(screen.getByRole("columnheader", { name: "To review" })).toBeDefined();
    expect(screen.queryByRole("columnheader", { name: "Waiting" })).toBeNull();
  });

  /** The normal recedes: nothing waiting is not said at all, rather than said as a zero. */
  it("says nothing about review when nothing is waiting", async () => {
    await openRoster([fine("alpha"), fine("beta")]);
    await screen.findByRole("table");

    expect(screen.getByText("2 projects")).toBeDefined();
    expect(screen.queryByText(/to review/)).toBeNull();
  });

  /**
   * **A stale roster says so first, and offers nothing that would act on it.**
   *
   * The note used to arrive under the stat strip in the faintest register the system has, so the
   * figures were read at full weight before the sentence saying they were old. And `remove` stayed
   * live on every row — an action on a roster nobody can vouch for, which DESIGN.md says is removed,
   * not disabled. An open panel closes rather than hiding, so it cannot come back by itself.
   */
  it("leads with the stale note, dates the headline, and takes the remove controls away", async () => {
    const state = daemonState({ projects: [fine("alpha", { open_review_items: 2 }), fine("beta")] });
    let answering = true;
    const fetchFake = daemonFetch(state);
    daemon.apiFetch.mockImplementation(async (path: string, init?: RequestInit) => {
      if (!answering && path === "/projects" && init?.method === undefined) {
        throw new ApiRefusal(503, "unavailable", "");
      }
      return await fetchFake(path, init);
    });
    daemon.apiText.mockImplementation(daemonText(state));
    daemon.probeHealth.mockResolvedValue(true);
    const { queryClient } = await renderApp({ initialPath: "/projects" });
    await screen.findByRole("table");

    await openRemove("alpha");
    answering = false;
    await act(async () => {
      await queryClient.refetchQueries({ queryKey: keys.projects.all, exact: true });
    });

    const note = await screen.findByText(/view is stale — last good read/);
    // Before the table in the document, which is before it in reading order.
    const table = screen.getByRole("table");
    expect(note.compareDocumentPosition(table) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
    expect(screen.getByText(/^as of \d\d:\d\d:\d\d — 2 projects · 2 items to review$/)).toBeDefined();

    expect(within(table).queryByRole("button", { name: "remove" })).toBeNull();
    expect(screen.queryByRole("group", { name: "Remove alpha from NucleOS" })).toBeNull();
    // The rows are still here: stale is the last good read, not an empty roster.
    expect(within(table).getByRole("rowheader", { name: "alpha" })).toBeDefined();
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
      "To review",
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

    expect(within(brokenRow).getByText("gate failed").className).toContain("ui-badge-danger");
    expect(within(unrunRow).getByText("gate not measured").className).toContain("ui-badge-info");
  });

  /**
   * The map's words and not the wire's. `errored` is exactly the word that reads as a failure, and
   * the whole point of drawing it blue is that the code was never measured.
   */
  it("names a gate that could not run in the map's words, not the wire's", async () => {
    await openRoster([fine("unrun", { last_gate: "errored", last_gate_at: "2026-09-20T14:05:00Z" })]);
    await screen.findByRole("table");
    const cell = screen.getByText("gate not measured");
    expect(cell.className).toContain("ui-badge-info");
    expect(screen.queryByText("errored")).toBeNull();
    // And the time in the tooltip is formatted, not the daemon's raw stamp.
    expect(cell.getAttribute("title")).toMatch(/^last run 20 Sept? 2026/);
  });

  it("says so plainly when the núcleo knows of no project, with the way to add one", async () => {
    await openRoster([]);

    const heading = await screen.findByRole("heading", { name: /no project has been registered/i });
    expect(screen.queryByRole("table")).toBeNull();
    const teach = heading.parentElement as HTMLElement;
    expect(within(teach).getByRole("link", { name: "Add a project…" }).getAttribute("href")).toBe(
      "/projects/new",
    );
  });
});

/** Past `ConfirmButton`'s dwell, so the second press is a decision and not a double-click. */
async function pastTheDwell(): Promise<void> {
  await act(async () => {
    await new Promise((resolve) => setTimeout(resolve, 350));
  });
}

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
  it("offers the record beside the checkbox, and forgets only when it is ticked and confirmed", async () => {
    const { state } = await openRoster([fine("spent")], {
      record: {
        forgets: { runs: 312, jobs: 0, proposals: 8, decisions: 0, stamps: 40, commands: 0, feed: 0 },
        holds: { slots: 0, worktrees: 0 },
      },
    });
    await screen.findByRole("table");

    const panel = await openRemove("spent");
    expect(await panel.findByText(/312 runs, 8 proposals and 40 stamps on record/)).toBeTruthy();
    expect(panel.getByText(/Nothing on disk is touched/)).toBeTruthy();

    fireEvent.click(panel.getByRole("checkbox"));
    /*
      The reassurance stops promising what is no longer true. The runs live in the núcleo's own
      database, which is on a disk, and "nothing on disk is touched" above the one irreversible act
      on this page was the sentence the critique caught.
    */
    expect(panel.queryByText(/Nothing on disk is touched/)).toBeNull();
    expect(panel.getByText(/will be deleted from the núcleo and cannot be brought back/)).toBeTruthy();

    // One press arms; nothing has been sent yet.
    fireEvent.click(panel.getByRole("button", { name: "remove and forget" }));
    expect(state.removed).toEqual([]);

    await pastTheDwell();
    fireEvent.click(
      panel.getByRole("button", { name: "delete 312 runs, 8 proposals and 40 stamps for good · spent" }),
    );
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

describe("the way out of the remove panel", () => {
  /**
   * **The panel's one exit, and where focus goes after it.** A panel that unmounts with focus inside
   * drops focus to the body, and somebody on a keyboard loses their place in the table — the house
   * standard is that focus goes back to the control that opened it.
   */
  it("hands focus back to the toggle on cancel", async () => {
    await openRoster([fine("one")]);
    await screen.findByRole("table");

    const panel = await openRemove("one");
    fireEvent.click(panel.getByRole("button", { name: "cancel" }));

    expect(screen.queryByRole("group", { name: "Remove one from NucleOS" })).toBeNull();
    const row = screen.getByRole("rowheader", { name: "one" }).closest("tr") as HTMLElement;
    expect(document.activeElement).toBe(within(row).getByRole("button", { name: "remove" }));
  });

  it("closes on Escape and hands focus back the same way", async () => {
    await openRoster([fine("one")]);
    await screen.findByRole("table");

    const panel = await openRemove("one");
    fireEvent.keyDown(panel.getByRole("button", { name: "cancel" }), { key: "Escape" });

    expect(screen.queryByRole("group", { name: "Remove one from NucleOS" })).toBeNull();
    const row = screen.getByRole("rowheader", { name: "one" }).closest("tr") as HTMLElement;
    expect(document.activeElement).toBe(within(row).getByRole("button", { name: "remove" }));
  });

  /**
   * The toggle says what pressing it does now. Left saying `remove` while open, the page had two
   * `remove` buttons with opposite effects — one closed the panel, the other removed the project.
   * Open, it reads `cancel`, the panel's own exit word, rather than `keep`, a second one.
   */
  it("names the open toggle for what it does now, and points it at the panel it opened", async () => {
    await openRoster([fine("one")]);
    await screen.findByRole("table");

    const row = screen.getByRole("rowheader", { name: "one" }).closest("tr") as HTMLElement;
    await openRemove("one");
    expect(within(row).queryByRole("button", { name: "remove" })).toBeNull();
    expect(within(row).queryByRole("button", { name: "keep" })).toBeNull();
    const toggle = within(row).getByRole("button", { name: "cancel" });
    expect(toggle.getAttribute("aria-expanded")).toBe("true");
    const panel = screen.getByRole("group", { name: "Remove one from NucleOS" });
    expect(toggle.getAttribute("aria-controls")).toBe(panel.id);
    // Exactly one `remove` on screen, and it is the one that removes.
    expect(screen.getAllByRole("button", { name: "remove" })).toEqual([
      within(panel).getByRole("button", { name: "remove" }),
    ]);

    fireEvent.click(toggle);
    expect(screen.queryByRole("group", { name: "Remove one from NucleOS" })).toBeNull();
    expect(within(row).getByRole("button", { name: "remove" }).getAttribute("aria-expanded")).toBe("false");
  });

  /**
   * The app tried and could not: the error rung, announced, next to the button. It was faint 12px
   * text with no role — the one failure on the page that read quieter than a column label.
   */
  it("says a dropped connection as an error, and keeps the panel open", async () => {
    const state = daemonState({ projects: [fine("spent")] });
    const fetchFake = daemonFetch(state);
    daemon.apiFetch.mockImplementation(async (path: string, init?: RequestInit) => {
      if (init?.method === "DELETE") throw new TypeError("Failed to fetch");
      return await fetchFake(path, init);
    });
    daemon.apiText.mockImplementation(daemonText(state));
    daemon.probeHealth.mockResolvedValue(true);
    await renderApp({ initialPath: "/projects" });
    await screen.findByRole("table");

    const panel = await openRemove("spent");
    fireEvent.click(panel.getByRole("button", { name: "remove" }));

    const alert = await panel.findByRole("alert");
    expect(alert.textContent).toBe("the núcleo did not answer — nothing was removed");
    expect(screen.getByRole("rowheader", { name: "spent" })).toBeTruthy();
  });

  /**
   * **A removal is answered where the row was.** It used to end in silence — the panel shut, the
   * row went on the next poll and focus fell to the body. Now one quiet line says what left and
   * that nothing else moved, carries the way back, and takes focus.
   */
  it("says the project left, keeps the way back, and puts focus on the line", async () => {
    await openRoster([fine("spent"), fine("kept")]);
    await screen.findByRole("table");

    const panel = await openRemove("spent");
    fireEvent.click(panel.getByRole("button", { name: "remove" }));

    const line = await screen.findByText(
      "spent left the roster — its folder is still at C:/Projects/spent, and its history is kept.",
    );
    const quiet = line.closest(".ui-quiet") as HTMLElement;
    expect(quiet.getAttribute("role")).toBe("status");
    expect(within(quiet).getByRole("link", { name: "add it back" }).getAttribute("href")).toBe(
      "/projects/new",
    );
    await waitFor(() => expect(document.activeElement?.contains(quiet)).toBe(true));
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
