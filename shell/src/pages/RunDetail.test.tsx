import { act } from "react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { screen } from "@testing-library/react";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

const daemon = vi.hoisted(() => ({ apiFetch: vi.fn(), apiText: vi.fn(), probeHealth: vi.fn() }));
vi.mock("../data/client", async (original) => ({
  ...(await original<typeof import("../data/client")>()),
  ...daemon,
}));

import { ApiUnavailable } from "../data/client";
import { keys } from "../data/keys";
import type { RunDetail, RunTailChunk } from "../data/runs";
import { daemonFetch, daemonState, project, renderApp } from "../test/harness";

beforeEach(() => {
  daemon.apiFetch.mockReset();
  daemon.apiText.mockReset();
  daemon.probeHealth.mockReset();

  daemon.probeHealth.mockResolvedValue(true);
  daemon.apiText.mockResolvedValue("daemon running");
  localStorage.clear();
});

/* ------------------------------------------------------------- fixtures -- */

function detail(overrides: Partial<RunDetail> = {}): RunDetail {
  return {
    id: 5,
    project_id: "alpha",
    status: "completed",
    gate_status: "passed",
    gate_exit_code: 0,
    gate_output: null,
    exit_code: 0,
    stdout: "done",
    stderr: null,
    session_id: "s-1",
    cost_usd: 0.0123,
    input_tokens: 1200,
    output_tokens: 300,
    cache_read_tokens: 8000,
    num_turns: 3,
    context_fill: 40_000,
    steerable: false,
    successor_run_id: null,
    ...overrides,
  };
}

/**
 * One run's routes, over the shared responder.
 *
 * `tail` is a function of the offset rather than a queue of answers: the shell
 * polls, so the tail is asked repeatedly and in an order nobody controls, and a
 * queue of one-shot answers runs out halfway through the second tick. Asked-for
 * paths are recorded so a test can prove which cursor was sent.
 */
function detailFetch(
  run: RunDetail,
  tail: (since: number) => RunTailChunk | undefined,
  asked: string[],
): (path: string, init?: RequestInit) => Promise<unknown> {
  const shared = daemonFetch(daemonState({ projects: [project({ project_id: "alpha" })] }));
  return async (path, init) => {
    if (init?.method !== undefined && init.method !== "GET") return await shared(path, init);
    if (path === `/runs/${run.id}`) return run;
    const cursor = new RegExp(`^/runs/${run.id}/tail\\?since=(\\d+)$`).exec(path);
    if (cursor !== null) {
      asked.push(path);
      return tail(Number(cursor[1]));
    }
    if (path.startsWith("/runs?")) return [];
    if (path === "/presets") return [];
    return await shared(path, init);
  };
}

/** The daemon has no live tail for this run — a 204, which `apiFetch` hands back as `undefined`. */
const NO_TAIL = () => undefined;

/* ----------------------------------------------------------------- gate -- */

describe("RunDetail — the gate", () => {
  it("renders a run nobody gated as ungated, never as failed", async () => {
    const asked: string[] = [];
    daemon.apiFetch.mockImplementation(
      detailFetch(
        detail({ gate_status: null, gate_exit_code: null, gate_output: null }),
        NO_TAIL,
        asked,
      ),
    );

    await renderApp({ initialPath: "/runs/5" });

    // A NULL `gate_status` means no gate was ever configured. Rendered in red it
    // would be the shell telling somebody their tests broke when they never
    // wrote any.
    const reading = await screen.findByText("no gate configured");
    expect(reading.className).toContain("ui-badge-off");
    expect(reading.className).not.toContain("ui-badge-danger");
    expect(screen.queryByText(/gate failed/i)).toBeNull();
    // And it does not borrow the passing tone either: nothing measured this.
    expect(screen.queryByText(/gate passed/i)).toBeNull();
    expect(screen.getByText(/no gate configured for it/)).toBeDefined();
  });

  it("keeps a gate that could not run apart from one that failed", async () => {
    const asked: string[] = [];
    daemon.apiFetch.mockImplementation(
      detailFetch(detail({ gate_status: "errored", gate_exit_code: 127 }), NO_TAIL, asked),
    );

    await renderApp({ initialPath: "/runs/5" });

    expect(await screen.findByText("gate not measured")).toBeDefined();
    expect(screen.queryByText("gate failed")).toBeNull();
  });
});

/* ----------------------------------------------------------------- tail -- */

describe("RunDetail — the live tail", () => {
  it("advances by the daemon's byte cursor, not by the length of the string", async () => {
    // "olá " is four JavaScript characters and FIVE bytes. A shell that measured
    // the received string would ask for `since=4` and redraw a character it had
    // already shown — and would keep drifting from there.
    const chunks: Record<number, RunTailChunk> = {
      0: { text: "olá ", next: 5, live: true },
      5: { text: "mundo", next: 10, live: true },
      10: { text: "", next: 10, live: true },
    };
    const asked: string[] = [];
    daemon.apiFetch.mockImplementation(
      detailFetch(detail({ status: "running", stdout: null }), (since) => chunks[since], asked),
    );

    const { queryClient } = await renderApp({ initialPath: "/runs/5" });
    expect(await screen.findByText(/olá/)).toBeDefined();
    expect(asked[0]).toBe("/runs/5/tail?since=0");

    await act(async () => {
      await queryClient.refetchQueries({ queryKey: keys.runs.tail(5) });
    });

    // Appended, not replaced — and asked for from the offset the daemon gave.
    expect(await screen.findByText(/olá mundo/)).toBeDefined();
    expect(asked).toContain("/runs/5/tail?since=5");
    expect(asked).not.toContain("/runs/5/tail?since=4");
  });

  it("reads a 204 as recorded, which is where the output is rather than an error", async () => {
    const asked: string[] = [];
    daemon.apiFetch.mockImplementation(detailFetch(detail({ stdout: "all done" }), NO_TAIL, asked));

    await renderApp({ initialPath: "/runs/5" });

    expect(await screen.findByText(/recorded/)).toBeDefined();
    // The output is not missing — it is in `runs.stdout`. A retry would achieve
    // nothing, so nothing on screen offers one.
    expect(screen.queryByText(/unreachable/)).toBeNull();
  });

  it("says the tail is unreachable when the read itself failed", async () => {
    const asked: string[] = [];
    const answer = detailFetch(detail({ status: "running" }), NO_TAIL, asked);
    daemon.apiFetch.mockImplementation(async (path: string, init?: RequestInit) => {
      if (path.startsWith("/runs/5/tail")) {
        throw new ApiUnavailable("transport", "the daemon did not answer");
      }
      return await answer(path, init);
    });

    await renderApp({ initialPath: "/runs/5" });

    // A read that failed says nothing at all about where the output is, which is
    // the opposite of what the 204 above says. Collapsing them would either hide
    // an outage or invent one.
    expect(await screen.findByText(/the live tail is unreachable/)).toBeDefined();
    expect(screen.queryByText(/^recorded/)).toBeNull();
  });
});

/* ------------------------------------------------------------- steering -- */

describe("RunDetail — steering", () => {
  it("shows no composer at all for a run that never opted in", async () => {
    const asked: string[] = [];
    daemon.apiFetch.mockImplementation(
      detailFetch(detail({ status: "running", steerable: false }), NO_TAIL, asked),
    );

    await renderApp({ initialPath: "/runs/5" });
    await screen.findByRole("heading", { level: 1, name: "Run 5" });

    // Absent, not disabled. `steerable` is decided when the run is created and
    // never after, so nothing a person could do here would make this run listen
    // — a greyed-out box would be an invitation to keep trying.
    expect(screen.queryByText("Speak to this run")).toBeNull();
    expect(screen.queryByLabelText("Say something to this run")).toBeNull();
    expect(screen.queryByRole("button", { name: "Send turn" })).toBeNull();
    expect(screen.queryByRole("button", { name: /End the turns/ })).toBeNull();
  });

  it("gives a run that did opt in a composer and a way to close its turns", async () => {
    const asked: string[] = [];
    daemon.apiFetch.mockImplementation(
      detailFetch(detail({ status: "running", steerable: true }), NO_TAIL, asked),
    );

    await renderApp({ initialPath: "/runs/5" });

    expect(await screen.findByLabelText("Say something to this run")).toBeDefined();
    // Without this control a steerable run can only end by going quiet long
    // enough to trip the progress deadline, and is then recorded `timed_out` —
    // a failure status, for having waited.
    expect(screen.getByRole("button", { name: /End the turns/ })).toBeDefined();
  });
});

/* -------------------------------------------------------------- handoff -- */

describe("RunDetail — the handoff", () => {
  it("links the successor a context handoff created", async () => {
    const asked: string[] = [];
    daemon.apiFetch.mockImplementation(
      detailFetch(detail({ context_fill: 165_000, successor_run_id: 6 }), NO_TAIL, asked),
    );

    await renderApp({ initialPath: "/runs/5" });

    // `successor_run_id` is in the núcleo and was absent from the old shell's
    // type, so a run that ran out of context and continued elsewhere looked like
    // a run that simply stopped.
    expect(await screen.findByRole("link", { name: "Run 6 continued it" })).toBeDefined();
    // And the meter says the run was past the point where the daemon splits it.
    expect(screen.getByText(/at the handoff mark/)).toBeDefined();
  });
});
