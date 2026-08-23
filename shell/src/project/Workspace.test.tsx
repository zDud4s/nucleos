import { describe, expect, it, vi } from "vitest";
import { fireEvent, screen, waitFor } from "@testing-library/react";
import {
  daemonFetch,
  daemonState,
  daemonText,
  heldSlots,
  project,
  readings,
  renderApp,
  slot,
  renderWithQuery,
  type DaemonState,
} from "../test/harness";
import type { ProjectReadings } from "../data/project-readings";
import { ModeEstado } from "./ModeEstado";
import { normaliseMode } from "./Workspace";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
const daemon = vi.hoisted(() => ({ apiFetch: vi.fn(), apiText: vi.fn(), probeHealth: vi.fn() }));
vi.mock("../data/client", async (original) => ({
  ...(await original<typeof import("../data/client")>()),
  ...daemon,
}));

/** Mount the workspace over a daemon holding exactly these facts. */
async function openWorkspace(options: {
  openProposals?: number;
  engaged?: boolean;
  mode?: string;
  readings?: ProjectReadings;
} = {}) {
  const state = daemonState({
    kill: { engaged: options.engaged === true },
    projects: [
      project({ project_id: "nucleos", mode: "shadow", open_proposals: options.openProposals ?? 0 }),
    ],
    ...(options.readings === undefined ? {} : { readings: options.readings }),
  });
  daemon.apiFetch.mockImplementation(daemonFetch(state));
  daemon.apiText.mockImplementation(daemonText(state));
  daemon.probeHealth.mockResolvedValue(true);
  // The whole app, through the real router: a route that is missing from the
  // real tree is missing here too, so this proves the mode resolves as well as
  // what it renders.
  return renderApp({ initialPath: `/projects/nucleos/${options.mode ?? "estado"}` });
}

/** A project with one run holding a slot, and a dozen changed files under it. */
function reviewState(): DaemonState {
  return daemonState({
    projects: [project({ project_id: "nucleos", mode: "shadow", wip_limit: 2 })],
    concurrency: {
      house: { limit: 4, held: 1 },
      projects: [
        heldSlots("nucleos", 2, [slot({ project_id: "nucleos", owner_id: 41 })]),
      ],
    },
    changed: { paths: ["core/src/http.rs", "shell/src/a.tsx"], tracked: 3_214 },
  });
}

/** Mount the app over a state a test built for itself. */
async function openState(state: DaemonState, path = "/projects/nucleos/estado") {
  daemon.apiFetch.mockImplementation(daemonFetch(state));
  daemon.apiText.mockImplementation(daemonText(state));
  daemon.probeHealth.mockResolvedValue(true);
  return renderApp({ initialPath: path });
}

/** The page's panels, in order, read the way a screen reader would read them. */
function panelsOf(container: HTMLElement): string[] {
  // Scoped to the page rather than the window: the rail and the footer are not
  // this page's composition, and folding them in would make the assertion pass
  // or fail for reasons that have nothing to do with the workspace.
  return Array.from(container.querySelectorAll(".app-page section[aria-label]")).map(
    (section) => section.getAttribute("aria-label") ?? "",
  );
}

/** The same reading as {@link panelsOf}, for a mode mounted without the app frame. */
function panelsOf2(container: HTMLElement): string[] {
  return Array.from(container.querySelectorAll("section[aria-label]")).map(
    (section) => section.getAttribute("aria-label") ?? "",
  );
}

describe("normaliseMode", () => {
  it("takes the three modes as themselves", () => {
    expect(normaliseMode("estado")).toBe("estado");
    expect(normaliseMode("codigo")).toBe("codigo");
    expect(normaliseMode("workflows")).toBe("workflows");
  });

  /**
   * The rule the inspector this replaces already followed. A route parameter is
   * a string, anybody can type one, and a typo in a path is not a missing page.
   */
  it("lands on State for anything else, including nothing at all", () => {
    expect(normaliseMode("rules")).toBe("estado");
    expect(normaliseMode("")).toBe("estado");
    expect(normaliseMode(undefined)).toBe("estado");
  });
});

describe("the project workspace", () => {
  it("opens on State and offers the other two modes", async () => {
    await openWorkspace();

    expect(await screen.findByRole("link", { name: "State" })).toBeTruthy();
    expect(screen.getByRole("link", { name: "State" }).getAttribute("aria-current")).toBe("page");
    expect(screen.getByRole("link", { name: "Code" })).toBeTruthy();
    expect(screen.getByRole("link", { name: "Workflows" })).toBeTruthy();
  });

  /**
   * **The invariant the whole page is built to keep.**
   *
   * The design permits exactly one thing to change between a calm project and a
   * project demanding a decision: the weight of the top section. If a panel
   * could appear or vanish, the page would reflow under the eyes of somebody
   * halfway down it the moment a proposal landed — and the poll makes that a
   * regular event, not an edge case.
   *
   * Compared by `aria-label` rather than by class, because the structure being
   * defended is the one a screen reader walks as much as the one an eye scans.
   */
  it("renders the same panels, in the same order, calm and demanding", async () => {
    const calm = await openWorkspace({ openProposals: 0 });
    await screen.findByRole("link", { name: "State" });
    const calmPanels = panelsOf(calm.container);

    calm.unmount();

    const loud = await openWorkspace({ openProposals: 2 });
    await screen.findByRole("link", { name: "State" });
    const loudPanels = panelsOf(loud.container);

    expect(calmPanels).toEqual(loudPanels);
    // And it is a real page, not two empty ones agreeing with each other.
    expect(calmPanels).toEqual([
      "Leading",
      "Readings",
      "Occupancy",
      "Branches",
      "Workflow",
      "Commands",
      "Settings",
      "Files the app owns",
    ]);
  });

  it("says what is waiting when something is, and says nothing extra when nothing is", async () => {
    const loud = await openWorkspace({ openProposals: 2 });
    expect(await screen.findByText(/2 decisions waiting on you/)).toBeTruthy();
    loud.unmount();

    await openWorkspace({ openProposals: 0 });
    expect(await screen.findByText(/Nothing waiting on you in nucleos/)).toBeTruthy();
  });

  /**
   * The ladder, seen from the page rather than from the module. The kill switch
   * is machine-wide and outranks a project's own queue: telling somebody to go
   * and approve two things, while nothing can start, would send them to do work
   * that has no effect.
   */
  it("leads with the kill switch even when decisions are also waiting", async () => {
    await openWorkspace({ openProposals: 2, engaged: true });

    expect(await screen.findByText(/kill switch is engaged/i)).toBeTruthy();
    expect(screen.queryByText(/2 decisions waiting on you/)).toBeNull();
  });

  /**
   * §12, from the top of the page. A reading nobody has taken must not be able
   * to impersonate a reading that came back fine — so the empty readings say so
   * in words, and the calm sentence claims nothing about them.
   */
  it("never lets an unmeasured reading read as a good one", async () => {
    const { container } = await openWorkspace({ openProposals: 0 });
    // Waited for on purpose: before the roster answers, the top of the page says
    // it is still reading — which is a third state, and precisely the one that
    // must not be confused with calm.
    await screen.findByText(/Nothing waiting on you/);

    // Four readings, four em dashes, four reasons — and not one zero. Exactly four, which is
    // also what keeps the dash meaning ONE thing on this page: the ceiling below says "off"
    // rather than borrowing the mark for a reading nobody took. among them. The harness's
    // default project is the ordinary case this has to survive: brand new, nothing behind it.
    expect(screen.getAllByText("—").length).toBe(4);
    expect(screen.getByText("nothing finished in the last 30 days")).toBeTruthy();
    expect(screen.getByText("nothing started in the last 30 days")).toBeTruthy();
    expect(screen.getByText("nothing judged in the last 30 days")).toBeTruthy();
    expect(screen.getByText("nothing landed in the last 30 days")).toBeTruthy();

    /*
      The calm line is deliberately narrow. The design's own example sentence reads "gate green on
      the last 12" — and the readings that could support it are thirty-day tallies, so putting one
      in this sentence would turn a month's average into a claim about right now. The ban is on the
      sentence, not on the page: naming a reading in a panel that shows its window is honest.
    */
    const leading = container.querySelector('section[aria-label="Leading"]');
    expect(leading?.textContent).toBe("Nothing waiting on you in nucleos.");
  });

  /**
   * The distinction the ladder's `unknown` exists for. A project the shell has
   * not heard about yet is not a calm project: calm is a measurement, and
   * reporting one before it has been taken is the same error as a zero standing
   * in for an absent reading.
   */
  it("says it is still reading before the roster answers, rather than saying all is well", async () => {
    // The mode on its own, not the app: `renderApp` waits behind the connection
    // gate, so by the time it paints the roster has already answered and this
    // state is over. Mounting the mode directly is the only way to see it.
    daemon.apiFetch.mockImplementation(() => new Promise(() => {}));
    const { container } = renderWithQuery(<ModeEstado projectId="nucleos" answered={false} />);

    const leading = container.querySelector('section[aria-label="Leading"]');
    expect(leading?.textContent).toBe("Reading nucleos…");
    // And the rest of the page is already standing, so nothing moves when the
    // answer arrives — the same invariant, seen from the other end.
    expect(panelsOf2(container)).toEqual([
      "Leading",
      "Readings",
      "Occupancy",
      "Branches",
      "Workflow",
      "Commands",
      "Settings",
      "Files the app owns",
    ]);
  });

  it("shows the four readings when the núcleo has numbers for them", async () => {
    await openWorkspace({
      readings: readings({
        efficiency: {
          measured_runs: 41,
          unmeasured_runs: 7,
          median_total_tokens: 84_210,
          previous_median_total_tokens: 112_000,
        },
        cost: { usd: 128.4, runs: 48 },
        gate: { passed: 26, failed: 3, errored: 1, no_gate: 12 },
        delivered: { landed: 22, timed: 18, median_minutes: 74 },
      }),
    });

    expect(await screen.findByText("84k")).toBeTruthy();
    expect(screen.getByText("$ 128.40")).toBeTruthy();

    // 26 of 30 judged — the twelve ungated runs are NOT in the denominator, so this is 87% and not
    // 62%. Getting that wrong is the whole reason `gateShare` exists.
    expect(screen.getByText("87%")).toBeTruthy();
    expect(screen.getByText("of 30 judged")).toBeTruthy();

    // The three gate facts stay three sentences, and the ungated runs are stated rather than hidden.
    expect(screen.getByText(/3 failed · 1 could not run · 12 ungated/)).toBeTruthy();

    // The runs that reported nothing are said out loud beside the median they are not in.
    expect(screen.getByText(/41 measured, 7 reporting no usage/)).toBeTruthy();
    // Fewer tokens than before is an improvement, and the page says which way it went.
    expect(screen.getByText(/fewer tokens than the month before/)).toBeTruthy();

    // A median over 18 of 22 is shown as a median over 18 of 22.
    expect(screen.getByText(/1.2 h median, over 18 of 22/)).toBeTruthy();
  });

  it("draws the branches against the branch the root is actually on", async () => {
    const state = daemonState({
      projects: [project({ project_id: "nucleos", mode: "shadow" })],
      branches: {
        // Not `master`. A project whose trunk is called something else is exactly the case a panel
        // that assumed a name would get wrong, and landing reads this same value.
        integration: "trunk",
        branches: [
          {
            name: "trunk",
            ahead: 0,
            behind: 0,
            measured: true,
            last_commit_at: "2026-08-23T09:00:00Z",
            last_subject: "the trunk moved",
          },
          {
            name: "feat/one",
            ahead: 3,
            behind: 1,
            measured: true,
            last_commit_at: "2026-08-23T08:00:00Z",
            last_subject: "work in progress",
          },
          {
            name: "grafted",
            ahead: 0,
            behind: 0,
            measured: false,
            last_commit_at: "2026-08-20T08:00:00Z",
            last_subject: "unrelated",
          },
        ],
        omitted: 2,
      },
    });
    daemon.apiFetch.mockImplementation(daemonFetch(state));
    daemon.probeHealth.mockResolvedValue(true);
    await renderApp({ initialPath: "/projects/nucleos/estado" });

    expect(await screen.findByText("trunk")).toBeTruthy();
    // Where work lands is named as a place, not reported as being level with itself.
    expect(screen.getByText("where work lands")).toBeTruthy();

    // Diverged, and both numbers shown — this is the one that reads as "ahead" until somebody
    // tries to fast-forward it.
    expect(screen.getByText("diverged +3 −1")).toBeTruthy();

    // Unmeasured shows the word and no digits. A `0/0` here would claim it is identical to trunk.
    expect(screen.getByText("distance unknown")).toBeTruthy();

    // A ceiling that hid what it dropped would read as "these are all your branches".
    expect(screen.getByText("2 older branches not measured.")).toBeTruthy();
  });

  it("takes the run to review from the route, so a slot can link straight to it", async () => {
    const state = reviewState();
    daemon.apiFetch.mockImplementation(daemonFetch(state));
    daemon.apiText.mockImplementation(daemonText(state));
    daemon.probeHealth.mockResolvedValue(true);
    await renderApp({ initialPath: "/projects/nucleos/codigo?run=41" });

    expect(await screen.findByText("core/src/http.rs")).toBeTruthy();
    /*
      The number that makes this a review surface rather than a list of two things: 3,214 tracked
      minus the 2 this run touched.

      Asserted without its separators on purpose. The count goes through `toLocaleString`, which is
      right for a number a person reads and wrong to pin in a test — the grouping depends on the
      environment's locale, and this runner and the webview need not agree on it.
    */
    const untouched = screen.getByText(/files nobody touched/);
    expect(untouched.textContent?.replace(/\D/g, "")).toBe("3212");
  });

  /**
   * The rule that keeps a page like this from becoming a drawer: the command lives *in* the thing
   * it acts on. Reviewing this run is an action about this slot, not about the project.
   */
  it("puts the door to a review inside the slot the run is holding", async () => {
    const state = reviewState();
    daemon.apiFetch.mockImplementation(daemonFetch(state));
    daemon.apiText.mockImplementation(daemonText(state));
    daemon.probeHealth.mockResolvedValue(true);
    await renderApp({ initialPath: "/projects/nucleos/estado" });

    const link = await screen.findByRole("link", { name: "Review run 41" });
    expect(link.getAttribute("href")).toContain("/projects/nucleos/codigo");
    expect(link.getAttribute("href")).toContain("run=41");
  });

  /**
   * A worktree with no recorded branch point cannot be measured, and that is not the same as a run
   * that changed nothing. Saying "nothing changed" for it would be the most reassuring possible way
   * to be wrong.
   */
  it("says a worktree cannot be measured rather than saying nothing changed", async () => {
    const state = reviewState();
    state.changed = null;
    daemon.apiFetch.mockImplementation(daemonFetch(state));
    daemon.apiText.mockImplementation(daemonText(state));
    daemon.probeHealth.mockResolvedValue(true);
    await renderApp({ initialPath: "/projects/nucleos/codigo?run=41" });

    expect(await screen.findByText(/no recorded branch point/)).toBeTruthy();
    expect(screen.queryByText(/changed nothing/)).toBeNull();
  });

  it("offers no review when nothing holds a worktree here", async () => {
    await openWorkspace({ mode: "codigo" });
    expect(await screen.findByText(/Nothing to review in nucleos/)).toBeTruthy();
  });

  it("shows the workflows mode as designed and not yet served", async () => {
    await openWorkspace({ mode: "workflows" });
    expect(await screen.findByText(/No workflow is installed in nucleos/)).toBeTruthy();
  });
});

/* ------------------------------------------------------- the write boundary -- */

describe("what the app may author", () => {
  /** A project with a rules file already in it. */
  function ownedState(overrides: Partial<DaemonState> = {}): DaemonState {
    return daemonState({
      projects: [project({ project_id: "nucleos", mode: "shadow", project_root: "C:/p" })],
      text: { diff: "", cat: "gate_command: cargo test\n" },
      ...overrides,
    });
  }

  /**
   * The fence is the daemon's, and the page draws what it is told.
   *
   * A client carrying its own copy would offer an editor for a file the daemon refuses, or hide
   * one for a file it would accept — and neither mistake announces itself. So the empty answer has
   * to produce no editor at all, which is what proves the list is doing the deciding.
   */
  it("offers an editor for exactly the files the núcleo says it authors", async () => {
    const nothing = await openState(ownedState({ ownership: [] }));
    expect(await screen.findByText(/authors no file in this project/)).toBeTruthy();
    expect(screen.queryByRole("button", { name: "edit" })).toBeNull();

    nothing.unmount();

    await openState(ownedState());
    expect(await screen.findByText(".ai/autopilot.yaml")).toBeTruthy();
    expect(screen.getByRole("button", { name: "edit" })).toBeTruthy();
  });

  /**
   * What is SENT, not what the component believed it sent. The path comes from the claim rather
   * than from anything typed here, which is the same rule the daemon follows on its side.
   */
  it("sends the text that was typed, to the path the núcleo named", async () => {
    const state = ownedState();
    await openState(state);

    fireEvent.click(await screen.findByRole("button", { name: "edit" }));
    const box = await screen.findByLabelText(".ai/autopilot.yaml");
    fireEvent.change(box, { target: { value: "gate_command: cargo clippy\n" } });
    fireEvent.click(screen.getByRole("button", { name: "save" }));

    await waitFor(() => expect(state.writes.length).toBe(1));
    expect(state.writes[0]).toEqual({
      path: ".ai/autopilot.yaml",
      contents: "gate_command: cargo clippy\n",
    });
  });

  /**
   * A refused save keeps the text in front of the person who wrote it, and says where the file
   * broke.
   *
   * Both halves matter and they fail differently. Dropping the draft makes a rejected edit an edit
   * LOST — the worst possible answer to "that YAML is invalid". And showing only the word "invalid"
   * sends somebody to a text editor to find the broken line, which is the surface this editor
   * exists to replace; the daemon already located it, so the sentence is free.
   */
  it("keeps a refused edit on the screen and says where the file broke", async () => {
    const state = ownedState({
      writeRefusal: {
        status: 422,
        code: "invalid",
        detail: "unknown field `gate_commmand` at line 1 column 1",
      },
    });
    await openState(state);

    fireEvent.click(await screen.findByRole("button", { name: "edit" }));
    const box = await screen.findByLabelText(".ai/autopilot.yaml");
    fireEvent.change(box, { target: { value: "gate_commmand: cargo test\n" } });
    fireEvent.click(screen.getByRole("button", { name: "save" }));

    expect(await screen.findByText(/gate_commmand/)).toBeTruthy();
    expect((box as HTMLTextAreaElement).value).toBe("gate_commmand: cargo test\n");
  });

  /**
   * The emergency stop holds this write too, and the sentence says the one thing that keeps that
   * from being a trap: the file is still ordinary text in an editor one click away.
   */
  it("says the stop is why a save was refused, and where the file is still editable", async () => {
    const state = ownedState({
      writeRefusal: { status: 423, code: "kill_switch", detail: "kill_switch" },
    });
    await openState(state);

    fireEvent.click(await screen.findByRole("button", { name: "edit" }));
    fireEvent.change(await screen.findByLabelText(".ai/autopilot.yaml"), {
      target: { value: "gate_command: x\n" },
    });
    fireEvent.click(screen.getByRole("button", { name: "save" }));

    expect(await screen.findByText(/emergency stop is engaged/)).toBeTruthy();
    expect(screen.getByText(/still editable in an editor/)).toBeTruthy();
  });

  /**
   * A file that is not there yet is a state, not an error. A project nobody has scheduled anything
   * in has no rules file, which is the ordinary case — and saving is how it gets one.
   */
  it("says a rules file is absent rather than showing an empty one", async () => {
    await openState(ownedState({ text: { diff: "", cat: null } }));

    fireEvent.click(await screen.findByRole("button", { name: "edit" }));
    expect(await screen.findByText(/no such file yet/)).toBeTruthy();
    expect((await screen.findByLabelText(".ai/autopilot.yaml") as HTMLTextAreaElement).value).toBe("");
  });
});

describe("the settings this app authors", () => {
  function settingsState(overrides: Partial<ReturnType<typeof project>> = {}): DaemonState {
    return daemonState({
      projects: [
        project({ project_id: "nucleos", mode: "shadow", project_root: "C:/p", ...overrides }),
      ],
    });
  }

  /**
   * The ceiling says what it is holding, because a ceiling of three reads as slack until you know
   * two are already taken.
   *
   * And clearing it is not setting it to zero: the daemon compares `open >= limit`, so zero means
   * *never start anything again* — the opposite end of the same axis from "no brake".
   */
  it("says what the ceiling is holding, and clears it without setting it to zero", async () => {
    const state = settingsState({ wip_limit: 3, open_proposals: 2 });
    await openState(state);

    expect(await screen.findByText("2 of 3 taken")).toBeTruthy();

    fireEvent.click(screen.getByRole("button", { name: "no ceiling" }));
    await waitFor(() =>
      expect(state.projects[0].wip_limit).toBe(null),
    );
    expect(await screen.findByText(/no ceiling — 2 proposals waiting on you/)).toBeTruthy();
  });

  /**
   * The third setting is earned, and the page says what is still missing rather than offering a
   * button that would refuse.
   *
   * `promotable` is the núcleo's arithmetic and is never recomputed here — the blocker sentence is
   * the same one the Autopilot page shows, from `lib/mode.ts`, so the two cannot come to disagree
   * about what restraint means.
   */
  it("does not offer to let a project act until the núcleo says it has earned it", async () => {
    const locked = await openState(
      settingsState({ promotable: false, classes_ready: 2, classes_total: 5 }),
    );
    const active = await screen.findByRole("button", { name: "active" });
    expect(active.hasAttribute("disabled")).toBe(true);
    expect(screen.getByText(/3 of 5 action classes are still short of the bar/)).toBeTruthy();

    locked.unmount();

    await openState(
      settingsState({
        promotable: true,
        classes_ready: 5,
        classes_total: 5,
        withheld_classes_ready: 1,
      }),
    );
    expect((await screen.findByRole("button", { name: "active" })).hasAttribute("disabled")).toBe(
      false,
    );
  });

  /**
   * Nothing measured is not zero cleared. A project that has never run in shadow has taken no
   * measurement, and the difference is the whole of the never-collapse contract.
   */
  it("says no class was measured rather than saying none cleared", async () => {
    await openState(settingsState());
    expect(await screen.findByText(/Nothing recorded in shadow yet/)).toBeTruthy();
    expect(screen.getByText(/not the same as a bad one/)).toBeTruthy();
  });
});
