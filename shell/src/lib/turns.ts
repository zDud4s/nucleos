/**
 * A chat turn, read purely — no React, no `data/`, no daemon.
 *
 * Kept apart from `data/chats.ts` on purpose: `merge`, `marksBetween` and
 * `unreadTotal` are decisions about *shape*, not about fetching, and a module
 * that cannot import a hook is a module that cannot accidentally grow one.
 * `turns.test.ts` asserts every rule here without mounting a component or
 * mocking a daemon.
 */

/** Which model answered a turn, or is about to. */
export type Brain = "cloud" | "local";

/**
 * A turn, exactly as `GET /assistant/chats/{chat_id}` serialises one —
 * `core/src/http.rs`'s `AssistantTurn`, oldest first.
 */
/**
 * One tool a turn ran, as the daemon distilled it.
 *
 * `detail` is the single argument worth showing — a path, a command, a pattern —
 * and never the whole input: a `Write` carries the file it is writing, and a
 * conversation that printed that argument would print the file.
 */
export interface ToolCall {
  name: string;
  detail: string | null;
  /**
   * The plan this call wrote, when it was one that writes plans. Empty for every
   * other tool, and empty on any turn recorded before the daemon carried them.
   */
  todos: Todo[];
}

/** One line of a plan, as the daemon read it out of a `TodoWrite`. */
export interface Todo {
  text: string;
  /** The CLI's own words: `pending`, `in_progress`, `completed`. */
  status: string;
}

/**
 * The plan a turn ended with, or nothing.
 *
 * The LAST one, because a plan is rewritten as it is worked through: every `TodoWrite` in a turn is
 * the same list at a different moment, and drawing all of them would be the same three items four
 * times over with only the ticks moving.
 */
export function planOf(did: ToolCall[]): Todo[] {
  for (let at = did.length - 1; at >= 0; at -= 1) {
    const call = did[at];
    if (call.todos !== undefined && call.todos.length > 0) return call.todos;
  }
  return [];
}

export interface AssistantTurnRow {
  id: number;
  asked: string;
  answer: string | null;
  error: string | null;
  status: string;
  cost_usd: number | null;
  /** Null on a turn from before the column existed — never a guess, and never rendered as a mark. */
  answered_by: Brain | null;
  session_id: string | null;
  created_at: string;
  /**
   * How much context the turn ran with, an absolute token count. Null on a turn
   * whose stream never reported one, and on every turn from before the column.
   */
  context_fill: number | null;
  /** The count past which the daemon stops resuming. The same on every row. */
  context_rotates_at: number;
  /**
   * What the turn ran, oldest first. Empty on a turn that acted on nothing AND
   * on a turn from before the daemon recorded this — the daemon collapses the
   * two deliberately, because a client cannot act on the difference.
   */
  did: ToolCall[];
  /**
   * What the turn thought, oldest first — and empty on every turn so far.
   *
   * The CLI withholds the words: a `thinking` block arrives as
   * `{"type":"thinking","thinking":"","signature":"…"}`, in its stream and in
   * its own transcript files alike. The field is carried so the day that
   * changes the words appear; until then `thought_tokens` below is what a turn
   * can honestly be asked about.
   */
  thought: string[];
  /**
   * The pictures this turn was sent with, as paths under the files root.
   *
   * Paths and not bytes: a transcript of forty turns costs forty short strings
   * rather than forty screenshots, and each one is fetched only when it is
   * actually about to be drawn.
   */
  images: string[];
  /**
   * Roughly how many tokens the turn spent thinking, or null when it did not
   * think and on every turn from before the column.
   *
   * An estimate — the CLI's own running count. What it has to be right about
   * is whether the model deliberated and roughly how hard.
   */
  thought_tokens: number | null;
}

/**
 * One exchange, as the page holds it.
 *
 * `answer` carries the whole settled reply. The daemon splits a failure into
 * `error` because that is where a run's stderr lands, but the transcript has
 * no separate place for it to go — a failed turn is shown exactly like an
 * answered one, with whatever text explains what happened. `null` while the
 * turn is still live, which is what lets a page tell "still thinking" apart
 * from "answered with nothing".
 */
export interface Turn {
  id: number;
  asked: string;
  answer: string | null;
  status: string;
  cost_usd: number | null;
  answeredBy: Brain | null;
  sessionId: string | null;
  /** What the turn ran. See `AssistantTurnRow.did`. */
  did: ToolCall[];
  /** See `AssistantTurnRow.images`. */
  images: string[];
  /** See `AssistantTurnRow.thought`. */
  thought: string[];
  /** See `AssistantTurnRow.thought_tokens`. */
  thoughtTokens: number | null;
  /** See `AssistantTurnRow.context_fill`. */
  contextFill: number | null;
  /** See `AssistantTurnRow.context_rotates_at`. */
  rotatesAt: number | null;
}

/** Whether a turn's status means the daemon is still working it. */
export function turnIsLive(status: string): boolean {
  return status === "pending" || status === "running";
}

/**
 * The first non-empty candidate, trimmed.
 *
 * `answer` is preferred over `error`: a turn that answered and also left
 * stray stderr should still read as answered. Reaching `error` at all means
 * the daemon has nothing else to show for this turn.
 */
function firstNonEmpty(...candidates: (string | null | undefined)[]): string | null {
  for (const candidate of candidates) {
    const trimmed = candidate?.trim();
    if (trimmed !== undefined && trimmed !== "") return trimmed;
  }
  return null;
}

/**
 * One daemon row, read into the shape the transcript draws.
 *
 * `answer` stays `null` while the turn is live — rendering it early would
 * show an empty bubble for a turn that has not answered yet, which reads as
 * the assistant ignoring you rather than as still working.
 */
export function turnFromRow(row: AssistantTurnRow): Turn {
  const settled = !turnIsLive(row.status);
  return {
    id: row.id,
    asked: row.asked,
    answer: settled ? firstNonEmpty(row.answer, row.error) : null,
    status: row.status,
    cost_usd: row.cost_usd,
    answeredBy: row.answered_by,
    sessionId: row.session_id,
    // Defaulted rather than trusted: a daemon older than the column sends no such key, and a
    // conversation losing one line is a better answer to that than a page that will not draw.
    did: row.did ?? [],
    // Defaulted for the reason `did` is: a daemon older than the column sends no such key, and a
    // turn drawn without its reasoning beats a page that refuses to draw the turn.
    images: row.images ?? [],
    thought: row.thought ?? [],
    thoughtTokens: row.thought_tokens ?? null,
    contextFill: row.context_fill ?? null,
    // Null, not a number this side made up. A daemon that does not send the ceiling is one whose
    // ceiling this window does not know, and guessing it would draw a proportion out of nothing.
    rotatesAt: row.context_rotates_at ?? null,
  };
}

/**
 * Is any turn in this transcript still live?
 *
 * `undefined` — no answer has landed yet — reads as live: the first fetch has
 * not resolved, and a transcript that turned its own fast poll off before it
 * ever started would be a page waiting on a load that never speeds up.
 */
export function anyTurnLive(turns: Turn[] | undefined): boolean {
  if (turns === undefined) return true;
  return turns.some((turn) => turnIsLive(turn.status));
}

/**
 * The daemon's transcript, plus any turn the page knows about that the
 * daemon's read has not caught up with yet.
 *
 * A plain replace is wrong in one narrow, reachable case: `POST
 * /assistant/message` inserts the turn's row before the request returns, so a
 * history read that overtakes that insert comes back without it — and the
 * message just sent would vanish from the page until the next poll tick
 * happened to land after the write. The daemon wins wherever both know a
 * turn; anything only the page has is kept, and sorted back into place by id.
 */
export function merge(history: Turn[], local: Turn[]): Turn[] {
  const known = new Set(history.map((turn) => turn.id));
  const extra = local.filter((turn) => !known.has(turn.id));
  return [...history, ...extra].sort((a, b) => a.id - b.id);
}

/**
 * The marks a transcript draws above one turn — at most two, and never more.
 *
 * `brain` fires only when BOTH turns know who answered them: a null on either
 * side means the mark would be drawn against a turn nothing recorded a model
 * for, which is a claim nobody made. `restart` fires when both turns carry a
 * session id and the ids differ — the daemon mints a fresh one when it will
 * not resume the old one, and that is the moment the model on the far side
 * stopped remembering anything above the line.
 *
 * `previous === null` — the transcript's first turn — draws neither mark:
 * there is nothing before it to have changed from.
 */
export type Mark = { kind: "brain"; from: Brain; to: Brain } | { kind: "restart" };

export function marksBetween(previous: Turn | null, turn: Turn): Mark[] {
  if (previous === null) return [];
  const marks: Mark[] = [];
  if (
    previous.answeredBy !== null &&
    turn.answeredBy !== null &&
    previous.answeredBy !== turn.answeredBy
  ) {
    marks.push({ kind: "brain", from: previous.answeredBy, to: turn.answeredBy });
  }
  if (previous.sessionId !== null && turn.sessionId !== null && previous.sessionId !== turn.sessionId) {
    marks.push({ kind: "restart" });
  }
  return marks;
}

/**
 * The sidebar's chat badge and the list's own headline: how many answers
 * landed since each conversation was last opened, summed across all of them.
 *
 * Takes the minimal shape rather than `ChatSummary` — this module does not
 * import from `data/`, and a count is all it needs to know about a row.
 */
export function unreadTotal(chats: { waiting: number }[]): number {
  return chats.reduce((total, chat) => total + chat.waiting, 0);
}
