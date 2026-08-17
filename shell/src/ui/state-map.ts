import type { BadgeTone } from "./Badge";

/**
 * The states that must never collapse, in one table.
 *
 * The design's §7 is a contract about *rendering*: there are pairs of states in
 * this system that mean opposite things and that a careless UI merges into one
 * red pill. A gate that could not run is not a gate that failed — one says the
 * measurement did not happen and the other says the code is broken, and a
 * person who reads the first as the second stops the wrong work. A job that was
 * cancelled is not a job that failed. A run interrupted by the núcleo crashing
 * under it is not a run that exited non-zero.
 *
 * Putting the mapping in a pure table rather than in each page's JSX is what
 * makes those distinctions testable without rendering anything, and what stops
 * the fourteenth page from quietly picking a different colour for `expired`.
 *
 * **This table covers only the domains whose states have been verified against
 * the núcleo.** It is deliberately incomplete: the remaining §7 rows (council,
 * team run, browser, e-mail, voice, web) arrive with the slices that build
 * those pages, each with its literals checked against the core rather than
 * guessed. An unmapped state is rendered as itself — see `StateBadge` — because
 * showing the literal admits ignorance, while assigning it a tone would be a
 * claim.
 */
export type StateDomain =
  | "run"
  | "job"
  | "gate"
  | "wait_reason"
  | "pillar"
  | "collision"
  | "slot"
  | "vcs";

export interface StateReading {
  tone: BadgeTone;
  label: string;
}

/**
 * State literals as the núcleo writes them, mapped to a tone and a sentence.
 *
 * Every key below was read out of `core/src/` rather than inferred from a name.
 */
const READINGS: Record<StateDomain, Record<string, StateReading>> = {
  /**
   * Run outcomes. The four that §7 forbids merging.
   *
   * `interrupted` is the núcleo dying underneath a run — a defect in *us*, and
   * the run may well have been fine. It gets the held tone, not the failure
   * tone, so that a screen full of interruptions reads as "the daemon
   * restarted" and sends you to look at the daemon.
   */
  run: {
    interrupted: { tone: "paused", label: "interrupted" },
    failed: { tone: "danger", label: "failed" },
    cancelled: { tone: "off", label: "cancelled" },
    awaiting_approval: { tone: "pending", label: "awaiting approval" },
  },

  /**
   * Job outcomes.
   *
   * `stopped` is a person or a rule halting the chain; `expired` is the window
   * closing on it; `cancelled` is the request being withdrawn. None of the
   * three is a failure and none of the three is a completion, so none of them
   * borrows either tone.
   */
  job: {
    completed: { tone: "active", label: "completed" },
    stopped: { tone: "off", label: "stopped" },
    expired: { tone: "paused", label: "expired" },
    cancelled: { tone: "off", label: "cancelled" },
  },

  /**
   * The gate — three outcomes out of three, never two.
   *
   * Two spellings for the same third state, and both are real: `job_items.status`
   * records `gate_errored` while `job_items.gate_status` records plain `errored`
   * (`core/src/job.rs`, `step_after_gate`). Whichever column a caller happens to
   * hold, it must not read as a failure.
   */
  gate: {
    passed: { tone: "active", label: "gate passed" },
    failed: { tone: "danger", label: "gate failed" },
    errored: { tone: "info", label: "gate not measured" },
    gate_errored: { tone: "info", label: "gate not measured" },
  },

  /**
   * Why a job is parked. Budget and slot contention ask for opposite answers —
   * one wants you to raise a ceiling, the other wants you to wait or to stop
   * something else — so they never share a tone or a sentence.
   *
   * `excluded` is the third, and it is neither: the job is not short of money
   * and not short of a slot. A rule somebody approved says it may not run while
   * its partner does (`core/src/job.rs`, `Brake::Park { reason: "excluded" }`),
   * and the only thing that changes it is lifting the rule or letting the
   * partner finish. Reading it as slot contention would send somebody looking
   * for capacity that is already there.
   */
  wait_reason: {
    budget: { tone: "paused", label: "held by budget" },
    slot: { tone: "pending", label: "waiting for a slot" },
    excluded: { tone: "paused", label: "held by an exclusion" },
  },

  /**
   * What is known about a coincidence between two trees.
   *
   * `not_measured` exists so that `clean` is never said in vain, and it is the
   * one row on this table that can do active damage if collapsed: somebody
   * trusting a `clean` nobody computed lets two jobs run at the same file. It
   * takes the informational tone for the same reason the errored gate does —
   * a fact with no verdict attached is not a verdict.
   *
   * Which *source* said it — declared or observed — is not in here. That is one
   * distinction up: §7 asks for two badges, and a card renders one of these per
   * source with the source named beside it.
   */
  collision: {
    collide: { tone: "danger", label: "trees overlap" },
    clean: { tone: "active", label: "no overlap" },
    not_measured: { tone: "info", label: "overlap not measured" },
  },

  /**
   * What is known about a slot's owner, when the answer is *not much*.
   *
   * Two entries, and they are the whole row: a listing that failed or came back
   * full is ordinary and reads as *detail unavailable*; a listing that answered
   * in full without the owner in it is a **leaked slot**, waiting on
   * `reconcile_orphaned_slots`, and it is a defect. Collapsing the two teaches
   * the reader to ignore the second — which is exactly the one worth seeing,
   * because it silently lowers a project's effective ceiling.
   *
   * A slot whose owner *is* described has no reading here: the card shows the
   * job or the run, which is a better answer than a badge.
   */
  slot: {
    unknown: { tone: "info", label: "detail unavailable" },
    orphaned: { tone: "danger", label: "awaiting reconciliation" },
  },

  /**
   * A request in the git queue.
   *
   * Two distinctions, both load-bearing. `blocked` is terminal but **not** a
   * failure: the queue will not retry it, and the answer is to fix the tree and
   * submit again — so it takes the held tone rather than the red one. And
   * `escalated` is a *normal outcome*: a person owns the conflict now, which is
   * the queue working, not the queue breaking. Dressing either as `failed`
   * sends somebody to debug a merge that behaved exactly as designed.
   */
  vcs: {
    succeeded: { tone: "active", label: "landed" },
    failed: { tone: "danger", label: "failed" },
    blocked: { tone: "paused", label: "blocked — submit it again" },
    escalated: { tone: "pending", label: "escalated to you" },
    rejected: { tone: "off", label: "rejected" },
    cancelled: { tone: "off", label: "cancelled" },
    interrupted: { tone: "paused", label: "interrupted" },
  },

  /**
   * A pillar's health. `disabled` is a pillar nobody asked for and `down` is a
   * pillar that is broken; a shell that shows the first as the second invents an
   * outage, and one that shows the second as the first hides one.
   */
  pillar: {
    disabled: { tone: "off", label: "not configured" },
    down: { tone: "danger", label: "down" },
  },
};

/**
 * What an *absent* value means, per domain.
 *
 * Absent is not zero and it is not failure. A NULL `gate_status` means no gate
 * was ever configured for this project — there is nothing to have passed or
 * failed. This is the row §7 calls out most sharply, because "no gate" rendered
 * in red is the UI telling you your tests broke when you never wrote any.
 *
 * Domains missing from this table have no reading for absence, and `StateBadge`
 * renders nothing at all rather than a badge that says "none".
 */
const ABSENT: Partial<Record<StateDomain, StateReading>> = {
  gate: { tone: "off", label: "no gate configured" },
};

/**
 * Read a state, or admit there is no reading for it.
 *
 * `null` out means "this table has nothing to say", which is a different answer
 * from a tone — callers are expected to show the raw literal rather than
 * substitute a guess.
 *
 * The lookup lowercases because the same state travels under two casings: the
 * núcleo's Rust enums are `Disabled`/`Down` and their JSON is
 * `disabled`/`down`. Both spellings name one state, and a badge that resolves
 * only one of them fails exactly when someone reads a value off a Rust type.
 */
export function readState(domain: StateDomain, state: string | null | undefined): StateReading | null {
  if (state === null || state === undefined || state.trim() === "") {
    return ABSENT[domain] ?? null;
  }
  return READINGS[domain][state.trim().toLowerCase()] ?? null;
}
