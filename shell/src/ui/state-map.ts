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
 * **This table covers every domain whose states have been verified against
 * the núcleo.** Every §7 row landed across the slices that built each page,
 * each with its literals checked against the core rather than guessed — team
 * run, the last of them, lands with this slice, and the table is complete. An
 * unmapped state is rendered as itself — see `StateBadge` — because showing
 * the literal admits ignorance, while assigning it a tone would be a claim.
 */
export type StateDomain =
  | "run"
  | "job"
  | "gate"
  | "wait_reason"
  | "pillar"
  | "collision"
  | "slot"
  | "vcs"
  | "council"
  | "council_seat"
  | "errand"
  | "email_class"
  | "voice_cleanup"
  | "web_trust"
  | "web_extract"
  | "browser_refusal"
  | "team_run"
  | "team_item"
  | "team_action"
  | "autopilot";

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
   * What a project does without being asked (`AutopilotMode`, `core/src/autopilot.rs`).
   *
   * The three are not a severity scale and must not read as one. `off` is a
   * decision — nothing runs here, and that is fine; `shadow` is the project
   * *proposing* and a person deciding, which is where every project starts and
   * where many stay on purpose; `active` is the núcleo acting on its own. A UI
   * that drew `off` as a fault would nag about a setting somebody chose, and one
   * that drew `shadow` as success would hide that nothing has been promoted.
   */
  autopilot: {
    off: { tone: "off", label: "off" },
    shadow: { tone: "shadow", label: "shadow" },
    active: { tone: "active", label: "active" },
  },
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
   *
   * `disk` is the fourth, and until 2026-09-14 it was reported as the second: a
   * checkout refused because the volume is below the free-space floor parked its
   * job as `slot`. Somebody reading that waits for a run to finish, or goes
   * looking for one, and nothing clears until somebody frees space. Paused
   * rather than pending for that reason — like budget, it waits on a hand.
   */
  wait_reason: {
    budget: { tone: "paused", label: "held by budget" },
    slot: { tone: "pending", label: "waiting for a slot" },
    excluded: { tone: "paused", label: "held by an exclusion" },
    disk: { tone: "paused", label: "held by a full disk" },
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
   *
   * `HealthState` has four values, not two; `degraded` is working-and-impaired
   * and takes neither the healthy tone nor the failure tone, because a person
   * who reads it as `down` stops a pillar that is still serving and one who
   * reads it as `ok` ignores one that is about to stop.
   */
  pillar: {
    disabled: { tone: "off", label: "not configured" },
    down: { tone: "danger", label: "down" },
    ok: { tone: "active", label: "healthy" },
    degraded: { tone: "paused", label: "degraded" },
  },

  /**
   * A council's run status — `core/src/council.rs:23-26`.
   *
   * `running` gets the pending tone rather than an active one: nothing has been
   * decided yet, and drawing a deliberation in progress the same colour as a
   * settled one would tell a person to stop watching a card that still has
   * something to say. `cancelled` is withdrawn work, not a verdict, so it takes
   * the same quiet `off` every other cancellation in this table does.
   */
  council: {
    running: { tone: "pending", label: "deliberating" },
    done: { tone: "active", label: "settled" },
    error: { tone: "danger", label: "failed" },
    cancelled: { tone: "off", label: "cancelled" },
  },

  /**
   * One seat's status within one phase — `core/src/council.rs:28-34`. The same
   * six literals serve both `stage1_status` and `stage2_status`; a card reads
   * this table twice, once per stage.
   *
   * Two pairs the design's §7 will not let collapse. `timeout` is a seat that
   * ran out of time, not a seat that failed — it gets the held tone, never the
   * danger one, so a card full of timeouts reads as "the deadline was too
   * short" rather than "these models are broken". And `skipped` is a seat that
   * was never invited to vote in stage 2 at all — a different fact from
   * `cancelled`, which is a seat that was invited and then had the invitation
   * withdrawn when the run was called off. Both read as quiet and dismissed
   * (`off`), but with different words, because a person auditing a council
   * needs to be able to tell the two apart from the label alone.
   */
  council_seat: {
    pending: { tone: "pending", label: "waiting" },
    ok: { tone: "active", label: "answered" },
    timeout: { tone: "paused", label: "timed out" },
    error: { tone: "danger", label: "failed" },
    cancelled: { tone: "off", label: "cancelled" },
    skipped: { tone: "off", label: "not asked" },
  },

  /**
   * An errand's status — `core/src/errands.rs`, `Status::as_str`.
   *
   * `done` is a closed errand: the asking stopped, the row and its folder
   * stay. It takes neither the failure tone nor the completion tone, because
   * closing is an ending and not a verdict — an errand can be closed the
   * moment it starts and closed after months of real work, and both are the
   * same status.
   */
  errand: {
    active: { tone: "active", label: "answering" },
    paused: { tone: "paused", label: "paused" },
    done: { tone: "off", label: "closed" },
  },

  /**
   * A message's triage class — `core/src/triage.rs:253` `VALID_CLASSES`, plus
   * the terminal fifth one at `triage.rs:839,886`.
   *
   * `failed` is deliberately kept off `noise`'s tone: it is triage giving up
   * after repeated attempts to read the message at all, which is a fact about
   * *us*, not a judgement about the mail's content the way the other four are.
   * Reading it as noise would hide a message the machine never actually looked
   * at behind one it looked at and dismissed.
   */
  email_class: {
    urgent: { tone: "pending", label: "urgent" },
    action: { tone: "paused", label: "needs a reply, not today" },
    info: { tone: "info", label: "worth having seen" },
    noise: { tone: "off", label: "noise" },
    failed: { tone: "danger", label: "triage could not read this" },
  },

  /**
   * Whether a voice memo's transcript was cleaned up — `core/src/voice.rs:92-116`.
   *
   * `raw` and `shrunk` both leave the raw transcript on screen and must not
   * collapse into one reading: `raw` is nothing having been attempted (no
   * cleanup model armed, or it was unreachable), `shrunk` is a cleanup having
   * been produced and then REFUSED by a guard — a rewrite the guard judged as
   * having dropped too much, kept raw rather than trusted.
   */
  voice_cleanup: {
    cleaned: { tone: "active", label: "cleaned up" },
    raw: { tone: "off", label: "raw — no cleanup model armed" },
    shrunk: { tone: "paused", label: "cleanup refused by a guard — raw kept" },
  },

  /**
   * Whether a fetched page's text reaches an agent as written or only as a
   * summary — `core/src/web.rs`, `trust.rs:31-34`. The default is quarantine;
   * `raw` is the narrower, earned case (spec §5.2's conjunction), not the
   * common one.
   */
  web_trust: {
    raw: { tone: "active", label: "full text reached the agent" },
    quarantined: { tone: "paused", label: "summarised before reaching the agent" },
  },

  /**
   * How much of a fetched page's structure survived extraction —
   * `sidecars/web/extract/extract.go:28,32`. `fallback` is a SHAPE, not a
   * failure: an index or a dashboard has no article root to find, and lands
   * here legitimately.
   */
  web_extract: {
    article: { tone: "active", label: "read as an article" },
    fallback: { tone: "info", label: "read as a page, not an article" },
  },

  /**
   * Why `POST /browser/open` refused — `browser.rs:773-783`,
   * `browser_policy.rs:90-142`. Two are recoverable right where the refusal
   * happened and two are not, and the four literals must not blur into "the
   * browser said no": `no-one-present` is fixed by opening the shell,
   * `pillar-disabled` by turning the pillar on, and neither `reach-undesigned`
   * (the autonomous path is a seam, not a built road) nor `unparseable-url`
   * clears on its own from here.
   */
  browser_refusal: {
    "reach-undesigned": { tone: "info", label: "this reach is not built yet" },
    "no-one-present": { tone: "pending", label: "open the shell to continue" },
    "pillar-disabled": { tone: "off", label: "the browser pillar is not enabled" },
    "unparseable-url": { tone: "danger", label: "that url could not be read" },
  },

  /**
   * A department's run — `core/src/team.rs:38` and `:45`, asserted exhaustive
   * by `every_state_of_the_machine_is_live_or_terminal_and_never_both`.
   *
   * §7's row, and the daemon wrote the argument for it in the same place it
   * wrote the states: `stopped` and `expired` are not failures. One is a money
   * ceiling reached, the other the run's four-hour one, and "an owner shown
   * `failed` goes looking for an error that does not exist". Neither takes the
   * danger tone, and neither says the word.
   */
  team_run: {
    planning: { tone: "pending", label: "planning" },
    working: { tone: "active", label: "working" },
    delivering: { tone: "active", label: "delivering" },
    done: { tone: "active", label: "delivered" },
    stopped: { tone: "paused", label: "stopped at a ceiling" },
    expired: { tone: "paused", label: "ran out of time" },
    failed: { tone: "danger", label: "failed" },
    cancelled: { tone: "off", label: "cancelled" },
  },

  /** One item of one round — the four states `team.rs` writes for `team_items`. */
  team_item: {
    pending: { tone: "pending", label: "not started" },
    running: { tone: "active", label: "running" },
    done: { tone: "active", label: "done" },
    failed: { tone: "danger", label: "failed" },
  },

  /**
   * What became of something a department asked the core to do.
   *
   * `rejected` is not a state the daemon stores. A refused action is written
   * `state = 'failed', error = 'rejected'`, and `teamActionState` in
   * `data/teams.ts` is what turns that pair back into this key — the second
   * half of §7's team row, that a human decision is not an execution result.
   * `pending` is deliberately neutral: an action may be pending because
   * somebody has not answered, or because the grant was `allow` and nobody has
   * to. Which of the two it is comes from `proposal_id`, not from here.
   */
  team_action: {
    pending: { tone: "pending", label: "not carried out yet" },
    working: { tone: "active", label: "being carried out" },
    done: { tone: "active", label: "carried out" },
    failed: { tone: "danger", label: "failed" },
    rejected: { tone: "off", label: "refused by you" },
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
  /**
   * A NULL `triage_class` (`core/src/email.rs:791`) is a message triage has
   * not reached yet, not a message that was read and found to be nothing —
   * that second fact is `noise`, a real class, and the two must not share a
   * badge.
   */
  email_class: { tone: "info", label: "not triaged yet" },
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
