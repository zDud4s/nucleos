/**
 * A chat turn, read purely — no React, no `data/`, no daemon.
 *
 * Kept apart from `data/chats.ts` on purpose: `merge`, `marksBetween` and
 * `unreadTotal` are decisions about *shape*, not about fetching, and a module
 * that cannot import a hook is a module that cannot accidentally grow one.
 * `turns.test.ts` asserts every rule here without mounting a component or
 * mocking a daemon.
 */

/**
 * Which model answered a turn, or is about to.
 *
 * Three values here, not two: `"openrouter"` is a third conversation route the daemon now serves
 * end to end (`core/src/chats.rs`'s `Brain::OpenRouter`, wire value `"openrouter"`) — a hosted third
 * party reached over OpenRouter's API, distinct from both the cloud agent CLI and the model running
 * on this machine.
 *
 * There is one OTHER `Brain` declaration in this codebase — `data/project-map.ts` — and it stays
 * `"cloud" | "local"` ON PURPOSE. Do not "fix" it to match this one: the project map's
 * `read_brain` only ever yields `cloud` or `local` in the daemon. A third value here says nothing
 * about that.
 */
export type Brain = "cloud" | "local" | "openrouter";

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
  /**
   * What the tool answered, cut by the daemon, or absent.
   *
   * Absent means three different things and the page can act on none of them apart: nothing came
   * back, the turn predates the column, or — the ordinary case — this call arrived on the
   * TRANSCRIPT, which deliberately strips them. See `ToolCall::result` in `core/src/runner.rs`:
   * the transcript is polled once a second while a turn is live, and a hundred turns of tool
   * output on that poll is a cost paid forever for something almost nobody has open. They are
   * fetched per turn, by `useTurnTools`, when somebody opens one.
   */
  result?: string | null;
  /** How long the whole answer was, in characters, so the page can say what it is not showing. */
  result_chars?: number | null;
  /** Whether the tool answered with an error rather than an answer. */
  result_failed?: boolean;
  /** The tool_use id. Absent from a daemon older than the agent map. */
  id?: string;
  /** Id of the Task/Agent call this ran inside. Absent or null means the main agent. */
  parent?: string | null;
  /** The subagent kind, on a Task/Agent call. */
  subagent_type?: string;
  /** The model the subagent ran on. */
  model?: string;
  /** Whether a Bash call was started with run_in_background. */
  background?: boolean;
  /** RFC3339 start and end of the call. */
  started_at?: string;
  finished_at?: string;
  /** Tokens the call spent, on a subagent. */
  tokens?: number;
  /** State of a background task. */
  status?: "running" | "completed" | "failed" | "killed";
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
  /** When the turn settled. Absent from a daemon older than the cache chip; null while live. */
  completed_at?: string | null;
  /** The model the turn ran on, when the daemon says. */
  model?: string | null;
  /** The prompt-cache lifetime in force for the turn, when the daemon knows it. */
  cache_ttl?: "5m" | "1h" | null;
  /**
   * How much context the turn ran with, an absolute token count. Null on a turn
   * whose stream never reported one, and on every turn from before the column.
   */
  context_fill: number | null;
  /**
   * The context window this conversation runs in, in tokens.
   *
   * The same on every row of one conversation, and NOT the same across
   * conversations: a chat picked up from the editor is given a window wide
   * enough to hold what it inherited. It used to be `context_rotates_at`, the
   * point past which the daemon stopped resuming; nothing rotates now, so the
   * number means the window rather than the cliff.
   */
  context_window: number;
  /**
   * Whether the CLI summarised its own context while producing this turn.
   *
   * What the transcript draws from it is a line saying the older exchanges were
   * summarised here — which is the thing that used to happen silently, and is
   * the whole complaint this answers.
   */
  compacted: boolean;
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
  /**
   * The conversation that handed this turn its words, or null for the ordinary
   * case — somebody typed them here.
   *
   * Optional on the wire because a daemon older than the column sends neither
   * key, the same way `did` and `images` are read defensively below.
   */
  relayed_from_chat_id?: string | null;
  /** What that conversation is called, or null when nobody has named it. */
  relayed_from_title?: string | null;
  /**
   * The relays this turn SENT. Empty on almost every turn.
   *
   * Optional on the wire for the reason the two above are: a daemon older than
   * the column sends no such key.
   */
  relayed_to?: RelaySent[];
  /**
   * What the person said to this turn WHILE it was working ("Send now").
   *
   * Optional on the wire for the reason the keys above are: an older daemon
   * sends no such key.
   */
  said_now?: SaidDuring[];
}

/** One line said into a running turn, as the daemon recorded it. */
export interface SaidDuring {
  text: string;
  created_at: string;
}

/**
 * One relay a turn sent: where it went, and what it said.
 *
 * `body` is what the daemon actually wrote down, not what the model asked for.
 * The two differ every time a relay is refused, and a sender's transcript built
 * from the asks would show messages that never arrived.
 */
export interface RelaySent {
  chat_id: string;
  /** Null on a conversation nobody has named yet. */
  title: string | null;
  body: string;
}

/** Where a turn's words came from, when it was not the person reading them. */
export interface RelayedFrom {
  chatId: string;
  /**
   * Null on a conversation with no title yet, which is most of them until the
   * daemon has summarised one. The id is what always resolves, which is why it
   * travels alongside rather than being replaced by the name.
   */
  title: string | null;
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
  /**
   * When the turn was asked — `AssistantTurnRow.created_at`, verbatim.
   *
   * It arrived on every row from the beginning and was drawn nowhere, which made a
   * transcript a stack of exchanges with no time in it: an answer from four minutes ago
   * and one from last Tuesday looked the same, and the only way to date either was to
   * count backwards from the conversation's own position in the list.
   *
   * It is also what the transcript ORDERS by, which is the second reason it cannot be dropped:
   * a department's report lives in a different table with an id sequence of its own, so ids say
   * nothing about which of the two happened first and only the clock does.
   */
  createdAt: string;
  /** See `AssistantTurnRow.completed_at`. */
  completedAt?: string | null;
  /** See `AssistantTurnRow.model`. */
  model?: string | null;
  /** See `AssistantTurnRow.cache_ttl`. */
  cacheTtl?: "5m" | "1h" | null;
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
  /** See `AssistantTurnRow.context_window`. */
  window: number | null;
  /** See `AssistantTurnRow.compacted`. */
  compacted: boolean;
  /**
   * The conversation that handed this turn over, or null when the person whose
   * transcript this is typed it themselves.
   *
   * Not a `Mark`. Those describe what changed BETWEEN two turns — the model, the
   * session — and are drawn from comparing a turn with the one above it. This is
   * a property of the turn itself: it is true of a relayed turn whether or not
   * anything precedes it, including when it is the first thing in a
   * conversation, which is exactly the case a between-turns rule would miss.
   */
  relayedFrom: RelayedFrom | null;
  /**
   * The relays this turn sent, oldest first.
   *
   * The mirror of `relayedFrom`, and it exists because that side shipped alone:
   * the conversation certain to be watched by the person who caused a relay was
   * the one that could not say what it had done.
   */
  relayedTo: RelaySent[];
  /** What the person said into this turn while it was running, oldest first. */
  saidNow?: SaidDuring[];
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
    createdAt: row.created_at,
    completedAt: row.completed_at ?? null,
    model: row.model ?? null,
    cacheTtl: row.cache_ttl ?? null,
    // Defaulted rather than trusted: a daemon older than the column sends no such key, and a
    // conversation losing one line is a better answer to that than a page that will not draw.
    did: row.did ?? [],
    // Defaulted for the reason `did` is: a daemon older than the column sends no such key, and a
    // turn drawn without its reasoning beats a page that refuses to draw the turn.
    images: row.images ?? [],
    thought: row.thought ?? [],
    thoughtTokens: row.thought_tokens ?? null,
    contextFill: row.context_fill ?? null,
    // Null, not a number this side made up. A daemon that does not send the window is one whose
    // window this side does not know, and guessing it would draw a proportion out of nothing.
    window: row.context_window ?? null,
    // False rather than null: a daemon too old to send this is one whose turns never compacted,
    // because compaction is what this release added. So the absent value and the false one say the
    // same thing here, which is the only case where collapsing them is honest.
    compacted: row.compacted ?? false,
    // Keyed off the ID and never off the title: the title is null on every conversation nobody has
    // named, so a check on it would read most relayed turns as ordinary ones. `?? null` is the
    // daemon-older-than-the-column case, same as `did` and `images` above.
    relayedFrom:
      row.relayed_from_chat_id === undefined || row.relayed_from_chat_id === null
        ? null
        : { chatId: row.relayed_from_chat_id, title: row.relayed_from_title ?? null },
    // Defaulted for the reason `did` and `images` are: a daemon older than the column sends no
    // such key, and a turn drawn without the note beats a page that refuses to draw the turn.
    relayedTo: row.relayed_to ?? [],
    saidNow: row.said_now ?? [],
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
 * The highest turn id below which the page holds nothing that can still change.
 *
 * Everything up to the first live turn is settled, so the transcript's poll asks the daemon only
 * for what lies past it — the live turn included, since it is the one still being written.
 * `null` when the page holds nothing yet, which is the read that must be made in full.
 */
export function settledWatermark(turns: Turn[] | undefined): number | null {
  if (turns === undefined || turns.length === 0) return null;
  const live = turns.findIndex((turn) => turnIsLive(turn.status));
  if (live === -1) return turns[turns.length - 1].id;
  return live === 0 ? turns[0].id - 1 : turns[live - 1].id;
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
export type Mark =
  | { kind: "brain"; from: Brain; to: Brain }
  | { kind: "restart" }
  | { kind: "cleared" }
  | { kind: "compacted" };

export function marksBetween(
  previous: Turn | null,
  turn: Turn,
  clearedAfter: number | null,
): Mark[] {
  if (previous === null) return [];
  const marks: Mark[] = [];
  if (
    previous.answeredBy !== null &&
    turn.answeredBy !== null &&
    previous.answeredBy !== turn.answeredBy
  ) {
    marks.push({ kind: "brain", from: previous.answeredBy, to: turn.answeredBy });
  }
  // A clear always restarts the session too, so both rules match and only one thing happened.
  // `cleared` wins because it is the whole truth: the restart mark's own words promise the model
  // was read the last few exchanges, and past a clear it was read nothing at all.
  const cleared =
    clearedAfter !== null && previous.id <= clearedAfter && turn.id > clearedAfter;
  if (cleared) {
    marks.push({ kind: "cleared" });
  } else if (
    previous.sessionId !== null &&
    turn.sessionId !== null &&
    previous.sessionId !== turn.sessionId
  ) {
    marks.push({ kind: "restart" });
  } else if (turn.compacted) {
    // Last of the three and never beside them, because all three answer the same question — what
    // happened to the conversation's memory here — and only one thing happened. Compaction is the
    // mildest and by far the commonest: the session is the same, the model still remembers, and
    // what changed is that the older exchanges are now a summary of themselves. Drawn against the
    // turn that compacted rather than between two turns, which is why it reads `turn` and not the
    // pair: the CLI decides to summarise before it answers, so this IS the turn it happened in.
    marks.push({ kind: "compacted" });
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
