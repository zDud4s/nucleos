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
 * the núcleo.** It is deliberately incomplete: the remaining §7 rows (collision,
 * slot, council, team run, browser, e-mail, voice, web, VCS) arrive with the
 * slices that build those pages, each with its literals checked against the
 * core rather than guessed. An unmapped state is rendered as itself — see
 * `StateBadge` — because showing the literal admits ignorance, while assigning
 * it a tone would be a claim.
 */
export type StateDomain = "run" | "job" | "gate" | "wait_reason" | "pillar";

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
   */
  wait_reason: {
    budget: { tone: "paused", label: "held by budget" },
    slot: { tone: "pending", label: "waiting for a slot" },
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
