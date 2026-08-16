import type { AssistantTurnRow, Brain, RunDetail } from "../api";
import { runIsLive } from "../derive";

/**
 * One exchange, as the shell remembers it.
 *
 * Held by `App` rather than by the page that draws it: leaving the tab unmounts the page, and a
 * transcript kept in local state went with it — the message you had just sent was gone when you came
 * back, and so was the poll that would have collected its answer.
 */
export interface Turn {
  /** The daemon's turn id, which is also a run id — the assistant's turns ARE runs. */
  id: number;
  asked: string;
  /** null while the turn is still running. */
  answer: string | null;
  status: string;
  cost_usd: number | null;
  failed: boolean;
  /**
   * Which model answered, or null on turns from before the daemon recorded it.
   *
   * The null is not a gap to be filled in with a guess: the transcript marks where a conversation
   * changed model, and a mark drawn against a turn nothing knows the model of would be a claim
   * nobody made.
   */
  answeredBy: Brain | null;
  /**
   * The CLI session this turn ran in, or null on a turn the shell has only just sent.
   *
   * Kept so the transcript can say where the conversation RESTARTED. The daemon refuses to resume a
   * session past its context ceiling, and past anything that read third-party text, and then mints a
   * fresh one — which means the model on the far side of that line has no memory of anything above
   * it. Nothing else on screen would say so.
   */
  sessionId: string | null;
}

/**
 * How a finished turn's text is found.
 *
 * A turn is a run, so its reply arrives as the run's stdout, and a turn that failed leaves stderr
 * instead. Preferring stdout and falling back to stderr means a failure is shown rather than
 * rendered as an empty bubble, which reads as the assistant ignoring you.
 */
function firstNonEmpty(...candidates: (string | null | undefined)[]): string | null {
  for (const candidate of candidates) {
    const trimmed = candidate?.trim();
    if (trimmed !== undefined && trimmed !== "") return trimmed;
  }
  return null;
}

export function replyText(detail: RunDetail): string | null {
  return firstNonEmpty(detail.stdout, detail.stderr);
}

/**
 * One remembered exchange, as the page holds it.
 *
 * The daemon reports a live turn with no answer yet, so `answer` stays null until the turn settles —
 * which is also what the pending-turn lookup reads to decide there is still something to poll.
 * Deriving it here rather than trusting a non-empty string means a turn that genuinely answered with
 * nothing is not mistaken for one still thinking.
 */
export function turnFromRow(row: AssistantTurnRow): Turn {
  const settled = !runIsLive(row.status);
  return {
    id: row.id,
    asked: row.asked,
    answer: settled ? firstNonEmpty(row.answer, row.error) : null,
    status: row.status,
    cost_usd: row.cost_usd,
    failed: settled && row.status !== "completed",
    answeredBy: row.answered_by,
    sessionId: row.session_id ?? null,
  };
}

/**
 * The daemon's transcript, plus any turn on screen it does not know about yet.
 *
 * A replace would be simpler and is wrong in one narrow, reachable case: the daemon inserts a turn's
 * row while the request that created it is still open, so a history read that overtakes that insert
 * comes back without it — and the message you had just sent would disappear on the way back to the
 * tab, which is the exact loss this is meant to end. The daemon wins wherever both know a turn;
 * anything only the page has is kept and sorted back into place by id.
 */
export function merge(history: Turn[], local: Turn[]): Turn[] {
  const known = new Set(history.map((turn) => turn.id));
  const extra = local.filter((turn) => !known.has(turn.id));
  return [...history, ...extra].sort((a, b) => a.id - b.id);
}
