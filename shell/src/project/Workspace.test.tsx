import { describe, expect, it, vi } from "vitest";
import { screen } from "@testing-library/react";
import { daemonFetch, daemonState, project, renderApp, renderWithQuery } from "../test/harness";
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
} = {}) {
  const state = daemonState({
    kill: { engaged: options.engaged === true },
    projects: [
      project({ project_id: "nucleos", mode: "shadow", open_proposals: options.openProposals ?? 0 }),
    ],
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

    expect(screen.getAllByText("not measured yet").length).toBe(4);

    /*
      The calm line is deliberately narrow. The design's own example sentence
      reads "gate green on the last 12" — and that clause cannot be written yet,
      because nothing counts gates per project. Naming a reading in a panel that
      says "not measured" is honest; asserting it is fine in the sentence at the
      top is the thing §12 forbids, so the ban is on the sentence, not the page.
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

  it("shows the other two modes as designed and not yet served", async () => {
    const code = await openWorkspace({ mode: "codigo" });
    expect(await screen.findByText(/review surface for nucleos is not built yet/)).toBeTruthy();
    code.unmount();

    await openWorkspace({ mode: "workflows" });
    expect(await screen.findByText(/No workflow is installed in nucleos/)).toBeTruthy();
  });
});
