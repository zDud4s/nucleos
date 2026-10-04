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
 * Every domain backed by Rust literals is checked by
 * `state-map-completeness.test.ts`, which reads those literals rather than
 * trusting this file's claim. An unmapped state is rendered as itself — see
 * `StateBadge` — because showing the literal admits ignorance, while assigning
 * it a tone would be a claim.
 * A domain the núcleo does not write is allowed here only when its own docstring says so.
 */
export type StateDomain =
  | "run"
  | "job"
  | "gate"
  | "wait_reason"
  | "quota"
  | "pillar"
  | "collision"
  | "slot"
  | "vcs"
  | "council"
  | "council_seat"
  | "email_class"
  | "voice_cleanup"
  | "web_trust"
  | "web_extract"
  | "browser_refusal"
  | "team_run"
  | "team_item"
  | "team_action"
  | "job_item"
  | "collision_source"
  | "exclusion"
  | "autopilot"
  | "brake"
  | "setting"
  | "machine_file"
  | "credential"
  | "department"
  | "feed"
  | "rule"
  | "folder"
  | "knowledge";

export interface StateReading {
  tone: BadgeTone;
  label: string;
  /**
   * Nothing has decided this yet — the badge is drawn dashed and unfilled.
   *
   * A verdict that has not been reached is not a verdict, and giving it a solid
   * badge in any of the seven tones says the opposite. Dashed is what this
   * system already uses for "provisional or empty" (DESIGN.md, Shapes), and it
   * is what separates `email_class: null` from `noise`: both are quiet, but one
   * was read and dismissed and the other has not been read at all.
   */
  provisional?: true;
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
  /** A brake is a switch, not workload: released is switched off on purpose; not_read is wired to nothing. */
  brake: {
    held: { tone: "paused", label: "held" },
    released: { tone: "off", label: "released" },
    not_read: { tone: "off", label: "not read" },
  },
  /** A setting that is on is a stated fact, never work in flight; off is not a held rule. */
  setting: {
    enabled: { tone: "info", label: "enabled" },
    disabled: { tone: "off", label: "disabled" },
    armed: { tone: "info", label: "armed" },
    unarmed: { tone: "off", label: "unarmed" },
    disarmed: { tone: "off", label: "disarmed" },
  },
  /**
   * Whether one of this machine's settings files exists (`MachineSetting.exists`, served by
   * `core/src/machine_config.rs`). The núcleo sends a boolean, so the two words are this shell's
   * derivation, allowed by this docstring. Never configured is a switch nobody set, not a fault.
   */
  machine_file: {
    configured: { tone: "info", label: "configured" },
    unconfigured: { tone: "off", label: "never configured" },
  },
  /**
   * Whether a credential is in the store (`MachineSecret.present`: true, false, or null when the
   * store could not be asked). A shell derivation, allowed by this docstring. `unknown` is amber
   * and not grey: unlike `unset`, which wants a credential pasted, it wants somebody to look at the
   * store.
   */
  credential: {
    set: { tone: "info", label: "set" },
    unset: { tone: "off", label: "not set" },
    unknown: { tone: "pending", label: "could not be asked" },
  },
  /** The núcleo does not write department states: this shell derivation keeps Teams and Bench in one vocabulary. */
  department: {
    working: { tone: "active", label: "at work" },
    waiting: { tone: "pending", label: "waiting" },
    idle: { tone: "off", label: "idle" },
  },
  /**
   * Every `kind` the núcleo writes into the feed, mapped to a reading.
   *
   * The enumeration is not this file's claim any more: `state-map-completeness.test.ts` reads the
   * kind argument at every call of `feed::append` / `append_on` and of the
   * two wrappers that forward a caller's kind (`job.rs::say`, `notify.rs::deliver_or_defer`)
   * across `core/src`, outside the test modules, and fails on a difference in either direction.
   * It went in at 46 rows and found eighteen kinds the núcleo writes and this table did not read —
   * the two the Teams pillar writes most among them — each of which had been rendering
   * `.ui-state-unmapped`, the device for a word the shell has never heard of.
   *
   * The one exclusion is `email_urgent`: `triage.rs:835` builds `format!("email_{}", class)` from
   * a project's configured notify classes, so the shell can read the class everybody has and not
   * everybody's classes.
   *
   * This domain spends no Acting Green: a feed line is written once and never refreshed, so a
   * fact that may have ended hours ago cannot claim work is executing now.
   *
   * Three kinds are ONE kind for two or three outcomes, and their summary is the only carrier of
   * which one happened — `team_action`, `team_run_finished` and `command_finished`. Each is a fact
   * here, with the verdict left to the sentence; splitting them is a change in `core/`, and is a
   * named follow-up for the owner rather than a thing the shell may guess at.
   */
  feed: {
    // Jobs.
    job_started: { tone: "info", label: "job started" },
    job_planned: { tone: "info", label: "job planned" },
    job_replanned: { tone: "info", label: "job replanned" },
    job_plan_failed: { tone: "danger", label: "job could not be planned" },
    job_item_failed: { tone: "danger", label: "job item failed" },
    // Held Ember, as `job_item.conflicted` is: the item is put down, not failed, and one event
    // keeps one tone on both pages — the same reasoning `reverted` follows.
    job_item_conflicted: { tone: "paused", label: "job item did not merge" },
    job_item_orphaned: { tone: "off", label: "job item never attempted" },
    job_gate_failed: { tone: "danger", label: "job gate failed" },
    // A round in which no item passed has nothing for a review to judge, so none runs (job 27,
    // 2026-09-14: a review read a reverted tree and reported "no work was done"). A fact, not a
    // failure: the red items already said so.
    job_review_skipped: { tone: "info", label: "job review skipped" },
    // A review that never reached the API is run once more (job 26, 2026-09-13: a DNS outage ended
    // it and the next round opened without a verdict). Stated Blue and not amber: the verdict it
    // stands for is still to come, but it comes from the job, not from the reader — the same
    // argument `job_waiting` makes below. The sequence stays open (`lanes.ts`) until it lands.
    job_review_retried: { tone: "info", label: "job review retried" },
    // Stated Blue, not Awaiting-You Amber. `job.rs::brakes` parks a job for exactly seven reasons —
    // `kill-switch`, `budget`, `quota`, `excluded`, `attention`, `slot` and `disk` (`park` writes the line) — and
    // none of them is a question put to the reader: an approval is `awaiting_approval`, a status
    // and not a park. Amber on every parked job taught the Feed to summon somebody for a slot that
    // frees itself. The verdict, where there is one, is the `wait_reason` badge beside it.
    job_waiting: { tone: "info", label: "job waiting" },
    job_finished: { tone: "info", label: "job finished" },
    job_failed: { tone: "danger", label: "job failed" },
    job_stopped: { tone: "off", label: "job stopped" },
    job_cancelled: { tone: "off", label: "job cancelled" },
    job_expired: { tone: "paused", label: "job expired" },
    job_interrupted: { tone: "paused", label: "job interrupted" },
    // Runs.
    run_retry: { tone: "info", label: "run retried" },
    run_failed_final: { tone: "danger", label: "run failed for good" },
    run_interrupted: { tone: "paused", label: "run interrupted" },
    run_stopped_probing: { tone: "danger", label: "run stopped after repeated refusals" },
    // The resolver stopped a run (spec .ai/specs/2026-09-27-autopilot-juiz-resolve-bloqueios-design.md, E1): same reading as a run stopped after refusals.
    run_stopped_by_judge: { tone: "danger", label: "run stopped by the judge" },
    resume_did_not_act: { tone: "info", label: "approved action never attempted" },
    shadow_run_completed: { tone: "shadow", label: "shadow run completed" },
    worktree_run_completed: { tone: "info", label: "worktree run completed" },
    token_efficiency: { tone: "info", label: "efficiency observation" },
    // A provider's usage window crossed one of the owner's thresholds (`core/src/quota.rs`, design
    // D11). Held Ember and not Stated Blue: unlike a slot, this one waits on a hand — the reader
    // decides whether to spend the rest of the window, and nothing here frees itself before the
    // reset. The same argument budget makes in `wait_reason`, and this is the same kind of ceiling.
    quota_warning: { tone: "paused", label: "quota threshold crossed" },
    // The resolver fell back to observe (spec .ai/specs/2026-09-27-autopilot-juiz-resolve-bloqueios-design.md): the authorisation dropped and waits on reviews, like a quota warning.
    judge_resolve_demoted: { tone: "paused", label: "resolver back to observe" },
    quota_blind: { tone: "info", label: "quota brake ran blind" },
    // Worktrees.
    worktree_gate_failed: { tone: "danger", label: "worktree gate failed" },
    worktree_provision_failed: { tone: "danger", label: "worktree could not be made" },
    worktree_workflow_missing: { tone: "danger", label: "worktree has no workflow" },
    worktree_released: { tone: "off", label: "worktree released" },
    worktree_branch_kept: { tone: "info", label: "unmerged branch kept" },
    worktree_removed: { tone: "off", label: "worktree removed" },
    worktree_gc_failed: { tone: "danger", label: "worktree cleanup failed" },
    // The resolver (spec .ai/specs/2026-09-27-autopilot-juiz-resolve-bloqueios-design.md, D7). Its
    // own lines, after today's: `worktree_gate_failed` is never edited. `judge_needs_owner` waits
    // on a hand — the reader decides what the failed gate or the refused action needs — so it is
    // Awaiting-You Amber; a correction started is a fact; a correction that did not finish is a
    // failure of the automatic turn.
    judge_needs_owner: { tone: "pending", label: "the judge hands this to you" },
    judge_correction_started: { tone: "info", label: "correction started" },
    judge_correction_failed: { tone: "danger", label: "correction did not finish" },
    // Git.
    vcs_request_finished: { tone: "info", label: "git request settled" },
    vcs_request_cancelled: { tone: "off", label: "git request cancelled" },
    vcs_request_interrupted: { tone: "paused", label: "git request interrupted" },
    // Settled: the request stopped wanting a person (merged by hand, superseded, dismissed, its
    // branch gone). Closes the request's sequence, so a resolution started before it stops reading
    // as still going.
    vcs_request_settled: { tone: "info", label: "git request no longer needs you" },
    vcs_resolution_started: { tone: "info", label: "conflict resolution started" },
    vcs_resolution_cancelled: { tone: "off", label: "conflict resolution stopped" },
    vcs_resolution_discarded: { tone: "danger", label: "resolution discarded changes" },
    land_resolution_failed: { tone: "danger", label: "resolution could not be landed" },
    // Teams.
    team_run_started: { tone: "info", label: "team run started" },
    team_run_finished: { tone: "info", label: "team run settled" },
    team_item_dropped: { tone: "off", label: "team item dropped" },
    team_action: { tone: "info", label: "team action settled" },
    team_trigger_armed: { tone: "info", label: "team trigger armed" },
    team_trigger_skipped: { tone: "paused", label: "team trigger did not fire" },
    // Council.
    council_started: { tone: "info", label: "council started" },
    council_stage: { tone: "info", label: "council stage" },
    council_finished: { tone: "info", label: "council settled" },
    // Schedules.
    schedule_rule_invalid: { tone: "danger", label: "schedule rule invalid" },
    // Mail.
    email_digest: { tone: "info", label: "e-mail digest" },
    email_urgent: { tone: "pending", label: "urgent e-mail" },
    email_triage_failed: { tone: "danger", label: "e-mail triage failed" },
    email_triage_paused: { tone: "paused", label: "e-mail triage paused" },
    email_triage_stalled: { tone: "paused", label: "e-mail triage stalled" },
    email_fetch_skipped: { tone: "info", label: "e-mail skipped" },
    email_sent_mailbox_foreign: { tone: "danger", label: "sent mail filed elsewhere" },
    // Project settings and machine lines.
    config_written: { tone: "info", label: "project file written" },
    // A project brought under NucleOS: by a person on the onboarding panel, or at startup for one
    // that passed the old check (`onboarding.rs`). A record, like the write beside it.
    project_onboarded: { tone: "info", label: "project onboarded" },
    // A breached health readout, recorded for review and nothing more (`health.rs`). Pending,
    // because it is a thing somebody is asked to look at; nothing was started on its account.
    health_breach_intent: { tone: "pending", label: "health breach recorded" },
    workflow_changed: { tone: "info", label: "workflow changed" },
    command_finished: { tone: "info", label: "project command finished" },
    action_authorized: { tone: "info", label: "action authorised by a grant" },
    proposal_record_failed: { tone: "danger", label: "proposal not recorded" },
    promotion_ready: { tone: "pending", label: "promotion ready" },
    judge_demoted: { tone: "paused", label: "judge back to observing" },
    // A credential set or forgotten from the app. The line names the key and never the value.
    secret_stored: { tone: "info", label: "credential set" },
    secret_forgotten: { tone: "info", label: "credential forgotten" },
    "web.read": { tone: "info", label: "web page read" },
  },
  /** Rule settings are shell derivations: armed is stated, capped is a ceiling, and never-fires is a fault. */
  rule: { armed: { tone: "info", label: "armed" }, "never-fires": { tone: "danger", label: "never fires" }, capped: { tone: "paused", label: "capped today" }, unseen: { tone: "info", label: "no commit seen yet" } },
  /** Folder facts are derived by the shell; an unnamed folder is off, while a missing named one is a fault.
   * A healthy folder is the absence of a fact, so `ok` is absent rather than a map row nobody renders.
   */
  folder: { missing: { tone: "danger", label: "gone" }, unset: { tone: "off", label: "not named" } },
  /** The four kinds are facts, not a severity scale, so all four use Stated Blue. */
  knowledge: { prompt: { tone: "info", label: "instruction" }, memory: { tone: "info", label: "fact" }, skill: { tone: "info", label: "how-to" }, subagent: { tone: "info", label: "delegation" } },
  /**
   * Run outcomes. `concurrency.rs`'s `LIVE_RUN_STATUSES` and `runs.rs`'s
   * `TERMINAL_RUN_STATUSES` name all eight; `run_stop.rs` counts the same set.
   *
   * `interrupted` is the núcleo dying underneath a run — a defect in *us*, and
   * the run may well have been fine. It gets the held tone, not the failure
   * tone, so that a screen full of interruptions reads as "the daemon
   * restarted" and sends you to look at the daemon. Acting Green means the núcleo is executing
   * right now, so terminal success takes Stated Blue: a fact with no verdict attached, not `off`,
   * which means switched off on purpose. A measurement's good outcome (`gate.passed`,
   * `collision.clean`, `council_seat.ok`, `voice_cleanup.cleaned`, `web_trust.raw`,
   * `web_extract.article`, `pillar.ok`) keeps Acting Green because it is the verdict; blueing it
   * would erase the difference from "not measured". `timed_out` is a ceiling, not a
   * verdict: `run_stop.rs` keeps `Kind::Timeout` apart from `Kind::Failed`, so
   * it gets Held Ember like `council_seat.timeout` and `team_run.expired` and
   * never says "fail". `superseded` is quiet because work continues in its
   * successor; it asks nothing of the reader, so it is off rather than paused.
   */
  run: {
    running: { tone: "active", label: "running" },
    awaiting_approval: { tone: "pending", label: "awaiting approval" },
    completed: { tone: "info", label: "completed" },
    interrupted: { tone: "paused", label: "interrupted" },
    failed: { tone: "danger", label: "failed" },
    cancelled: { tone: "off", label: "cancelled" },
    timed_out: { tone: "paused", label: "timed out" },
    superseded: { tone: "off", label: "superseded" },
  },

  /**
   * Job outcomes.
   *
   * `stopped` is a person or a rule halting the chain; `expired` is the window
   * closing on it; `cancelled` is the request being withdrawn. None of the
   * three is a failure and none of the three is a completion, so none of them
   * borrows either tone. Its completed state follows the terminal-success rule: Stated Blue is a
   * fact, while Acting Green is work happening now.
   */
  job: {
    // The six live statuses (`core/src/job.rs`, `LIVE_STATUSES`). A job that is running is not
    // an unknown word — firing the unmapped badge on the one job actually working teaches the
    // reader to ignore the device that exists to admit ignorance.
    planning: { tone: "active", label: "planning" },
    implementing: { tone: "active", label: "implementing" },
    gating: { tone: "active", label: "running the gate" },
    reviewing: { tone: "active", label: "reviewing" },
    awaiting_approval: { tone: "pending", label: "awaiting approval" },
    // Held by a brake — budget, a slot, or an exclusion. `wait_reason` says which.
    waiting: { tone: "paused", label: "held" },
    completed: { tone: "info", label: "completed" },
    failed: { tone: "danger", label: "failed" },
    gate_failed: { tone: "danger", label: "the gate failed" },
    // The `gate` domain's rule, and for the same reason: a gate that could not run measured
    // nothing, and red would say the code is broken when the measurement is.
    gate_errored: { tone: "info", label: "gate not measured" },
    // The núcleo died underneath it — the `run` domain's reading, unchanged.
    interrupted: { tone: "paused", label: "interrupted" },
    stopped: { tone: "off", label: "stopped" },
    expired: { tone: "paused", label: "expired" },
    cancelled: { tone: "off", label: "cancelled" },
  },

  /**
   * One item of a job's queue — every `job_items.status` the núcleo stores, read out of
   * `item_state_from` in `core/src/job.rs`, plus `pending`, which that function reaches through
   * its `_ =>` arm because it is the column's default. `GateRetriable` is absent on purpose: it is
   * never stored, only derived from `gate_failed` and an attempt count, so the wire cannot say it.
   * `state-map-completeness.test.ts` holds this row to that function.
   *
   * **`conflicted` is Held Ember — not Wrong Red, and not Awaiting-You Amber.** The daemon says
   * which in the variant's own doc (`core/src/job.rs`, `ItemState::Conflicted`): "The item is put
   * down rather than failed: a conflict is a question about two pieces of work, not a verdict on
   * either", and it "Leaves for `Running` — the resolution node, in that same tree". Red would be
   * the verdict the daemon declines to give. Amber would be a summons, and nobody is being waited
   * on: `ItemState::claimable_as` answers `Some("conflicted")` and `batch_of` takes a claimable
   * item as work (the comment above `next_step`'s positional search: "`Conflicted` is found by
   * the same search, and it is work for the same reason: the item owes a run"), so the queue
   * starts the resolution run itself. Put down and owed a run is `reverted`'s reading too — held,
   * not wrong — and the feed's `job_item_conflicted` wears the same tone, because one event must
   * not wear two tones on two pages.
   *
   * `skipped` is the one that does ask: the item put itself down with a `skipped-item` proposal,
   * and that proposal is in Waiting. `orphaned` is the feed's `job_item_orphaned`, quiet for the
   * reason given there. `passed` is terminal success, so Stated Blue.
   */
  job_item: {
    pending: { tone: "off", label: "to do" },
    running: { tone: "active", label: "running" },
    implemented: { tone: "active", label: "written, not yet measured" },
    merging: { tone: "active", label: "merging" },
    reverted: { tone: "paused", label: "taken back off the branch" },
    passed: { tone: "info", label: "done" },
    failed: { tone: "danger", label: "failed" },
    cancelled: { tone: "off", label: "cancelled" },
    gate_failed: { tone: "danger", label: "the gate failed" },
    // The `gate` domain's rule: a gate that could not run measured nothing.
    gate_errored: { tone: "info", label: "gate not measured" },
    skipped: { tone: "pending", label: "skipped, needs a decision" },
    conflicted: { tone: "paused", label: "did not merge" },
    orphaned: { tone: "off", label: "never attempted" },
    // Taken over by an item of a later round (`job.rs`, `STATUS_SUPERSEDED`): terminal, and its
    // work continues in the successor — quiet, as `run.superseded` is.
    superseded: { tone: "off", label: "taken over by a later round" },
  },

  /**
   * Which of the collision warning's two sources is speaking.
   *
   * The keys are the núcleo's own field names on `Collisions` (`declared`, `observed` —
   * `data/fleet.ts`, read off `core/src/collision.rs`); the words and the tones are the shell's,
   * and this docstring is the permission the header asks for. §7 asks for two badges because
   * *this collided* is a measurement and *this will collide* is a prediction, so the source is
   * worded apart AND toned apart: the measurement in Wrong Red, the prediction in Held Ember.
   */
  collision_source: {
    observed: { tone: "danger", label: "observed" },
    declared: { tone: "paused", label: "predicted" },
  },

  /**
   * One "these two never run at the same time", in either of its two lives.
   *
   * A shell derivation, allowed by this docstring: `active` is a row of `fleet_exclusions` and
   * `pending` is a `fleet-exclusion` proposal still in the queue (`canvas/model.ts`,
   * `exclusionEdges`). A rule in force holds a job back, which is Held Ember's whole meaning —
   * not Deliberating Violet, which is shadow mode and nothing else. A question nobody has answered
   * asks something of the reader, so it is amber.
   */
  exclusion: {
    active: { tone: "paused", label: "rule in force" },
    pending: { tone: "pending", label: "asked, not decided" },
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
   *
   * `slot` is Stated Blue and not amber: the slot frees itself when the run holding it ends, so the
   * wait asks nothing of the reader — it is a fact about the queue. Budget, exclusion and disk keep
   * Held Ember, because each is a rule or a ceiling somebody could lift.
   */
  wait_reason: {
    budget: { tone: "paused", label: "held by budget" },
    quota: { tone: "paused", label: "held by quota" },
    slot: { tone: "info", label: "waiting for a slot" },
    excluded: { tone: "paused", label: "held by an exclusion" },
    disk: { tone: "paused", label: "held by a full disk" },
  },

  /**
   * How much of one provider's usage window is gone (`core/src/quota.rs`).
   *
   * The vocabulary is the Rust `QUOTA_STATES` array and is checked against it
   * by `state-map-completeness.test.ts` — the daemon computes the state from
   * the owner's thresholds and sends the word, so this table only paints it.
   *
   * The two quiet tones are the point of the domain, not an afterthought.
   * `unmeasured` is a provider with no readable source: there is no number, so
   * the ring is drawn dashed and empty, and the later brake ignores it
   * entirely. `stale` is a real number about a window that has already rolled
   * over. Both are `off` rather than `info`, because `info` would state them
   * with the same confidence as a figure somebody measured — which is the one
   * thing a quota display must never do, since the same reading is what a
   * brake is later allowed to stop work on.
   *
   * `warn` is Awaiting-You Amber and `exhausted` is red: the first asks the
   * owner to decide what to spend the rest on, the second says the decision has
   * been made for them.
   */
  quota: {
    ok: { tone: "info", label: "within the window" },
    warn: { tone: "pending", label: "most of the window is gone" },
    exhausted: { tone: "danger", label: "the window is spent" },
    stale: { tone: "off", label: "this window has since reset" },
    unmeasured: { tone: "off", label: "no quota source to read" },
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
   * failure: the queue will not retry it, and the answer is to submit again —
   * so it takes the held tone rather than the red one. The instruction lives on
   * `pages/Waiting.tsx`'s `VcsRow`: a full sentence in an 11px pill at 0.08em
   * tracking is a badge doing the row's work, while every other badge in the
   * shots is one or two words. And
   * `escalated` is a *normal outcome*: a person owns the conflict now, which is
   * the queue working, not the queue breaking. Dressing either as `failed`
   * sends somebody to debug a merge that behaved exactly as designed. `succeeded` is terminal
   * success, so it is Stated Blue rather than Acting Green.
   */
  vcs: {
    succeeded: { tone: "info", label: "landed" },
    failed: { tone: "danger", label: "failed" },
    blocked: { tone: "paused", label: "blocked" },
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
   * something to say. `done` is terminal success, so it is Stated Blue rather than work in
   * flight. `cancelled` is withdrawn work, not a verdict, so it takes
   * the same quiet `off` every other cancellation in this table does.
   */
  council: {
    running: { tone: "pending", label: "deliberating" },
    done: { tone: "info", label: "settled" },
    error: { tone: "danger", label: "failed" },
    cancelled: { tone: "off", label: "cancelled" },
  },

  /**
   * One seat's status within one step — `core/src/council.rs`. The same
   * literals serve every step a seat takes (answer, critique, revise); a card
   * reads this table once per step it draws.
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
    // A step whose run answered and whose payload did not parse. Not the seat's
    // run failing — the model spoke, just not in the shape asked for — so it is
    // held, like `timeout`, rather than danger, and it is not silence either.
    invalid: { tone: "paused", label: "invalid" },
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
   * danger tone, and neither says the word. `done` is terminal success, so it is Stated Blue
   * rather than Acting Green.
   */
  team_run: {
    // Awaiting-You Amber asks something of the reader; a director planning a round asks nothing.
    planning: { tone: "active", label: "planning" },
    working: { tone: "active", label: "working" },
    delivering: { tone: "active", label: "delivering" },
    done: { tone: "info", label: "delivered" },
    stopped: { tone: "paused", label: "stopped at a ceiling" },
    expired: { tone: "paused", label: "ran out of time" },
    failed: { tone: "danger", label: "failed" },
    cancelled: { tone: "off", label: "cancelled" },
  },

  /**
   * One item of one round. Exactly four, and these four: `core/src/team.rs` writes
   * `'pending'` (`:3398`), `'running'` (`:2890`), `'done'` (`:3253`) and `'failed'`
   * (`:2669, :2852, :3229, :3265, :3813, :3842`) and writes nothing else into
   * `team_items.state`. `working`, `planned` and `skipped` were this shell's own invention —
   * see `state-map-completeness.test.ts`, which reads the Rust rather than trusting this line.
   * Awaiting-You Amber asks something of the reader; unstarted work is queued, not a summons.
   * `done` is a terminal fact, not work in flight, so it is Stated Blue.
   */
  team_item: {
    pending: { tone: "off", label: "not started" },
    running: { tone: "active", label: "running" },
    done: { tone: "info", label: "done" },
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
   * to. Which of the two it is comes from `proposal_id`, not from here. `done` is terminal
   * success, so it is Stated Blue rather than Acting Green.
   */
  team_action: {
    pending: { tone: "pending", label: "not carried out yet" },
    working: { tone: "active", label: "being carried out" },
    done: { tone: "info", label: "carried out" },
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
  email_class: { tone: "off", label: "not triaged yet", provisional: true },
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

/** Every literal this table has a reading for, in one domain. For the completeness test. */
export function statesOf(domain: StateDomain): string[] {
  return Object.keys(READINGS[domain]);
}
