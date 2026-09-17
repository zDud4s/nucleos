import { describe, expect, it } from "vitest";
import type { Job, JobItem } from "../data/fleet";
import {
  buildProgress,
  itemNode,
  lifecycleOf,
  readingOf,
  statusIsKnown,
  tallyOf,
} from "./job-progress";

/**
 * Every variant of `ItemState` in `core/src/job.rs`, by the string `job_items.status` stores.
 *
 * `GateRetriable` is absent ON PURPOSE and is not an oversight: that state is never stored, and
 * the row says `gate_failed` for it. Held here so a new variant in the núcleo cannot quietly
 * arrive as "to do" -- which is exactly what happened to the four team states, and is the bug
 * `FleetCanvas.itemReading`'s comment was written about.
 */
const EVERY_STORED_STATUS = [
  "pending",
  "running",
  "implemented",
  "passed",
  "failed",
  "cancelled",
  "gate_failed",
  "gate_errored",
  "skipped",
  "merging",
  "conflicted",
  "reverted",
  "orphaned",
  "superseded",
];

function job(over: Partial<Job> = {}): Job {
  return {
    id: 1,
    project_id: "nucleos",
    rule_name: null,
    status: "running",
    wait_reason: null,
    max_items: 8,
    created_at: "2026-09-01T00:00:00Z",
    completed_at: null,
    slot: 0,
    round: 0,
    max_rounds: 1,
    team_id: null,
    team_name: null,
    team_max_parallel: null,
    ...over,
  } as Job;
}

function item(over: Partial<JobItem> = {}): JobItem {
  return {
    ordinal: 0,
    description: "do the thing",
    status: "pending",
    round: 0,
    run_id: null,
    gate_status: null,
    agent_id: null,
    agent_name: null,
    depends_on: [],
    files: [],
    ...over,
  };
}

describe("job-progress — the status table", () => {
  it("knows every status the núcleo can store", () => {
    const unknown = EVERY_STORED_STATUS.filter((status) => !statusIsKnown(status));
    expect(unknown).toEqual([]);
  });

  it("gives every stored status a reading of its own", () => {
    const readings = EVERY_STORED_STATUS.map(readingOf);
    expect(new Set(readings).size).toBe(EVERY_STORED_STATUS.length);
  });

  it("never lets a team state read as work not yet begun", () => {
    // The historical bug, pinned: all four fell to the default and read "to do", and the worst of
    // them is `conflicted` -- an item the queue owes a resolution run, shown as work nobody has
    // started.
    for (const status of ["merging", "conflicted", "reverted", "orphaned"]) {
      expect(lifecycleOf(status)).not.toBe("todo");
    }
  });

  it("draws a conflicted item as work the queue still owes, not as a question for a person", () => {
    // `core/src/job.rs`: `Conflicted` leaves for `Running`, the resolution node, and `next_step`
    // finds it as work. Filing it under "waiting on a person" sent the reader to a queue that
    // holds nothing for them.
    expect(lifecycleOf("conflicted")).toBe("running");
    expect(readingOf("conflicted")).not.toMatch(/person/);
  });

  it("keeps stopped-by-somebody apart from broke-on-its-own in the reading", () => {
    expect(readingOf("cancelled")).not.toBe(readingOf("failed"));
    expect(readingOf("skipped")).not.toBe(readingOf("cancelled"));
  });

  it("never paints withdrawn work as a failure", () => {
    // `state-map.ts` gives `cancelled` the `off` tone and NEVER the failure tone — "withdrawn
    // work, not a verdict". A lifecycle that folded the two would make this graph contradict
    // every badge beside it. The first draft did exactly that.
    expect(lifecycleOf("cancelled")).toBe("withdrawn");
    expect(lifecycleOf("failed")).toBe("stopped");
  });

  it("keeps a gate verdict apart from work that broke", () => {
    // The gate held it; the item did not break. `paused` against `danger` in the tone layer.
    expect(lifecycleOf("gate_failed")).toBe("gated");
    expect(lifecycleOf("gate_errored")).toBe("gated");
    expect(lifecycleOf("gate_failed")).not.toBe(lifecycleOf("failed"));
  });

  it("counts a not-yet-measured item as in flight, not as done", () => {
    // `Implemented` is "the run finished cleanly but the gate has not measured it yet".
    expect(lifecycleOf("implemented")).toBe("running");
    expect(lifecycleOf("passed")).toBe("done");
  });

  it("treats a reverted item as still held by the queue", () => {
    // It leaves for another attempt or for `gate_failed`; either way the job is not finished
    // with it, and "stopped" would be wrong in the direction that hides live work.
    expect(lifecycleOf("reverted")).toBe("running");
  });
});

describe("job-progress — the gate's undecidable status", () => {
  it("marks a gate_failed item as undecided", () => {
    // `GateRetriable` is never stored and the attempt count is not on the wire, so this item may
    // be going round again or may be finished red. The view must not pick one.
    expect(itemNode(item({ status: "gate_failed" })).undecided).toBe(true);
  });

  it("marks nothing else as undecided", () => {
    const undecided = EVERY_STORED_STATUS.filter((status) => itemNode(item({ status })).undecided);
    expect(undecided).toEqual(["gate_failed"]);
  });
});

describe("job-progress — the graph", () => {
  it("turns depends_on into edges", () => {
    const progress = buildProgress(job(), [
      item({ ordinal: 0 }),
      item({ ordinal: 1, depends_on: [0] }),
    ]);
    expect(progress.edges).toContainEqual({ from: "item-0", to: "item-1", kind: "depends" });
  });

  it("hangs a root off the plan node so the graph has one source", () => {
    const progress = buildProgress(job(), [item({ ordinal: 0 })]);
    expect(progress.edges).toContainEqual({ from: "plan", to: "item-0", kind: "queue" });
  });

  it("does not hang a dependent item off the plan node as well", () => {
    const progress = buildProgress(job(), [
      item({ ordinal: 0 }),
      item({ ordinal: 1, depends_on: [0] }),
    ]);
    expect(progress.edges.filter((edge) => edge.to === "item-1")).toHaveLength(1);
  });

  it("drops a dependency on an ordinal that is not in the queue", () => {
    // A rewritten plan can leave a dependency pointing at an item that no longer exists. An edge
    // to a node nothing draws is a line into empty space.
    const progress = buildProgress(job(), [item({ ordinal: 1, depends_on: [99] })]);
    expect(progress.edges.some((edge) => edge.from === "item-99")).toBe(false);
    // ...and with no surviving parent it becomes a root, rather than disappearing from the graph.
    expect(progress.edges).toContainEqual({ from: "plan", to: "item-1", kind: "queue" });
  });

  it("groups items by round and marks the one the job is on", () => {
    const progress = buildProgress(job({ round: 1, max_rounds: 3 }), [
      item({ ordinal: 0, round: 0 }),
      item({ ordinal: 1, round: 1 }),
    ]);
    expect(progress.rounds.map((round) => round.round)).toEqual([0, 1]);
    expect(progress.rounds.map((round) => round.current)).toEqual([false, true]);
  });

  it("draws no review or replan node", () => {
    // Neither is observable from the wire: `round_added_nothing` decides whether a round gets a
    // review at all, and it does not reach the shell. A guessed node reads exactly like a
    // measured one.
    const progress = buildProgress(job({ max_rounds: 3 }), [item({ ordinal: 0 })]);
    expect(progress.nodes.map((node) => node.kind).sort()).toEqual(["item", "plan"]);
  });
});

describe("job-progress — the plan node", () => {
  it("is running while the job is planning", () => {
    const progress = buildProgress(job({ status: "planning" }), []);
    expect(progress.nodes[0].lifecycle).toBe("running");
  });

  it("is done once items exist", () => {
    const progress = buildProgress(job(), [item()]);
    expect(progress.nodes[0].lifecycle).toBe("done");
  });

  it("is not counted in the tally", () => {
    // "3 of 7" is a claim about the queue. Counting the plan node would make an empty job read
    // 1/1 done while having done nothing.
    const empty = buildProgress(job({ status: "completed" }), []);
    expect(empty.tally.total).toBe(0);
    expect(empty.tally.done).toBe(0);
  });
});

describe("job-progress — the tally", () => {
  it("gathers waiting, gated and stopped into attention", () => {
    const tally = tallyOf(
      [
        item({ ordinal: 0, status: "skipped" }),
        item({ ordinal: 1, status: "gate_failed" }),
        item({ ordinal: 2, status: "failed" }),
        item({ ordinal: 3, status: "passed" }),
      ].map(itemNode),
    );
    expect(tally).toEqual({ done: 1, running: 0, todo: 0, attention: 3, total: 4 });
  });
});

describe("job-progress — the one-line reading", () => {
  it("says what a planning job is doing", () => {
    expect(buildProgress(job({ status: "planning" }), []).reading).toBe("working out what to do");
  });

  it("says when the planner found nothing, which is not a failure", () => {
    expect(buildProgress(job({ status: "completed" }), []).reading).toContain("found no work");
  });

  it("counts done out of total, and names live work", () => {
    const progress = buildProgress(job(), [
      item({ ordinal: 0, status: "passed" }),
      item({ ordinal: 1, status: "running" }),
    ]);
    expect(progress.reading).toContain("1/2 done");
    expect(progress.reading).toContain("1 running");
  });

  it("stays quiet about rounds on a job that has only one", () => {
    // Every job has a round 0; "round 1 of 1" is noise on the great majority of them.
    const progress = buildProgress(job({ max_rounds: 1 }), [item()]);
    expect(progress.reading).not.toContain("round");
  });

  it("names the round once a job may run more than one", () => {
    const progress = buildProgress(job({ round: 1, max_rounds: 4 }), [item({ round: 1 })]);
    expect(progress.reading).toContain("round 2 of 4");
  });
});
