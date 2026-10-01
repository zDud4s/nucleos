import { act } from "react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { screen, within } from "@testing-library/react";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

const daemon = vi.hoisted(() => ({ apiFetch: vi.fn(), apiText: vi.fn(), probeHealth: vi.fn() }));
vi.mock("../data/client", async (original) => ({
  ...(await original<typeof import("../data/client")>()),
  ...daemon,
}));

import { ApiUnavailable } from "../data/client";
import { keys } from "../data/keys";
import type { RunDetail, RunStop, RunTailChunk } from "../data/runs";
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
    authored_prompt_estimate: 10_500,
    cli_own_estimate: 18_700,
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
  stop?: RunStop,
): (path: string, init?: RequestInit) => Promise<unknown> {
  const shared = daemonFetch(daemonState({ projects: [project({ project_id: "alpha" })] }));
  return async (path, init) => {
    if (init?.method !== undefined && init.method !== "GET") return await shared(path, init);
    if (path === `/runs/${run.id}`) return run;
    // Refused when a test passes none, deliberately: that is the shape of every
    // test written before this route existed, and serving them a report would
    // hide the thing worth pinning — that the block stays silent rather than
    // drawing a failure over a page that is fine.
    if (path === `/runs/${run.id}/stop`) {
      if (stop === undefined) throw new Error("no stop report for this run");
      return stop;
    }
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

/**
 * A stop report, with every kind-specific field at `null` unless a case names it.
 *
 * That default is the contract and not laziness: the daemon sends every field
 * present and null rather than omitting the ones a kind does not use, so a
 * fixture that left them out would let a component read `undefined` under a type
 * promising otherwise — and pass.
 */
function stopReport(overrides: Partial<RunStop> = {}): RunStop {
  return {
    run_id: 5,
    status: "completed",
    kind: "completed",
    summary: "the run completed",
    decisions_recorded: true,
    gate: null,
    timeout: null,
    leading_up: null,
    exit_code: null,
    stderr_tail: null,
    successor_run_id: null,
    ...overrides,
  };
}

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
    // wrote any. The reading is the pipeline's gate stage — the page says each
    // state once, and this is the once.
    const reading = await screen.findByText("no gate configured");
    const stage = reading.closest("g")!.getAttribute("class");
    expect(stage).toContain("ui-runpipe-off");
    expect(stage).not.toContain("ui-runpipe-danger");
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

/* --------------------------------------------------------- why it stopped -- */

describe("RunDetail — why it stopped", () => {
  /**
   * §9's first requirement, and the one the whole block exists to satisfy: this
   * panel and the gate panel above it must not read as the same thing.
   *
   * "Gate" above is a test suite's exit code — a verdict about the CODE. Here,
   * `kind: "gate"` means the run stopped waiting for a person to approve a tool
   * call — a fact about the DAEMON. One word, two mechanisms, and a reader who
   * conflates them concludes their tests failed when nothing ran them.
   */
  it("gives the tool gate a title of its own rather than a second panel called Gate", async () => {
    daemon.apiFetch.mockImplementation(
      detailFetch(
        detail({ status: "awaiting_approval" }),
        NO_TAIL,
        [],
        stopReport({
          status: "awaiting_approval",
          kind: "gate",
          summary: "the run is waiting for somebody to approve a tool call",
          gate: {
            tool_name: "Bash",
            action_class: "unrecognized",
            decision: "pending_approval",
            reason: "unrecognized shell commands and code execution require approval",
            classifier_version: 11,
            policy_digest: null,
            tool_input: '{"command":"cargo fmt --all -- --check"}',
            tool_input_truncated: false,
            created_at: "2026-08-30T23:32:15Z",
          },
        }),
      ),
    );

    await renderApp({ initialPath: "/runs/5" });

    expect(await screen.findByText("Why it stopped")).toBeDefined();
    // The deterministic gate keeps its own panel and its own name beside this one.
    expect(screen.getByText("Gate")).toBeDefined();
    expect(screen.getByText(/approve a tool call/)).toBeDefined();
    expect(screen.getByText(/unrecognized shell commands/)).toBeDefined();
  });

  it("still puts the tail first on a live run, with the judge's block at the end", async () => {
    const answer = detailFetch(detail({ status: "running", stdout: null }), NO_TAIL, []);
    daemon.apiFetch.mockImplementation(async (path: string, init?: RequestInit) =>
      path.endsWith("/judge-verdicts") ? [] : answer(path, init),
    );

    await renderApp({ initialPath: "/runs/5" });

    // `RunDetail.tsx` lifts block 3 (the tail) to the top of a live run; the judge's block,
    // appended last, must not have shifted which block that is.
    const tail = (await screen.findByText("Live output")).closest("section") as HTMLElement;
    const facts = screen.getByText("This run").closest("section") as HTMLElement;
    expect(tail.compareDocumentPosition(facts) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
  });

  /**
   * §9: nothing is rendered for a live run except that it is still going.
   *
   * The two kind-specific blocks are asserted ABSENT rather than left unchecked,
   * because a timeout verdict on a run still going would be a statement about a
   * deadline nothing has reached.
   */
  it("tells a live run it has not stopped, and says nothing else about it", async () => {
    daemon.apiFetch.mockImplementation(
      detailFetch(
        detail({ status: "running", stdout: null }),
        NO_TAIL,
        [],
        stopReport({
          status: "running",
          kind: "running",
          summary: "the run is still going",
        }),
      ),
    );

    await renderApp({ initialPath: "/runs/5" });

    expect(await screen.findByText("the run is still going")).toBeDefined();
    // Scoped to this panel and not to the page. "Exit code" is also a label in
    // the facts panel above, so a bare `queryByText` here asserts something
    // about a block this test is not about — and fails for a reason that has
    // nothing to do with §9. Found by writing it the loose way first.
    const panel = screen.getByText("Why it stopped").closest("section");
    expect(panel).not.toBeNull();
    expect(within(panel as HTMLElement).queryByText(/silence ceiling/)).toBeNull();
    expect(within(panel as HTMLElement).queryByText(/Exit code/)).toBeNull();
  });

  /**
   * §5.3, and it is the difference between an absence and a silence. An empty
   * list is the same shape whether the mode records nothing or the run simply
   * asked for nothing, and only one of those is worth a person's attention.
   */
  it("says a real-mode run records no decisions rather than showing an empty list", async () => {
    daemon.apiFetch.mockImplementation(
      detailFetch(
        detail(),
        NO_TAIL,
        [],
        stopReport({ decisions_recorded: false, leading_up: [] }),
      ),
    );

    await renderApp({ initialPath: "/runs/5" });

    expect(await screen.findByText(/recorded no decisions/)).toBeDefined();
    expect(screen.getByText(/Nothing is missing/)).toBeDefined();
  });

  /**
   * §6. The verdict a person can act on is which ceiling fired, and `silence`
   * is only claimable because the wall clock was nowhere near — which is what
   * the sentence has to convey, in minutes, without ever naming a `wall`
   * verdict the daemon cannot support.
   */
  it("reads a timeout as the silence it was, against a wall clock that was nowhere near", async () => {
    daemon.apiFetch.mockImplementation(
      detailFetch(
        detail({ status: "timed_out" }),
        NO_TAIL,
        [],
        stopReport({
          status: "timed_out",
          kind: "timeout",
          summary: "the run timed out",
          timeout: {
            elapsed_seconds: 1923,
            wall_ceiling_seconds: 7200,
            silence_ceiling_seconds: 1800,
            measured_from: "created_at",
            verdict: "silence",
          },
          leading_up: [],
        }),
      ),
    );

    await renderApp({ initialPath: "/runs/5" });

    expect(await screen.findByText(/32 minutes against a 30 minutes silence ceiling/)).toBeDefined();
    expect(screen.getByText(/120 minutes wall clock/)).toBeDefined();
    expect(screen.getByText(/stopped reporting/)).toBeDefined();
  });

  /**
   * The block explains something already on the page, so a daemon that will not
   * answer this one route must cost a reader nothing. An error strip here would
   * be a second failure report about a page that rendered fine.
   */
  it("stays silent when the report cannot be had, rather than reporting its own failure", async () => {
    daemon.apiFetch.mockImplementation(detailFetch(detail(), NO_TAIL, []));

    await renderApp({ initialPath: "/runs/5" });

    // The page itself is there.
    expect(await screen.findByText("Gate")).toBeDefined();
    expect(screen.queryByText("Why it stopped")).toBeNull();
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

describe("RunDetail — absent readings", () => {
  it("draws a run whose cost and context the daemon never sent", async () => {
    const run = detail();
    delete (run as { cost_usd?: unknown }).cost_usd;
    delete (run as { context_fill?: unknown }).context_fill;
    const asked: string[] = [];
    daemon.apiFetch.mockImplementation(detailFetch(run, NO_TAIL, asked, stopReport()));

    await renderApp({ initialPath: "/runs/5" });

    expect(await screen.findByText("cost not recorded")).toBeDefined();
    expect(screen.getByText("context fill not reported")).toBeDefined();
  });
});

describe("RunDetail headline", () => {
  it("a terminal run's headline says where it ran and leaves the state to the badge", async () => {
    daemon.apiFetch.mockImplementation(detailFetch(detail({ status: "completed", project_id: "alpha" }), NO_TAIL, []));

    await renderApp({ initialPath: "/runs/5" });

    const heading = await screen.findByText("ran in alpha");
    expect(heading.textContent).toBe("ran in alpha");
    expect(screen.getAllByText("completed").length).toBeGreaterThan(0);
    expect(heading.textContent).not.toContain("completed");
  });

  it("keeps terminal state out of a timed-out headline", async () => {
    daemon.apiFetch.mockImplementation(detailFetch(detail({ status: "timed_out" }), NO_TAIL, []));

    await renderApp({ initialPath: "/runs/5" });

    const heading = await screen.findByText("ran in alpha");
    expect(heading.textContent).not.toMatch(/ended /);
    expect(heading.textContent).not.toContain("timed out");
  });
});

/* ------------------------------------------------------- what it cost us -- */

describe("RunDetail — the prompt budget", () => {
  it("shows what the daemon wrote and what it did not", async () => {
    daemon.apiFetch.mockImplementation(
      detailFetch(detail({ authored_prompt_estimate: 10_500, cli_own_estimate: 18_700 }), NO_TAIL, []),
    );

    await renderApp({ initialPath: "/runs/5" });

    // Both readings, both labelled `estimate` and neither labelled `tokens` —
    // it is four characters to the token and there is no tokenizer in this
    // product to make it anything better.
    expect(await screen.findByText(/10,500 estimate — what we wrote/)).toBeDefined();
    expect(screen.getByText(/18,700 estimate — the CLI's own/)).toBeDefined();
  });

  it("names the rest unknown when the run reported no tokens", async () => {
    daemon.apiFetch.mockImplementation(
      detailFetch(detail({ authored_prompt_estimate: 10_500, cli_own_estimate: null }), NO_TAIL, []),
    );

    await renderApp({ initialPath: "/runs/5" });

    // A residual of zero would read as *the CLI added nothing*, which is the
    // opposite of the truth about a run that simply never reported its usage.
    expect(await screen.findByText(/10,500 estimate — what we wrote/)).toBeDefined();
    expect(screen.getByText("the rest is unknown")).toBeDefined();
    expect(screen.queryByText(/estimate — the CLI's own/)).toBeNull();
  });

  it("says nothing was recorded for a run whose prompt it did not write", async () => {
    daemon.apiFetch.mockImplementation(
      detailFetch(detail({ authored_prompt_estimate: null, cli_own_estimate: null }), NO_TAIL, []),
    );

    await renderApp({ initialPath: "/runs/5" });

    expect(await screen.findByText("This run did not record what went into its prompt.")).toBeDefined();
    expect(screen.queryByText(/estimate — what we wrote/)).toBeNull();
  });
});
