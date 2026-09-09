// §spec motor-de-workflows
import { describe, expect, it, vi } from "vitest";
import { fireEvent, screen, waitFor, within } from "@testing-library/react";
import {
  bundle,
  daemonFetch,
  daemonState,
  daemonText,
  installedWorkflow,
  project,
  renderApp,
  type DaemonState,
} from "../test/harness";
import { NO_WORKFLOW_MEANS } from "../data/workflows";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
const opener = vi.hoisted(() => ({ openUrl: vi.fn() }));
vi.mock("@tauri-apps/plugin-opener", () => opener);
const daemon = vi.hoisted(() => ({ apiFetch: vi.fn(), apiText: vi.fn(), probeHealth: vi.fn() }));
vi.mock("../data/client", async (original) => ({
  ...(await original<typeof import("../data/client")>()),
  ...daemon,
}));

/** The Workflows mode, over a daemon holding exactly these facts. */
async function openWorkflows(overrides: Partial<DaemonState> = {}) {
  const state = daemonState({
    projects: [project({ project_id: "nucleos", mode: "shadow" })],
    ...overrides,
  });
  daemon.apiFetch.mockImplementation(daemonFetch(state));
  daemon.apiText.mockImplementation(daemonText(state));
  daemon.probeHealth.mockResolvedValue(true);
  const rendered = await renderApp({ initialPath: "/projects/nucleos/workflows" });
  return { state, rendered };
}

describe("the workflows mode", () => {
  /**
   * A project with no workflow is a real answer, not a gap.
   *
   * The alternative — an empty canvas, or a spinner that never resolves — would say "something is
   * missing here" about a project that is developing perfectly well by hand.
   */
  it("says a project has no workflow rather than showing an empty graph", async () => {
    await openWorkflows({ library: [] });
    expect(await screen.findByText(/No workflow is installed here/)).toBeTruthy();
  });

  /**
   * The same nothing, on two screens, saying the same thing.
   *
   * They are rightly two shapes — a `Teach` here, over the library somebody came to install from,
   * and one quiet line in the State mode's panel — and the claim inside them was written twice.
   * One copy said the app does not pretend otherwise by drawing an empty graph and the other did
   * not, so the same emptiness had two explanations and only one mentioned the graph. Asserted
   * against the exported sentence rather than against a string typed here, because a test carrying
   * its own third copy would go green on a page that had drifted from both.
   */
  it("explains the emptiness in the same words as the State mode", async () => {
    const empty = await openWorkflows({ library: [] });
    expect(await screen.findByText(NO_WORKFLOW_MEANS, { exact: false })).toBeTruthy();

    empty.rendered.unmount();

    await renderApp({ initialPath: "/projects/nucleos/state" });
    const panel = within(await screen.findByRole("region", { name: "Workflow" }));
    fireEvent.click(await panel.findByRole("button", { name: "why?" }));
    expect(panel.getByText(NO_WORKFLOW_MEANS, { exact: false })).toBeTruthy();
  });

  /** The library is what you install from, and a bundle already in use says so instead of offering. */
  it("offers the library, and marks the one this project already uses", async () => {
    const { state } = await openWorkflows({
      library: [bundle({ version: "1.0" }), bundle({ version: "1.1", hash: "sha256:bbbb" })],
      workflows: [installedWorkflow({ version: "1.0" })],
    });

    expect(await screen.findByText("used here")).toBeTruthy();
    // The other version is an upgrade rather than an addition, and the button says which — one
    // workflow per name means "use 1.1" replaces the 1.0 that is working.
    fireEvent.click(screen.getByRole("button", { name: "use 1.1 instead" }));
    await waitFor(() => expect(state.workflowChanges.length).toBe(1));
    expect(state.workflowChanges[0]).toEqual({
      verb: "install",
      name: "harness",
      version: "1.1",
    });
  });

  it("the quiet controls are the primitive", async () => {
    await openWorkflows({ workflows: [installedWorkflow()] });

    expect((await screen.findByRole("button", { name: "stop using it" })).className).toContain(
      "ui-button-quiet",
    );
  });

  /**
   * Drift is stated with both hashes, never implied.
   *
   * The daemon serves what was pinned and what is there now precisely so the page can make a claim
   * somebody is able to check. A badge alone would be an assertion on trust.
   */
  it("shows both hashes when the bundle has changed under the pin", async () => {
    await openWorkflows({
      workflows: [
        installedWorkflow({
          standing: "drifted",
          hash: "sha256:1111111111111111",
          origin_hash: "sha256:2222222222222222",
        }),
      ],
    });

    expect(await screen.findByText("111111111111")).toBeTruthy();
    expect(screen.getByText("222222222222")).toBeTruthy();
    expect(screen.getByRole("button", { name: "follow the library again" })).toBeTruthy();
  });

  /**
   * §6.3's guard: three exits, inline, and the middle one is the point.
   *
   * Most of the time what somebody wants is to improve the workflow rather than diverge from it.
   * Offering both side by side is what makes ejecting deliberate instead of the path of least
   * resistance — and the panel is inline because a modal takes the page away to ask about something
   * that is on it.
   */
  it("puts editing in the library beside ejecting, and never opens a dialog", async () => {
    const { state, rendered } = await openWorkflows({
      library: [bundle({ path: "C:/lib/harness/1.0" })],
      workflows: [installedWorkflow()],
    });

    fireEvent.click(await screen.findByRole("button", { name: "eject" }));
    const guard = screen.getByRole("group", { name: "Eject or edit in the library" });
    expect(guard).toBeTruthy();
    // Inline, not a modal: nothing here is a dialog, and the rest of the page is still on screen.
    expect(rendered.container.querySelector("dialog")).toBeNull();
    expect(await screen.findByText("On this machine")).toBeTruthy();

    fireEvent.click(screen.getByRole("button", { name: "edit in the library" }));
    expect(opener.openUrl).toHaveBeenCalledWith("vscode://file/C:/lib/harness/1.0");
    expect(state.workflowChanges).toEqual([]);

    fireEvent.click(screen.getByRole("button", { name: "eject" }));
    fireEvent.click(screen.getByRole("button", { name: "eject and edit" }));
    await waitFor(() => expect(state.workflowChanges.length).toBe(1));
    expect(state.workflowChanges[0].verb).toBe("eject");
  });

  /**
   * The diff comes before the button that would destroy what it shows.
   *
   * For an ejected workflow, "update" REPLACES the project's copy. The thing that says what would
   * be lost has to be reachable first, and it is only fetched when somebody opens it — walking two
   * directories is not something a page does to draw its first paint.
   */
  it("fetches the diff only when it is opened, and names the files that diverged", async () => {
    await openWorkflows({
      workflows: [
        installedWorkflow({
          standing: "ejected",
          ejected_at: "2026-02-01T00:00:00Z",
          local_hash: "sha256:cccc",
        }),
      ],
      workflowDiff: {
        origin_version: "1.0",
        changes: [
          { path: "skills/plan.md", change: "changed" },
          { path: "scripts/extra.py", change: "added" },
        ],
        unchanged: 4,
      },
    });

    expect(await screen.findByText(/receiving no updates for/)).toBeTruthy();
    expect(
      daemon.apiFetch.mock.calls.some((call) => String(call[0]).includes("/diff")),
      "the diff must not be fetched before it is asked for",
    ).toBe(false);

    fireEvent.click(screen.getByRole("button", { name: "see the diff" }));
    expect(await screen.findByText("skills/plan.md")).toBeTruthy();
    expect(screen.getByText("scripts/extra.py")).toBeTruthy();
    expect(screen.getByText(/4 other files identical/)).toBeTruthy();
  });

  /**
   * An ejected copy identical to the origin is still frozen, and the sentence says so.
   *
   * "No differences" alone would read as *you are up to date*, which is the opposite of what being
   * ejected means: it receives nothing new whether or not anybody has touched it.
   */
  it("says an untouched ejected copy is still frozen", async () => {
    await openWorkflows({
      workflows: [installedWorkflow({ standing: "ejected", ejected_at: "2026-02-01T00:00:00Z" })],
      workflowDiff: { origin_version: "1.0", changes: [], unchanged: 6 },
    });
    fireEvent.click(await screen.findByRole("button", { name: "see the diff" }));
    expect(await screen.findByText(/still frozen/)).toBeTruthy();
  });

  /**
   * The new machine: the pin travelled and the bundle did not.
   *
   * §6.1's first named weakness. The page shows the origin the pin recorded, because being told
   * what you are missing is the difference between a fixable state and a mystery — the same
   * mistake `CLAUDE.md` records about `.githooks/` not travelling with the repository.
   */
  it("names where a missing bundle came from instead of showing nothing", async () => {
    await openWorkflows({
      workflows: [
        installedWorkflow({ standing: "missing", origin_hash: null, origin: "git:acme/harness#1.0" }),
      ],
    });
    expect(await screen.findByText("git:acme/harness#1.0")).toBeTruthy();
    // And there is nothing to open in a library that does not have it.
    fireEvent.click(screen.getByRole("button", { name: "eject" }));
    expect(screen.getByRole("button", { name: "edit in the library" })).toHaveProperty(
      "disabled",
      true,
    );
  });

  /** A refusal is the daemon's, in the page's words, and the row stays where it is. */
  it("says why a change was refused rather than dropping the row", async () => {
    await openWorkflows({
      workflows: [installedWorkflow()],
      workflowRefusal: { status: 423, code: "kill_switch", detail: "stopped" },
    });

    fireEvent.click(await screen.findByRole("button", { name: "eject" }));
    fireEvent.click(screen.getByRole("button", { name: "eject and edit" }));
    expect(await screen.findByText(/emergency stop is engaged/)).toBeTruthy();
    expect(screen.getByText("harness")).toBeTruthy();
  });

  /**
   * A pins file that does not parse is a file somebody has to fix, and the parser's words say which
   * line. An empty list here would say "this project uses no workflow", which is a claim the daemon
   * explicitly could not make.
   */
  it("reports an unreadable pins file in the parser's own words", async () => {
    await openWorkflows({ pinsError: "workflows: mapping values are not allowed at line 3" });
    expect(await screen.findByText(/mapping values are not allowed at line 3/)).toBeTruthy();
    expect(screen.queryByText(/No workflow is installed here/)).toBeNull();
  });

  /**
   * A machine with no library says so. An empty shelf and no shelf are different facts, and only
   * one of them has an install button that could ever work.
   */
  it("distinguishes a machine with no library from an empty one", async () => {
    await openWorkflows({ library: null });
    expect(await screen.findByText(/no folder for a workflow library/)).toBeTruthy();
  });
});
