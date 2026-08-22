import { describe, expect, it, vi } from "vitest";
import { screen } from "@testing-library/react";
import {
  daemonFetch,
  daemonState,
  project,
  readings,
  renderApp,
  renderWithQuery,
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
  daemon.probeHealth.mockResolvedValue(true);
  // The whole app, through the real router: a route that is missing from the
  // real tree is missing here too, so this proves the mode resolves as well as
  // what it renders.
  return renderApp({ initialPath: `/projects/nucleos/${options.mode ?? "estado"}` });
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

    // Four readings, four em dashes, four reasons — and not one zero among them. The harness's
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

  it("shows the other two modes as designed and not yet served", async () => {
    const code = await openWorkspace({ mode: "codigo" });
    expect(await screen.findByText(/review surface for nucleos is not built yet/)).toBeTruthy();
    code.unmount();

    await openWorkspace({ mode: "workflows" });
    expect(await screen.findByText(/No workflow is installed in nucleos/)).toBeTruthy();
  });
});
