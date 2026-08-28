// §spec workspace-de-projeto
import { describe, expect, it, vi } from "vitest";
import { fireEvent, screen, waitFor } from "@testing-library/react";
import {
  daemonFetch,
  daemonState,
  daemonText,
  heldSlots,
  project,
  installedWorkflow,
  projectCommand,
  readings,
  renderApp,
  slot,
  renderWithQuery,
  type DaemonState,
} from "../test/harness";
import { ApiRefusal } from "../data/client";
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
  it("takes the four modes as themselves", () => {
    expect(normaliseMode("estado")).toBe("estado");
    expect(normaliseMode("mapa")).toBe("mapa");
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
  it("opens on State and offers the other three modes", async () => {
    await openWorkspace();

    expect(await screen.findByRole("link", { name: "State" })).toBeTruthy();
    expect(screen.getByRole("link", { name: "State" }).getAttribute("aria-current")).toBe("page");
    expect(screen.getByRole("link", { name: "Map" })).toBeTruthy();
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

  /**
   * The third mode resolves and reads the library, which is what it did not do while it was a
   * placeholder. The route is the assertion: `normaliseMode` above proves the parameter maps, and
   * this proves the thing it maps to is mounted and asking the daemon.
   */
  it("mounts the workflows mode on the library and the project's pins", async () => {
    await openWorkspace({ mode: "workflows" });
    expect(await screen.findByText(/No workflow is installed here/)).toBeTruthy();
    expect(screen.getByText("On this machine")).toBeTruthy();
  });

  /**
   * The tab test above proves the "Map" link exists. It would not notice a typo in the render
   * condition — `mode === "maps"` would leave that same link sitting there, clickable, over an
   * empty page. This proves the thing the link points to is actually mounted and asking the
   * daemon for the project's structure.
   */
  it("mounts the map mode and reads the project's structure", async () => {
    const state = daemonState({
      projects: [project({ project_id: "nucleos", mode: "shadow" })],
    });
    daemon.apiFetch.mockImplementation(async (path: string, init?: RequestInit) => {
      if (path.endsWith("/map")) {
        // The whole answer `GET /map` gives, `junction` included. A mock that stopped at the
        // structure would be a payload the daemon cannot produce, and the mode reads both halves
        // off this one response.
        return {
          modules: [
            { path: "core/src/a.rs", reader: "rust", declares: false, cites: [], tested: false },
          ],
          imports: [],
          unread: [],
          foreign: [],
          junction: {
            decisions: [],
            unclaimed: ["core/src/a.rs"],
            unmatched: [],
            counts: {
              decisions: 0,
              declared: 0,
              ambiguous: 0,
              silent: 0,
              unnumbered: 0,
              unclaimed: 1,
              unmatched: 0,
            },
          },
          // The verdict half, which arrives on this same answer. Left out, every count the stamp
          // panel reads would be `undefined` and the mode would go down with it — which is what
          // `junction` did one slice ago, on this exact mock.
          standings: {},
          stamps: {
            settled: 0,
            partial: 0,
            never: 0,
            lapsed: 0,
            withdrawn: 0,
            guessed: 0,
            no_anchor: 0,
            untracked: 0,
            no_repository: 0,
            unwatched: 0,
            decisions: 0,
          },
          // The triager's axis, which arrives on this same answer too. §5.3's `K` and `J` are read
          // off `triage_counts` and not off `stamps` since slice 5 — a mock that stopped at the
          // stamps would leave the header printing `undefined`, which is what `junction` and then
          // `standings` each did in turn on this exact mock.
          triage: {},
          triage_counts: {
            flagged: 0,
            silenced: 0,
            untriaged: 0,
            unseen: 0,
            waiting: 0,
            unchecked: 0,
          },
          git_would_not_answer: false,
          recency: { window: 200, ages: {} },
          // `null` is *the triager has never run here*, which the panel now reads rather than
          // infers from two empty piles — an inference that called a project triaged this morning
          // un-triaged the moment its answers went stale.
          last_triaged_at: null,
        };
      }
      // The pile is capped by the daemon, so it answers a page and the size it was cut from.
      if (path.endsWith("/map/silenced")) return { rows: [], total: 0 };
      return daemonFetch(state)(path, init);
    });
    daemon.apiText.mockImplementation(daemonText(state));
    daemon.probeHealth.mockResolvedValue(true);
    await renderApp({ initialPath: "/projects/nucleos/mapa" });

    // Was `/declaring nothing they implement/`, which this mode no longer says: that count is the
    // junction's `unclaimed` now, and the structure panel reports what it alone knows.
    expect(await screen.findByText(/this reader could read/)).toBeTruthy();
    expect(screen.getByText(/module nobody asked for/)).toBeTruthy();
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

/* --------------------------------------------- what this project can be asked -- */

describe("what this project can be asked to do", () => {
  function withCommands(commands: DaemonState["commands"], rest: Partial<DaemonState> = {}) {
    return daemonState({
      projects: [project({ project_id: "nucleos", mode: "shadow", project_root: "C:/p" })],
      commands,
      ...rest,
    });
  }

  /**
   * The rule that keeps this from becoming a drawer, asserted rather than described.
   *
   * A gate's last verdict is a fact you want without asking, so it earns a place at the foot of the
   * page. Everything else is a verb you go looking for by name, and it is not on the screen until
   * somebody asks. If both ended up in the bar this test still passes on count — so it checks that
   * the two non-gates are ABSENT, which is the half that actually holds the line.
   */
  it("puts the gates in the bar and leaves everything else to the palette", async () => {
    await openState(
      withCommands([
        projectCommand({ id: 1, name: "gate", is_gate: true }),
        projectCommand({ id: 2, name: "fmt", command: "cargo fmt --check", is_gate: false }),
        projectCommand({ id: 3, name: "docs", command: "cargo doc", is_gate: false }),
      ]),
    );

    expect(await screen.findByRole("button", { name: /^gate,/ })).toBeTruthy();
    expect(screen.queryByRole("button", { name: /^fmt,/ })).toBeNull();
    expect(screen.queryByRole("button", { name: /^docs,/ })).toBeNull();
    // And the door to the rest says how many there are, so a short bar is not mistaken for a short
    // list.
    expect(screen.getByRole("button", { name: /all 3/ })).toBeTruthy();
  });

  /**
   * A project whose commands are all verbs claims nothing about being green — and says so, rather
   * than drawing an empty bar that reads as "nothing is wrong".
   */
  it("says nothing here claims to know whether the project is green", async () => {
    await openState(
      withCommands([projectCommand({ id: 2, name: "fmt", is_gate: false })]),
    );
    expect(await screen.findByText(/No command here is marked a gate/)).toBeTruthy();
  });

  /** What was SENT — the id the núcleo will act on, not the button the component thinks it drew. */
  it("starts the command that was clicked", async () => {
    const state = withCommands([
      projectCommand({ id: 7, name: "gate", is_gate: true }),
      projectCommand({ id: 9, name: "suite", is_gate: true }),
    ]);
    await openState(state);

    fireEvent.click(await screen.findByRole("button", { name: /^suite,/ }));
    await waitFor(() => expect(state.started).toEqual([9]));
  });

  /**
   * The never-collapse contract, at the smallest scale it appears anywhere in this app: three
   * verdicts, three sentences, and the two that are easiest to merge are the two that must not be.
   *
   * A command that could not be measured says nothing about whether the project works. Drawn as a
   * failure it would send somebody to look for broken tests that are not broken — and drawn as a
   * pass it would be worse still.
   */
  it("never lets a measurement that did not happen read as a failure", async () => {
    await openState(
      withCommands([
        projectCommand({
          id: 1,
          name: "gate",
          last: {
            outcome: "failed",
            started_at: "2026-08-23T09:00:00Z",
            ended_at: "2026-08-23T09:04:00Z",
            exit_code: 101,
            output: "2 tests failed",
          },
        }),
        projectCommand({
          id: 2,
          name: "suite",
          last: {
            outcome: "errored",
            started_at: "2026-08-23T09:00:00Z",
            ended_at: "2026-08-23T09:00:01Z",
            exit_code: null,
            output: "failed to start gate command",
          },
        }),
        projectCommand({ id: 3, name: "fmt", last: null }),
      ]),
    );

    expect(await screen.findByRole("button", { name: "gate, failed with exit 101" })).toBeTruthy();
    expect(screen.getByRole("button", { name: "suite, could not be measured" })).toBeTruthy();
    // And never run is a fourth thing again: not a pass, not a failure, not a measurement.
    expect(screen.getByRole("button", { name: "fmt, never run here" })).toBeTruthy();
  });

  /**
   * A command already going cannot be started again from here, which is the same answer the núcleo
   * gives a second click — drawn ahead of the round trip so the button does not invite one.
   */
  it("will not offer to start a command that is already going", async () => {
    const state = withCommands([
      projectCommand({
        id: 1,
        name: "gate",
        last: {
          outcome: "running",
          started_at: "2026-08-23T09:00:00Z",
          ended_at: null,
          exit_code: null,
          output: null,
        },
      }),
    ]);
    await openState(state);

    const button = await screen.findByRole("button", { name: "gate, running now" });
    expect(button.hasAttribute("disabled")).toBe(true);
    fireEvent.click(button);
    expect(state.started).toEqual([]);
  });

  /** The stop is why, said in words, rather than a click that appears to do nothing. */
  it("says the emergency stop is why nothing started", async () => {
    const state = withCommands([projectCommand({ id: 1, name: "gate" })], {
      runRefusal: { status: 423, code: "kill_switch", detail: "kill_switch" },
    });
    await openState(state);

    fireEvent.click(await screen.findByRole("button", { name: /^gate,/ }));
    expect(await screen.findByText(/emergency stop is engaged/)).toBeTruthy();
  });

  /**
   * Drift takes the top of the page, and an ejected copy does not.
   *
   * `workflowDrift` was a hard-coded `false` until the library existed to fill it. Both halves are
   * asserted together because the risk is one of them: a page that led with an ejected copy would
   * be shouting about a decision somebody made on purpose, which is how a surface teaches people to
   * stop reading its top.
   */
  it("leads with a drifted workflow and says nothing at the top about an ejected one", async () => {
    await openState(
      daemonState({
        projects: [project({ project_id: "nucleos", mode: "shadow" })],
        workflows: [installedWorkflow({ standing: "drifted" })],
      }),
    );
    expect(await screen.findByText(/differs from the bundle it references/)).toBeTruthy();

    await openState(
      daemonState({
        projects: [project({ project_id: "nucleos", mode: "shadow" })],
        workflows: [
          installedWorkflow({ standing: "ejected", ejected_at: "2026-02-01T00:00:00Z" }),
        ],
      }),
    );
    expect(await screen.findAllByText(/Nothing waiting on you/)).toBeTruthy();
    // Still stated where it is listed — the panel says how long it has been frozen.
    expect(screen.getAllByText(/frozen/).length).toBeGreaterThan(0);
  });

  /**
   * Nothing declared is not an empty bar. Declaration rather than detection is the design's own
   * decision, and the sentence is where somebody learns a list will not appear by itself.
   */
  it("says a project has declared nothing rather than showing an empty bar", async () => {
    await openState(withCommands([]));
    expect(await screen.findByText(/Nothing declared yet/)).toBeTruthy();
  });

  /**
   * The declaration goes as the núcleo's own shape, checkboxes and all — asserted on what was SENT,
   * because the two flags are the ones a form is most likely to get subtly wrong: `is_gate` decides
   * whether the command earns a place in the bar, and `runnable_by` decides whether an autonomous
   * run may execute it.
   *
   * Absent is not an option for the second: the form always says which, so a command never reaches
   * the daemon relying on a default nobody chose.
   */
  it("declares a command as the núcleo's own shape, both flags said out loud", async () => {
    const state = withCommands([]);
    await openState(state);

    fireEvent.click(await screen.findByRole("button", { name: "declare a command" }));
    fireEvent.change(screen.getByLabelText("Command name"), { target: { value: "  gate  " } });
    fireEvent.change(screen.getByLabelText("What it runs"), { target: { value: "cargo test" } });
    fireEvent.click(screen.getByLabelText(/says whether this project is green/));
    fireEvent.click(screen.getByRole("button", { name: "declare" }));

    await waitFor(() => expect(state.declared.length).toBe(1));
    expect(state.declared[0]).toEqual({
      // Trimmed, because a name with a space on the end is a name nobody can type twice.
      name: "gate",
      command: "cargo test",
      // Empty means the project root, and the empty string is not that — it is a folder with no
      // name, which the núcleo would have to refuse.
      cwd: null,
      is_gate: true,
      runnable_by: "person",
    });
  });

  /**
   * A refused declaration keeps what was typed, and says which part was wrong. Dropping the form's
   * contents would make a rejected command a command retyped.
   */
  it("keeps a refused declaration on the screen and names the part that was wrong", async () => {
    const state = withCommands([]);
    state.writeRefusal = null;
    await openState(state);

    // The fake accepts declarations, so the refusal is arranged by making the route answer one.
    daemon.apiFetch.mockImplementation(async (path: string, init?: RequestInit) => {
      if (init?.method === "POST" && path.endsWith("/commands")) {
        throw new ApiRefusal(422, "invalid", "unbalanced quote in the command");
      }
      return daemonFetch(state)(path, init);
    });

    fireEvent.click(await screen.findByRole("button", { name: "declare a command" }));
    fireEvent.change(screen.getByLabelText("Command name"), { target: { value: "gate" } });
    fireEvent.change(screen.getByLabelText("What it runs"), {
      target: { value: 'bash -c "cargo test' },
    });
    fireEvent.click(screen.getByRole("button", { name: "declare" }));

    expect(await screen.findByText(/unbalanced quote/)).toBeTruthy();
    expect((screen.getByLabelText("Command name") as HTMLInputElement).value).toBe("gate");
  });

  /**
   * A workflow's command has no forget button, and that is the overlay rather than a gap: it
   * belongs to the bundle, and the way to be rid of it is to override it with one of this project's
   * own. A button that refused would be a worse answer than none.
   */
  it("offers to forget this project's commands and not the workflow's", async () => {
    const state = withCommands([
      projectCommand({ id: 1, name: "gate", source: "project" }),
      projectCommand({ id: 2, name: "fmt", source: "workflow", is_gate: false }),
    ]);
    await openState(state);

    fireEvent.click(await screen.findByRole("button", { name: "declare a command" }));
    const forgets = screen.getAllByRole("button", { name: "forget" });
    expect(forgets.length).toBe(1);
    expect(screen.getByText("the workflow's")).toBeTruthy();

    fireEvent.click(forgets[0]);
    await waitFor(() => expect(state.commands.map((row) => row.id)).toEqual([2]));
  });
});
