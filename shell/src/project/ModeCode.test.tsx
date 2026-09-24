// §spec workspace-de-projeto
import { describe, expect, it, vi } from "vitest";
import { fireEvent, screen, waitFor, within } from "@testing-library/react";
import {
  daemonFetch,
  daemonState,
  daemonText,
  heldSlots,
  project,
  proposal,
  renderApp,
  slot,
  type DaemonState,
} from "../test/harness";
import { ApiUnavailable } from "../data/client";
import type { RunSearchResult } from "../data/fleet";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
const opener = vi.hoisted(() => ({ openUrl: vi.fn() }));
vi.mock("@tauri-apps/plugin-opener", () => opener);
const daemon = vi.hoisted(() => ({ apiFetch: vi.fn(), apiText: vi.fn(), probeHealth: vi.fn() }));
vi.mock("../data/client", async (original) => ({
  ...(await original<typeof import("../data/client")>()),
  ...daemon,
}));

const DIFF = [
  "diff --git a/core/src/http.rs b/core/src/http.rs",
  "--- a/core/src/http.rs",
  "+++ b/core/src/http.rs",
  "@@ -4,2 +4,2 @@",
  " fn route() {",
  "-  old();",
  "+  new();",
  "",
].join("\n");

function liveRun(overrides: Partial<RunSearchResult> = {}): RunSearchResult {
  return {
    id: 41,
    project_id: "nucleos",
    status: "running",
    mode: "shadow",
    created_at: "2026-08-23T09:00:00Z",
    completed_at: null,
    cost_usd: null,
    prompt_excerpt: "Teach the diff route to refuse a path outside the worktree",
    ...overrides,
  };
}

/** Runs 41 (and optionally 42) holding worktrees in `nucleos`, with a two-file change. */
function reviewState(runs: number[] = [41]): DaemonState {
  const state = daemonState({
    projects: [project({ project_id: "nucleos", mode: "shadow", wip_limit: 2 })],
    concurrency: {
      house: { limit: 4, held: runs.length },
      projects: [
        heldSlots(
          "nucleos",
          2,
          runs.map((id, at) => slot({ project_id: "nucleos", owner_id: id, slot: at + 1 })),
        ),
      ],
    },
    changed: { paths: ["core/src/http.rs", "shell/src/a.tsx"], tracked: 3_214 },
  });
  state.text.diff = DIFF;
  state.text.cat = "one\ntwo\nthree\n";
  return state;
}

/**
 * Mount the app at a path over `state`, answering the two reads the harness does not know —
 * the live run listing and a blame — and counting how often `/changed` is asked.
 */
async function open(
  state: DaemonState,
  path: string,
  extra: { live?: RunSearchResult[]; concurrencyDown?: boolean } = {},
) {
  const base = daemonFetch(state);
  const calls = { changed: 0 };
  daemon.apiFetch.mockImplementation(async (route: string, init?: RequestInit) => {
    if (route.startsWith("/runs?live=true")) return extra.live ?? [liveRun()];
    if (route.includes("/blame")) return [];
    if (route === "/concurrency" && extra.concurrencyDown === true) {
      throw new ApiUnavailable("transport", "connection refused");
    }
    if (route.includes("/changed")) calls.changed += 1;
    return base(route, init);
  });
  daemon.apiText.mockImplementation(daemonText(state));
  daemon.probeHealth.mockResolvedValue(true);
  const harness = await renderApp({ initialPath: path });
  return { ...harness, calls };
}

describe("the Code mode", () => {
  /** A daemon that is down leaves the reading undefined forever; "reading" over it is a lie. */
  it("says the núcleo did not answer rather than reading forever", async () => {
    await open(reviewState(), "/projects/nucleos/code", { concurrencyDown: true });
    expect(await screen.findByText(/did not answer, so there is no telling/)).toBeTruthy();
    expect(screen.queryByText("Reading what there is to review…")).toBeNull();
  });

  /**
   * The review names what it is reviewing — run, prompt, state, branch point — and the two ways
   * out it ends on: the run's own page, and the queue where its decision waits.
   */
  it("heads the review with the run, its state, its branch point and where it is decided", async () => {
    const state = reviewState();
    state.proposals = [proposal({ id: 7, run_id: 41, project_id: "nucleos" })];
    await open(state, "/projects/nucleos/code?run=41");

    expect(await screen.findByRole("heading", { name: "Run #41" })).toBeTruthy();
    expect(await screen.findByText(/refuse a path outside the worktree/)).toBeTruthy();
    expect(screen.getByText("running")).toBeTruthy();
    expect(screen.getByText("shadow run")).toBeTruthy();
    expect(screen.getByText(/feat\/x · from aaaaaaa/)).toBeTruthy();

    const details = screen.getByRole("link", { name: "Run details →" });
    expect(details.getAttribute("href")).toBe("/runs/41");
    const decision = await screen.findByRole("link", { name: "Its decision is waiting →" });
    expect(decision.getAttribute("href")).toContain("/waiting");
    expect(decision.getAttribute("href")).toContain("project=nucleos");
  });

  it("carries each run's prompt on its pill, so three runs can be told apart", async () => {
    await open(reviewState([41, 42]), "/projects/nucleos/code?run=41", {
      live: [liveRun(), liveRun({ id: 42, prompt_excerpt: "Rename the blame column" })],
    });
    const picker = await screen.findByRole("navigation", { name: "Runs to review" });
    expect(await within(picker).findByText("Rename the blame column")).toBeTruthy();
  });

  /**
   * The diff that decides whether an agent's work is trusted is the one drawn best: additions and
   * removals as elements, their prefixes kept, and each new-side line a door to the editor.
   */
  it("draws the run's diff with its sides told apart and its lines numbered", async () => {
    const { container } = await open(reviewState(), "/projects/nucleos/code?run=41");

    const region = await screen.findByRole("region", { name: "What run 41 changed" });
    expect(region.querySelector("ins")?.textContent).toBe("+  new();");
    expect(region.querySelector("del")?.textContent).toBe("-  old();");
    const door = await screen.findByRole("link", { name: "Open line 5 in VS Code" });
    expect(door.getAttribute("href")).toBe("vscode://file/C:/Projects/nucleos-run-41/core/src/http.rs:5");
    expect(container.querySelector("pre.leading-\\[1\\.45\\]")).toBeNull();
  });

  it("says when it read, and reads again when asked", async () => {
    const { calls } = await open(reviewState(), "/projects/nucleos/code?run=41");
    expect(await screen.findByText(/Read at \d\d:\d\d\./)).toBeTruthy();
    const before = calls.changed;

    fireEvent.click(screen.getByRole("button", { name: "Refresh" }));
    await waitFor(() => expect(calls.changed).toBe(before + 1));
  });

  it("offers the file and the blame as pressed settings, and says why they wait for a file", async () => {
    await open(reviewState(), "/projects/nucleos/code?run=41");
    const views = await screen.findByRole("group", { name: "How to read it" });
    const diff = within(views).getByRole("button", { name: "Diff" });
    const file = within(views).getByRole("button", { name: "File" });

    expect(diff.getAttribute("aria-pressed")).toBe("true");
    expect((file as HTMLButtonElement).disabled).toBe(true);
    const reason = document.getElementById(file.getAttribute("aria-describedby") ?? "");
    expect(reason?.textContent).toMatch(/Pick a file on the left/);

    fireEvent.click(await screen.findByRole("button", { name: "core/src/http.rs" }));
    expect((file as HTMLButtonElement).disabled).toBe(false);
    fireEvent.click(file);
    expect(file.getAttribute("aria-pressed")).toBe("true");
    // The file view numbers its lines, and each number opens the editor there.
    const line = await screen.findByRole("link", { name: "Open line 3 in VS Code" });
    expect(line.getAttribute("href")).toBe("vscode://file/C:/Projects/nucleos-run-41/core/src/http.rs:3");
  });

  /** The path of the last run opened the next one on a 404, which read as a fault in the daemon. */
  it("forgets the open file when another run is picked", async () => {
    await open(reviewState([41, 42]), "/projects/nucleos/code?run=41");
    fireEvent.click(await screen.findByRole("button", { name: "core/src/http.rs" }));
    expect(screen.queryByText("everything this run changed")).toBeNull();

    const picker = screen.getByRole("navigation", { name: "Runs to review" });
    fireEvent.click(within(picker).getByText("run #42"));
    expect(await screen.findByRole("heading", { name: "Run #42" })).toBeTruthy();
    expect(screen.getByText("everything this run changed")).toBeTruthy();
  });

  it("says so when VS Code does not answer, beside the door that was pressed", async () => {
    opener.openUrl.mockRejectedValueOnce(new Error("no handler for vscode://"));
    await open(reviewState(), "/projects/nucleos/code?run=41");
    fireEvent.click(await screen.findByRole("button", { name: "Open the worktree in VS Code" }));
    expect(await screen.findByText(/VS Code did not answer/)).toBeTruthy();
  });
});
