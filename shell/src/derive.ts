import type { Budget, ClassTally, JobItem, ProjectSummary } from "./api";
import type { BadgeTone } from "./ui/Badge";

/**
 * The badge tone for a message's triage class.
 *
 * Those five tones are the app's entire state vocabulary, so this mapping makes claims about
 * attention rather than decoration: gold means the message wants something from you, green means it
 * wants nothing, grey means it should recede, and `shadow` — already the app's tone for "observed,
 * not acted on" — means no verdict exists yet.
 *
 * `failed` borrows `action`'s tone on purpose. It is not a statement about the content; it is a
 * message that still needs something from you, namely a requeue.
 */
export function mailTone(triageClass: string | null): BadgeTone {
  switch (triageClass) {
    case null:
      return "shadow";
    case "urgent":
      return "pending";
    case "action":
    case "failed":
      return "paused";
    case "info":
      return "active";
    default:
      // `noise` and anything a future núcleo invents. An unknown class recedes rather than
      // shouting: the alternative is a model typo painting the inbox gold.
      return "off";
  }
}

/** What a message's badge says. `null` is a state, not a missing value. */
export function mailLabel(triageClass: string | null): string {
  return triageClass ?? "waiting";
}

/**
 * The name a downloaded attachment is saved under.
 *
 * The daemon already sends a sanitised `Content-Disposition`, but downloading through a blob — the
 * only way to send the bearer token — bypasses that header entirely, so the name has to be made
 * safe again on this side. Mirrors the essential half of the daemon's `safe_filename`: last path
 * segment, no control characters, and a name of our own when nothing usable survives.
 */
export function safeDownloadName(filename: string | null): string {
  const base = (filename ?? "").split(/[/\\]/).pop() ?? "";
  // eslint-disable-next-line no-control-regex
  const cleaned = base.replace(/[\u0000-\u001f\u007f]/g, "").trim();
  if (cleaned === "" || cleaned === "." || cleaned === "..") return "attachment.bin";
  return cleaned;
}

/**
 * Turns the base64 a bulk attachment read arrives in back into bytes.
 *
 * `atob` yields a string of char codes, not text — writing it into a Blob directly would re-encode
 * every byte above 127 as UTF-8 and quietly corrupt any file that is not plain ASCII, which is
 * every real document.
 */
export function base64ToBytes(encoded: string): Uint8Array {
  const binary = atob(encoded);
  const bytes = new Uint8Array(binary.length);
  for (let i = 0; i < binary.length; i += 1) {
    bytes[i] = binary.charCodeAt(i);
  }
  return bytes;
}

/**
 * A file size in the units a person recognises from their own file manager.
 *
 * Decimal units (kB = 1000), because that is what the OS shows next to the same file — matching the
 * binary convention would make every attachment look slightly smaller here than where it lands.
 */
export function formatBytes(bytes: number): string {
  if (!Number.isFinite(bytes) || bytes < 0) return "—";
  if (bytes < 1000) return `${bytes} B`;
  const units = ["kB", "MB", "GB"];
  let value = bytes / 1000;
  let unit = 0;
  while (value >= 1000 && unit < units.length - 1) {
    value /= 1000;
    unit += 1;
  }
  // One decimal below 10 and none above: "1.4 MB" is informative, "847.3 kB" is false precision.
  return `${value < 10 ? value.toFixed(1) : Math.round(value)} ${units[unit]}`;
}

export interface Readiness {
  ready: boolean;
  rate: number | null;
  samples: number;
}

/**
 * Mirrors `READINESS_MIN_REVIEWED` / `READINESS_MIN_AGREE_PERCENT` in `core/src/shadow.rs`, which is
 * the SINGLE SOURCE OF TRUTH for the promotion bar. These copies drive per-class scoreboard COPY
 * only — the gate on the promote control reads `project.promotable` off the daemon, so a drift here
 * can mislabel a row but can never let a project be promoted on different arithmetic.
 */
export const READINESS_MIN_REVIEWED = 10;
export const READINESS_MIN_RATE = 0.95;

export function promotionReadiness(tally: ClassTally): Readiness {
  const samples = tally.reviewed;
  const rate = samples === 0 ? null : tally.agree / samples;
  const ready =
    samples >= READINESS_MIN_REVIEWED &&
    rate !== null &&
    rate >= READINESS_MIN_RATE;
  return { ready, rate, samples };
}

/** A short reason a class is not yet promotable, or null when it is ready. */
export function readinessGap(tally: ClassTally): string | null {
  const { ready, rate, samples } = promotionReadiness(tally);
  if (ready) return null;
  if (samples < READINESS_MIN_REVIEWED) {
    const missing = READINESS_MIN_REVIEWED - samples;
    return `${missing} more review${missing === 1 ? "" : "s"}`;
  }
  return `${Math.round((rate ?? 0) * 100)}% agreement`;
}

/** How many action classes in a group clear the promotion bar. */
export function scoreboardReadiness(
  tallies: ClassTally[],
): { ready: number; total: number } {
  const ready = tallies.filter((tally) => promotionReadiness(tally).ready).length;
  return { ready, total: tallies.length };
}

export function readinessCriterionLabel(): string {
  return `Ready at ${READINESS_MIN_REVIEWED}+ reviews, ≥${Math.round(READINESS_MIN_RATE * 100)}% agreement`;
}

/** The three numbers the shadow-exit rule is decided on, exactly as `shadow.rs` computes them. */
export interface PromotionCriteria {
  classes_ready: number;
  classes_total: number;
  withheld_classes_ready: number;
}

/**
 * Which promotion criterion is not met yet, or null when they all are.
 *
 * Three situations, because each asks for something different to be done about it. Nothing reviewed
 * yet is a lack of evidence. A class short of the bar is more reviewing. And a corpus of nothing but
 * ALLOWED actions is a gap that no amount of further reviewing closes: it validates that the
 * classifier lets through what it should, and says nothing about whether it holds back what it
 * should — restraint being the only property that matters once the project acts on its own.
 *
 * That last one is the case that reads as a bug when it is not named: every exercised class ready,
 * every count matching, and the button still locked.
 */
export function promotionCriterionGap(criteria: PromotionCriteria): string | null {
  if (criteria.classes_total === 0) return "No reviewed shadow decisions yet";
  if (criteria.classes_ready < criteria.classes_total) {
    return `${criteria.classes_ready}/${criteria.classes_total} action classes ready`;
  }
  if (criteria.withheld_classes_ready === 0) {
    return "No withheld action class has cleared the review bar";
  }
  return null;
}

/**
 * Why the promote-to-active control is locked for a project, or null when it may be promoted.
 *
 * The gate exists so shadow mode has a real exit criterion: promotion stays the human's call, but it
 * can't be made on a hunch after three runs. A project already active is never gated (this only
 * guards the way IN to autonomy).
 *
 * The daemon still owns the verdict — `project.promotable` is what unlocks the control. This only
 * decides which unmet criterion to NAME, which is why it delegates: the wording of the gap and the
 * arithmetic behind it stay in one place.
 */
export function promotionBlock(project: ProjectSummary): string | null {
  if (project.mode === "active" || project.promotable) return null;
  return promotionCriterionGap({
    classes_ready: project.classes_ready,
    classes_total: project.classes_total,
    withheld_classes_ready: project.withheld_classes_ready ?? 0,
  });
}

/**
 * Why a project has stopped starting new work, or null when nothing is holding it.
 *
 * A project whose approval queue is full goes quiet on purpose — but silent throttling reads as a
 * bug, so the reason has to be as visible as the budget pause is. Self-clearing: reviewing one
 * proposal releases it, which is why the copy points at reviewing rather than at raising the limit.
 */
export function queueBlock(project: ProjectSummary): string | null {
  if (!project.queue_full) return null;
  // A null limit means no ceiling was configured, so there is no denominator to
  // show: "3/0 proposals waiting" would claim a limit of zero that the queue
  // has somehow exceeded, which is not a thing that can happen.
  const waiting =
    project.wip_limit === null
      ? `${project.open_proposals}`
      : `${project.open_proposals}/${project.wip_limit}`;
  return `${waiting} proposals waiting — new work is deferred until you review one`;
}

export function totalPending(projects: ProjectSummary[]): number {
  return projects.reduce((sum, project) => sum + project.pending, 0);
}

export type AutopilotState =
  | "kill"
  | "budget"
  | "first"
  | "swamped"
  | "pending"
  | "quiet";

/** Proposals beyond this count tip the approval queue into its dense layout. */
export const SWAMPED_THRESHOLD = 3;

export interface AutopilotSignals {
  killEngaged: boolean | null;
  budgetPaused: boolean;
  /** True only once the project list has loaded and turned out empty. */
  isFirstProject: boolean;
  proposalCount: number;
  pending: number;
}

/**
 * The single most important thing about autopilot right now, in priority order:
 * a global stop outranks a budget pause, which outranks onboarding, a swamped
 * queue, pending review, and finally calm. Shared by the Autopilot cockpit and
 * the Home digest so the two never tell a different story.
 */
export function autopilotState(signals: AutopilotSignals): AutopilotState {
  if (signals.killEngaged === true) return "kill";
  if (signals.budgetPaused) return "budget";
  if (signals.isFirstProject) return "first";
  if (signals.proposalCount > SWAMPED_THRESHOLD) return "swamped";
  if (signals.pending > 0) return "pending";
  return "quiet";
}

export function agreementRate(tally: ClassTally): number | null {
  if (tally.reviewed === 0) return null;
  return tally.agree / tally.reviewed;
}

export function groupScoreboardByMode(
  tallies: ClassTally[],
): Record<string, ClassTally[]> {
  const grouped: Record<string, ClassTally[]> = {};
  for (const tally of tallies) {
    (grouped[tally.mode] ??= []).push(tally);
  }
  return grouped;
}

export function killSwitchLabel(engaged: boolean): string {
  return engaged ? "Kill switch engaged — autopilot paused" : "Kill switch off";
}

export function formatUsd(amount: number): string {
  return `$${amount.toFixed(2)}`;
}

export function periodLabel(period: Budget["period"]): string {
  switch (period) {
    case "daily":
      return "today";
    case "weekly":
      return "this week";
    case "monthly":
      return "this month";
  }
}

export function budgetStatusLabel(budget: Budget): string {
  if (budget.limit_usd === null) return "No spending limit set";
  const base = `${formatUsd(budget.window_spend_usd)} of ${formatUsd(budget.limit_usd)} ${periodLabel(budget.period)}`;
  return budget.paused ? `Paused — ${base}` : base;
}

/**
 * The two shadow-review answers, and the verdict string the daemon expects behind each.
 *
 * The daemon's `AGREE_CASE` (core/src/shadow.rs) reads a verdict as the answer to "would you
 * have allowed this ACTION?": `approve` agrees with `allow`, and `reject` agrees with BOTH
 * `deny` and `pending_approval` — withholding an action the classifier also withheld IS
 * agreement.
 *
 * These buttons used to read "Agree"/"Disagree", which asks about the CLASSIFICATION instead.
 * The two questions coincide only on `allow` rows and are inverse everywhere else: agreeing that
 * an action should require approval sent `approve`, which the gate scored as a disagreement. The
 * promotion scoreboard therefore punished the reviewer who read carefully and rewarded the one
 * who waved everything through — the exact failure the shadow gate exists to prevent. Do not
 * relabel these without changing `AGREE_CASE` to match.
 */
export const REVIEW_ALLOW = { label: "Allow", verdict: "approve" } as const;
export const REVIEW_BLOCK = { label: "Block", verdict: "reject" } as const;

/** The classifier's own verdict, phrased as an answer to the same question the buttons ask. */
export function classifierVerdictLabel(decision: string): string {
  switch (decision) {
    case "allow":
      return "would allow";
    case "deny":
      return "would block";
    case "pending_approval":
      return "would ask you";
    default:
      return `would ${decision}`;
  }
}

/**
 * A compact, human relative time like "12 min ago" for feed/proposal metadata;
 * the exact ISO instant belongs in a `title`. `nowMs` defaults to the current
 * time so callers pass just the ISO string, while tests pin it. Unparseable
 * input falls back to the raw string; timestamps older than ~a month fall back
 * to the calendar date.
 */
export function relativeTime(iso: string, nowMs: number = Date.now()): string {
  const then = Date.parse(iso);
  if (Number.isNaN(then)) return iso;
  const sec = Math.floor((nowMs - then) / 1000);
  if (sec < 60) return "just now";
  const min = Math.floor(sec / 60);
  if (min < 60) return `${min} min ago`;
  const hr = Math.floor(min / 60);
  if (hr < 24) return `${hr} h ago`;
  const day = Math.floor(hr / 24);
  if (day < 7) return `${day} d ago`;
  const wk = Math.floor(day / 7);
  if (wk < 5) return `${wk} w ago`;
  return iso.slice(0, 10);
}

/**
 * The statuses a job holds the project's exclusivity slot in.
 *
 * Mirrors `job::LIVE_STATUSES` in the daemon, which mirrors the partial unique index in migration
 * 0037. The shell only reads it, so a drift here is cosmetic rather than dangerous — but a job the
 * shell calls finished while the daemon still drives it is exactly the confusion the cancel button
 * exists to resolve, so it is worth keeping honest.
 */
export const JOB_LIVE_STATUSES = [
  "planning",
  "implementing",
  "gating",
  "reviewing",
  "awaiting_approval",
  "waiting",
] as const;

export function jobIsLive(status: string): boolean {
  return (JOB_LIVE_STATUSES as readonly string[]).includes(status);
}

/**
 * What a job is doing, in the user's words.
 *
 * `waiting` is the one status that cannot speak for itself: it now covers a budget window that will
 * reopen and a slot another run is holding, and those ask opposite things of whoever reads it —
 * spend more and wait, or find out what else is running. A bare "waiting" makes the reader guess.
 */
export function jobStageLabel(status: string, waitReason: string | null): string {
  switch (status) {
    case "planning":
      return "working out what to do";
    case "implementing":
      return "working through its list";
    case "gating":
      return "running the tests";
    case "reviewing":
      return "reviewing its own diff";
    case "awaiting_approval":
      return "waiting for you to approve an action";
    case "waiting":
      switch (waitReason) {
        case "budget":
          return "paused — the budget is spent for now";
        case "slot":
          return "queued — something else has the project";
        case "attention":
          return "holding off — you are at the keyboard";
        case "kill-switch":
          return "stopped — the kill switch is engaged";
        default:
          return "waiting to continue";
      }
    default:
      return jobEndingLabel(status);
  }
}

/**
 * How a job ended, said in a way that survives being skim-read.
 *
 * The pair this function exists for is `gate_failed` and `gate_errored`. A non-zero exit says the
 * code is broken; a command that would not start says nothing was ever measured. Both are "did not
 * pass", and the daemon keeps them apart from `gate.rs` all the way up to `jobs.status` — collapsing
 * them here, at the last step, would waste every one of those and tell somebody their tests failed
 * when no test ever ran.
 *
 * `expired` and `stopped` are the second pair. One means the four-hour clock ran out and the rest
 * of the queue is still worth doing; the other means the budget window is spent and starting again
 * today stops in the same place.
 */
export function jobEndingLabel(status: string): string {
  switch (status) {
    case "completed":
      return "finished";
    case "failed":
      return "stopped — an item did not finish";
    case "cancelled":
      return "cancelled";
    case "gate_failed":
      return "stopped — the tests went red";
    case "gate_errored":
      return "stopped — the tests could not be run, so nothing was measured";
    case "expired":
      return "stopped — it ran out of time, and its list was not finished";
    case "stopped":
      return "stopped — the budget window is spent";
    case "interrupted":
      return "interrupted — the daemon restarted and the repository had moved";
    default:
      // A status a newer daemon invented. Shown rather than swallowed: an unrecognised ending is
      // still an ending, and hiding it would leave the row looking unfinished forever.
      return status;
  }
}

/**
 * How far a job got, counted in items that are actually done.
 *
 * `passed` only. An item that has been implemented but not yet gated is not finished work — that is
 * the whole reason the gate runs between items — and counting it would let the bar reach the end
 * while a red gate was still to come.
 */
export function jobProgress(items: JobItem[]): { done: number; total: number } {
  return {
    done: items.filter((item) => item.status === "passed").length,
    total: items.length,
  };
}

/** The badge tone for one item of a job's queue. */
export function jobItemTone(status: string): BadgeTone {
  switch (status) {
    case "passed":
      return "active";
    case "running":
      return "pending";
    case "implemented":
      return "shadow";
    case "failed":
    case "cancelled":
    case "gate_failed":
    case "gate_errored":
      return "paused";
    default:
      // `pending`, and anything a newer daemon adds. Neither started nor finished, so it recedes.
      return "off";
  }
}

/**
 * What one item's row says about itself.
 *
 * The distinction worth the words is `passed` with no gate status: that item was not measured. It
 * happens legitimately — the project configures no gate command, or `gate_after_each_item` is off
 * and this was not the last item — and a row that said a flat "passed" for both would claim a
 * verdict that nobody ever produced.
 */
export function jobItemLabel(item: JobItem): string {
  switch (item.status) {
    case "pending":
      return "not started";
    case "running":
      return "in progress";
    case "implemented":
      return "done, not yet measured";
    case "passed":
      return item.gate_status === "passed" ? "passed the tests" : "done, not measured";
    case "failed":
      return "did not finish";
    case "cancelled":
      return "cancelled";
    case "gate_failed":
      return "the tests went red here";
    case "gate_errored":
      return "the tests could not be run here";
    default:
      return item.status;
  }
}

/**
 * The feed's own words for a job event.
 *
 * The feed prints `kind` verbatim for everything else, which reads fine for `run_retry` and badly
 * for `job_gate_failed`. Only the job kinds are translated; anything else falls through unchanged,
 * so a daemon that starts emitting a new kind still shows it rather than showing nothing.
 */
export function feedKindLabel(kind: string): string {
  switch (kind) {
    case "job_started":
      return "job started";
    case "job_planned":
      return "job planned its work";
    case "job_plan_failed":
      return "job could not plan";
    case "job_item_failed":
      return "job item did not finish";
    case "job_gate_failed":
      return "job gate";
    case "job_waiting":
      return "job waiting";
    case "job_finished":
      return "job finished";
    case "job_stopped":
      return "job stopped";
    case "job_expired":
      return "job ran out of time";
    case "job_cancelled":
      return "job cancelled";
    case "job_interrupted":
      return "job interrupted";
    case "job_failed":
      return "job failed";
    default:
      return kind;
  }
}
