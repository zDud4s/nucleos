// §spec autopilot-job-graph
import type { Job, JobItem } from "../data/fleet";

/**
 * A job's queue as the graph it actually is, rather than the list it is drawn as.
 *
 * `JobItemsPanel` renders `items` top to bottom in ordinal order, which throws away the one field
 * that says why the queue looks the way it does: `depends_on`. Two items with no dependency
 * between them can run at the same time and a list cannot show that; two items where one waits on
 * the other look identical to two that do not. The queue has been a DAG on the wire for as long as
 * `depends_on` has been on it — this module is the reading of it.
 *
 * Pure, and importable by a test that mounts nothing. Every judgement about what a status MEANS is
 * here rather than in a component, for the same reason `next_step` is split from the rest of
 * `job.rs`: the interesting questions ("is a job with one conflicted item still working?", "what
 * does the tally say when the gate went red on a retry?") are table tests with no DOM.
 */

/**
 * What a node is doing, for colour and for the tally.
 *
 * Seven, and they line up one-to-one with the seven state tones in `tokens.css` — which that file
 * calls "the app's whole colour vocabulary for *state*". That alignment is load-bearing rather
 * than tidy: `ui/state-map.ts` is emphatic that `cancelled` takes the `off` tone and NEVER the
 * failure tone, because it is "withdrawn work, not a verdict", and `core/src/job.rs` says the
 * same thing from its end — `Cancelled` is "you stopped it" against `Failed`'s "the work broke",
 * and `Skipped` is "not a failure and not a cancellation".
 *
 * An earlier draft of this had six and folded `cancelled` in with `failed`. On a graph that reads
 * at a glance it would have painted red the one thing two separate modules go out of their way to
 * say is not red.
 *
 * `todo` is the one with no tone at all, and that is right rather than a gap: an item nobody has
 * started is not in a state, it is waiting to be in one.
 */
export type Lifecycle =
  | "done"
  | "running"
  | "todo"
  | "waiting"
  | "gated"
  | "stopped"
  | "withdrawn";

export type NodeKind = "plan" | "item";

export interface ProgressNode {
  /** Stable across polls: the layout must not jump when a status changes. */
  id: string;
  kind: NodeKind;
  /** `ordinal`, for an item. Absent on the plan node. */
  ordinal: number | null;
  round: number;
  label: string;
  lifecycle: Lifecycle;
  /** The status verbatim, so nothing downstream has to trust the bucket. */
  status: string;
  /** What this node is doing, in words a person reads without a legend. */
  reading: string;
  runId: number | null;
  agentName: string | null;
  gateStatus: string | null;
  /** What its director said it would touch. Empty for a job without a team. */
  files: string[];
  /**
   * True when `status` cannot distinguish "going round again" from "finished red".
   *
   * `ItemState::GateRetriable` is NEVER STORED -- `job_items.status` says `gate_failed` either
   * way, and only the attempt count beside the job's budget separates them. That count is not on
   * the wire, so the shell genuinely cannot tell. Drawing such an item as stopped would be a lie
   * in the direction that matters: a queue that still owes it a run, shown as work that will never
   * happen.
   */
  undecided: boolean;
}

export interface ProgressEdge {
  from: string;
  to: string;
  /** `depends_on` edges are the director's; `queue` edges only say where a round began. */
  kind: "depends" | "queue";
}

export interface Round {
  round: number;
  nodes: ProgressNode[];
  /** True for the round `job.round` names -- the one the daemon is working through now. */
  current: boolean;
}

/**
 * The counts a progress read needs, which is a coarser question than `Lifecycle`.
 *
 * `attention` gathers `waiting`, `gated` and `stopped`: for "how far along is this", they are the
 * same answer -- not done, and not moving on their own. Which of the three it is stays on the node.
 */
export interface Tally {
  done: number;
  running: number;
  todo: number;
  attention: number;
  total: number;
}

export interface Progress {
  nodes: ProgressNode[];
  edges: ProgressEdge[];
  rounds: Round[];
  tally: Tally;
  /** One sentence for the collapsed view, where there is room for a line and not a graph. */
  reading: string;
}

const PLAN_ID = "plan";

/**
 * Every status `job_items.status` can hold, mapped once.
 *
 * Written as a table rather than a `switch` with a default, because a default is what let the four
 * team states read as "to do" for a whole release -- the comment in `FleetCanvas.itemReading` is
 * about exactly that bug. An unknown status here falls to `todo` too, but `statusIsKnown` makes it
 * visible instead of silent.
 */
const LIFECYCLE: Record<string, Lifecycle> = {
  pending: "todo",
  running: "running",
  // Ran clean; the gate has not measured it yet. In flight, not done.
  implemented: "running",
  merging: "running",
  // The merge landed, the gate went red, the job's branch was reset. It leaves for another
  // attempt or for `gate_failed` -- either way the queue is still holding it.
  reverted: "running",
  passed: "done",
  // Put down rather than failed, and not waiting on anybody: `Conflicted` "leaves for `Running` —
  // the resolution node, in that same tree", and `next_step` finds it by the same search as pending
  // work because "the item owes a run" (`core/src/job.rs`). The queue is still holding it, as with
  // `reverted`.
  conflicted: "running",
  // The item asked for a decision and the job moved on. Nothing broke and nobody stopped it.
  skipped: "waiting",
  gate_failed: "gated",
  gate_errored: "gated",
  failed: "stopped",
  // Withdrawn work, not a verdict — `state-map.ts` gives it the `off` tone and never the failure
  // one, and this bucket exists so that this drawing cannot disagree with the badges beside it.
  cancelled: "withdrawn",
  // A later round took this item's work over: set aside, not a verdict, and the item that
  // replaced it is the one to read.
  superseded: "withdrawn",
  // Never attempted, because something it depended on ended badly. Terminal, and a failure of the
  // job even though this item never ran.
  orphaned: "stopped",
};

const READING: Record<string, string> = {
  pending: "to do",
  running: "running",
  implemented: "written, not yet measured",
  merging: "merging into the job's branch",
  reverted: "merged, gate went red, branch reset",
  passed: "done",
  conflicted: "the merge hit a conflict — a run resolves it in the item's own tree",
  skipped: "put down: it asked for a decision",
  gate_failed: "the gate said no",
  gate_errored: "the gate could not run",
  failed: "failed",
  cancelled: "stopped by somebody",
  superseded: "taken over by a later round",
  orphaned: "never attempted — something it needed ended badly",
};

export function statusIsKnown(status: string): boolean {
  return status in LIFECYCLE;
}

export function lifecycleOf(status: string): Lifecycle {
  return LIFECYCLE[status] ?? "todo";
}

export function readingOf(status: string): string {
  return READING[status] ?? status;
}

/**
 * The plan node's own state, read off the job rather than off any item.
 *
 * There is no item for it: `plan` is a node of the job's sequence, and what it produced is the
 * queue. `planning` is the only status that means it is in flight; once items exist it is done,
 * and a job that ended with no items at all never got one.
 */
function planNode(job: Job, items: JobItem[]): ProgressNode {
  const running = job.status === "planning";
  const lifecycle: Lifecycle = running ? "running" : items.length > 0 ? "done" : "todo";
  return {
    id: PLAN_ID,
    kind: "plan",
    ordinal: null,
    round: 0,
    label: "plan",
    lifecycle,
    status: running ? "running" : items.length > 0 ? "passed" : "pending",
    reading: running
      ? "working out what to do"
      : items.length > 0
        ? `queued ${items.length} item${items.length === 1 ? "" : "s"}`
        : "nothing queued yet",
    runId: null,
    agentName: null,
    gateStatus: null,
    files: [],
    undecided: false,
  };
}

export function itemNode(item: JobItem): ProgressNode {
  const status = item.status;
  return {
    id: `item-${item.ordinal}`,
    kind: "item",
    ordinal: item.ordinal,
    round: item.round,
    label: item.description,
    lifecycle: lifecycleOf(status),
    status,
    reading: readingOf(status),
    runId: item.run_id,
    agentName: item.agent_name,
    gateStatus: item.gate_status,
    files: item.files,
    undecided: status === "gate_failed",
  };
}

/**
 * The queue as nodes and edges.
 *
 * **No `review` or `replan` node is drawn, and that is deliberate.** Both exist in the job's
 * sequence (`plan → implement×N → gate → review`, then a replan opens the next round), but nothing
 * on the wire says whether either ran: `RoundState::round_added_nothing` decides whether a round
 * gets a review at all, and neither it nor the replan verdict reaches the shell. A node drawn from
 * a guess would be indistinguishable from one drawn from a fact, and the whole point of this view
 * is that a person can trust what it says. What is missing is named in `reading` instead.
 */
export function buildProgress(job: Job, items: JobItem[]): Progress {
  const plan = planNode(job, items);
  const nodes: ProgressNode[] = [plan, ...items.map(itemNode)];
  const byOrdinal = new Map(items.map((item) => [item.ordinal, item]));

  const edges: ProgressEdge[] = [];
  for (const item of items) {
    // The director's edges. Only ordinals that are actually in the queue: a job whose plan was
    // rewritten can carry a dependency on an item that no longer exists, and an edge to a node
    // that is not drawn is a line into nothing.
    const parents = item.depends_on.filter((ordinal) => byOrdinal.has(ordinal));
    for (const ordinal of parents) {
      edges.push({ from: `item-${ordinal}`, to: `item-${item.ordinal}`, kind: "depends" });
    }
    // A root of its round hangs off the plan node, so the graph has one source and the layout has
    // somewhere to start. Roots of later rounds hang off it too: without the replan node -- which
    // cannot be drawn honestly -- there is nothing else for them to attach to.
    if (parents.length === 0) {
      edges.push({ from: PLAN_ID, to: `item-${item.ordinal}`, kind: "queue" });
    }
  }

  const numbers = [...new Set(items.map((item) => item.round))].sort((a, b) => a - b);
  const rounds: Round[] = numbers.map((round) => ({
    round,
    nodes: nodes.filter((node) => node.kind === "item" && node.round === round),
    current: round === job.round,
  }));

  return { nodes, edges, rounds, tally: tallyOf(nodes), reading: readingFor(job, items) };
}

/**
 * The tally counts ITEMS, never the plan node.
 *
 * "3 of 7 done" is a claim about the queue, and the plan node is not in the queue -- counting it
 * would make every job report one more piece of work than its planner found, and a job with an
 * empty queue would read `1/1 done` while having done nothing at all.
 */
export function tallyOf(nodes: ProgressNode[]): Tally {
  const items = nodes.filter((node) => node.kind === "item");
  const count = (life: Lifecycle) => items.filter((node) => node.lifecycle === life).length;
  return {
    done: count("done"),
    running: count("running"),
    todo: count("todo"),
    attention: count("waiting") + count("gated") + count("stopped") + count("withdrawn"),
    total: items.length,
  };
}

function readingFor(job: Job, items: JobItem[]): string {
  if (job.status === "planning") return "working out what to do";
  if (items.length === 0) return "its planner looked and found no work";

  const tally = tallyOf(items.map(itemNode));
  const parts = [`${tally.done}/${tally.total} done`];
  if (tally.running > 0) parts.push(`${tally.running} running`);
  if (tally.attention > 0) parts.push(`${tally.attention} needing a look`);
  // Rounds are only worth a word once there is more than one: every job has a round 0, and
  // "round 1 of 1" is noise on the great majority of them.
  if (job.max_rounds > 1) parts.push(`round ${job.round + 1} of ${job.max_rounds}`);
  return parts.join(" · ");
}
