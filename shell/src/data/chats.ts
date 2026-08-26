import { keepPreviousData, useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import {
  anyTurnLive,
  merge,
  turnFromRow,
  type AssistantTurnRow,
  type Brain,
  type ToolCall,
  type Turn,
} from "../lib/turns";
import { apiFetch, apiText } from "./client";
import { keys } from "./keys";
import { POLL } from "./poll";

/**
 * Chats, as hooks: the list, one conversation's transcript, and the seven
 * writes a person can make — send a message, open a conversation, rename it,
 * change its model, name it locally, mark it read, and archive it.
 *
 * Every shape below was read off `core/src/chats.rs`, `core/src/sessions.rs`
 * and the routes `core/src/http.rs` mounts under `/assistant`, field for
 * field. The pure decisions about a turn — merging, marking, counting — live
 * in `lib/turns.ts`; this file is the seam between those decisions and the
 * daemon.
 */

export type { Brain, ToolCall, Turn };

/** One row of the list — `ChatSummary`, archived excluded, most recently active first. */
export interface ChatSummary {
  chat_id: string;
  title: string | null;
  brain: Brain;
  created_at: string;
  /** Set only when this conversation continues a session had somewhere else. */
  cwd: string | null;
  /**
   * Which conversation had in the editor this one was picked up from, or null when it
   * was opened here.
   *
   * What the window reads the old conversation back by. Not the session the next turn
   * resumes — the daemon replaces that one the first time a context rotates, and this
   * never moves.
   */
  ide_session_id: string | null;
  first_message: string | null;
  last_activity: string | null;
  /** Answers landed since this conversation was last opened. */
  waiting: number;
  /**
   * How many of those another conversation put there rather than something you
   * asked. A subset of `waiting`, never a separate axis.
   *
   * Optional because a daemon older than the column sends no such key — and it
   * reads as zero, which is the honest answer for a daemon with no relays.
   */
  relayed_waiting?: number;
}

/**
 * Where a conversation runs, and whether that gives its turns tools.
 *
 * Two facts and not one, because a directory alone is not enough: the daemon grants tools on a
 * directory whose classifier hook is wired, and every fresh worktree lacks one. A window that read
 * only `cwd` would say "this conversation has a project" about one that still cannot open a file.
 */
export interface ChatProject {
  cwd: string | null;
  tools: boolean;
  /**
   * The session a terminal standing in `cwd` could carry this conversation on
   * in, or `null` when the daemon itself would not resume it.
   *
   * Measured: `claude --resume <this>` from that directory really does continue
   * a conversation the daemon had. The way back was always there — nothing said
   * so, which made it a way back only somebody who reads the daemon could find.
   */
  session: string | null;
  /**
   * Whether this conversation plans without acting.
   *
   * Beside the tools and not beside the title, because it is the same question
   * in the other direction: one says what this conversation CAN do, the other
   * what it will choose not to.
   */
  planning: boolean;
}

/** A conversation already had in the IDE that this daemon could continue. */
export interface IdeSession {
  session_id: string;
  cwd: string;
  title: string | null;
  last_activity: string;
  /**
   * Whether continuing this session would give the model the project's tools.
   *
   * False means its directory has no classifier hook, and a conversation continued there runs on
   * the NucleOS MCP server alone — it cannot read a file, edit one, or run a command. The daemon
   * answers this by asking the same function the turn itself asks, so it is a promise and not a
   * guess. `useWireIdeSessionTools` is what turns a false into a true.
   */
  tools: boolean;
}

/**
 * A turn part way through: what it has written, and what it is doing.
 *
 * Distilled by the daemon rather than by this window. The CLI's stream carries
 * `content_block_delta`s, tool calls and transport events in a format the app
 * does not own and which changes without asking — so `runner.rs` reads it
 * beside the parse that pulls the final reply out, and what arrives here is
 * words.
 */
export interface LiveTurn {
  /** The answer so far. Empty means nothing has been written yet. */
  text: string;
  /** The tool running right now, or null when the model is writing. */
  doing: string | null;
  /** What it has run so far, oldest first. */
  did: ToolCall[];
  /**
   * What it has thought so far — and empty on every stream so far. See
   * `AssistantTurnRow.thought`: the CLI withholds the words.
   */
  thought: string[];
  /** Roughly how many tokens it has spent thinking, or null if it has not. */
  thought_tokens: number | null;
}

/** One thing said in a conversation had in the editor. */
export interface Said {
  /** Whether the owner typed it. The model answered everything else. */
  by_owner: boolean;
  text: string;
  /**
   * Whether this is a note ABOUT the conversation rather than a line OF it.
   *
   * One thing sets it: a subagent worked here, and its rows were dropped — a
   * different conversation, with a different model, that the owner never saw.
   * Drawn as a note and never as a bubble: attributing it to anybody would put
   * words on somebody who did not say them.
   */
  aside: boolean;
}

/**
 * What was said in an editor session, and whether that is all of it.
 *
 * `cut` is not decoration. The daemon reads these files from the recent end under two ceilings, and
 * two hundred messages back looks exactly like a conversation that had two hundred messages — so
 * without being told, a person scrolls up, finds the top, and reads it as the whole thing.
 */
export interface Conversation {
  said: Said[];
  cut: boolean;
  /**
   * Roughly how many tokens continuing this session would carry, or null when
   * the file could not be read. Rough by tens of percent and named so — what it
   * has to be right about is the order of magnitude.
   */
  context_estimate: number | null;
  /** The count past which the daemon stops resuming and starts a fresh context. */
  context_rotates_at: number;
}

/**
 * One exchange a conversation was handed: what was asked, and what was answered.
 *
 * A pair rather than two `Said`s, because that is the unit the daemon carries — an unanswered
 * question is not part of a tail, and a tuple is the shape that cannot hold half of one.
 */
export type Exchange = [string, string];

/**
 * A conversation as it is read back: its turns, and whatever it was handed before the first one.
 *
 * `handed` is empty for every ordinary chat. It is non-empty only where a session picked up from
 * the editor was too large to resume: the daemon started a fresh context and put the verbatim tail
 * of the old one in front of it. Without this the window showed the whole editor conversation and
 * said nothing about how much of it the model actually has — which reads as "it remembers all of
 * this" and is false.
 */
export interface Transcript {
  handed: Exchange[];
  turns: Turn[];
  /**
   * What was said to this conversation while it was busy and has not been sent
   * yet, oldest first, each with the name it can be taken back by.
   *
   * Not turns and never drawn as ones: nothing has run, nothing is billed, and
   * a bubble that looked like a turn would be claiming a run that does not
   * exist. They leave this list by becoming turns, on their own.
   */
  queued: Waiting[];
  /**
   * What this conversation is waiting to be allowed to do, which is nearly
   * always nothing.
   *
   * A turn is HELD while one of these stands: the CLI is sitting on a hook call
   * and the model behind it, so answering is not a preference somebody gets to
   * at their leisure. It travels on the transcript because that is what polls at
   * a turn's own speed while a turn is live, which is exactly when one appears.
   */
  asks: Ask[];
}

/**
 * One tool call this conversation is being held on.
 *
 * `detail` is the one argument worth showing beside the name — a command, a
 * path — and deliberately not the whole input: a `Write` carries the file it is
 * writing, and a window that printed that argument would print the file.
 */
export interface Ask {
  id: string;
  tool: string;
  detail: string | null;
}

/**
 * One message waiting to be said, and the name it can be taken back by.
 *
 * An id and not a position: the daemon sends the front of the queue while a
 * person is looking at it, so "the second one" means something different a
 * moment later — and taking one back by position would take back a message
 * nobody pointed at.
 */
export interface Waiting {
  id: number;
  text: string;
}

/** One name a conversation offers for an `@`, relative to its own directory. */
export interface Mention {
  /** Relative to the conversation's directory, forward-slashed. This is what goes in the message. */
  path: string;
  /** The last component, which is what is being typed at. */
  name: string;
  is_dir: boolean;
}

/** What a conversation offers for an `@`, and whether it had anywhere to look. */
export interface Mentions {
  /**
   * False when this conversation has no working directory, or its directory is gone.
   *
   * The distinction an empty list cannot draw: "nothing matches what you typed" and "there is
   * nowhere to look" look identical to a caller and are entirely different facts. Only the second
   * is worth a sentence.
   */
  rooted: boolean;
  hits: Mention[];
  /** True when a ceiling cut the list, so the window never implies the file is simply not there. */
  truncated: boolean;
}

/** A picture on its way out: base64, with what the browser said it is. */
export interface Attachment {
  media_type: string;
  data: string;
}

/** Where a command came from, which is the only thing explaining two of the same name. */
export type CommandSource = "project" | "personal" | "plugin";

/** One slash command a conversation can run. */
export interface Command {
  /** What is typed after the slash: `name`, `dir:name`, or `plugin:name`. */
  name: string;
  description: string | null;
  /** What the command expects after its name, when its file says. */
  hint: string | null;
  source: CommandSource;
}

/** What `POST /assistant/chats` accepts. Both fields are optional; absent brain means cloud. */
export interface NewChat {
  brain?: Brain;
  /** The id of an IDE session to continue, from `useIdeSessions`. */
  continueSession?: string;
}

/** What `PATCH /assistant/chats/{chat_id}` accepts. Either field, or both. */
export interface ChatPatch {
  chatId: string;
  title?: string;
  brain?: Brain;
}

/* ------------------------------------------------------------------ reads -- */

/**
 * The conversation list.
 *
 * Polled at the fast cadence **in the background**, unlike almost every other
 * list in the app: this is also the sidebar's unread badge, on screen on
 * every page and not only on this one, and a badge that stops counting the
 * moment the window loses focus is a badge lying about what is waiting.
 * `keepPreviousData` for the ordinary reason a list gets it — a roster that
 * blanks on every refetch makes the page flicker once every three seconds.
 */
export function useChats() {
  return useQuery({
    queryKey: keys.chats.all,
    queryFn: () => apiFetch<ChatSummary[]>("/assistant/chats"),
    refetchInterval: POLL.fast,
    refetchIntervalInBackground: true,
    placeholderData: keepPreviousData,
  });
}

/**
 * One conversation's transcript, oldest first.
 *
 * The daemon inserts a turn's row before the request that created it
 * returns, so a history read that overtakes that insert comes back without
 * it — and the message just sent would vanish from the page until the next
 * poll happened to land after the write. `merge` is what stops that: the
 * fresh read is combined with whatever is already in the cache, which is
 * where `useSendMessage` puts a turn the instant the daemon answers with its
 * id, well before the next scheduled fetch.
 *
 * Cadence is 1.5 s while any turn is live and 3 s once the conversation has
 * settled — never off, because a turn can land from Telegram while this
 * window is simply sitting open on the conversation.
 */
export function useChatTranscript(chatId: string | null) {
  const queryKey = keys.chats.detail(chatId ?? "");
  return useQuery({
    queryKey,
    queryFn: async ({ client }): Promise<Transcript> => {
      const read = await apiFetch<{
        handed: Exchange[];
        turns: AssistantTurnRow[];
        queued: Waiting[];
        asks: Ask[];
      }>(
        `/assistant/chats/${encodeURIComponent(chatId ?? "")}`,
      );
      const fresh = read.turns.map(turnFromRow);
      const local = client.getQueryData<Transcript>(queryKey)?.turns ?? [];
      // Defaulted rather than trusted, exactly as the turn fields are: a daemon older than the
      // column answers with turns and no `handed`, and a conversation that will not draw over a
      // missing field is a worse answer than one that draws without the note.
      return {
        handed: read.handed ?? [],
        queued: read.queued ?? [],
        asks: read.asks ?? [],
        turns: merge(fresh, local),
      };
    },
    enabled: chatId !== null,
    refetchInterval: (query) => (anyTurnLive(query.state.data?.turns) ? POLL.turn : POLL.fast),
  });
}

/**
 * Whether this machine has a model that can answer a conversation.
 *
 * Reports what startup resolved and cannot change without a daemon restart,
 * so this is read once and never polled — a live probe would be a second,
 * differently-timed opinion about a fact that is already settled.
 */
export function useLocalModel() {
  return useQuery({
    queryKey: keys.chats.localModel,
    queryFn: () => apiFetch<{ available: boolean }>("/assistant/local-model"),
  });
}

/**
 * The conversations already had in the IDE that this daemon could continue.
 *
 * `enabled` rather than always-on: this is offered only from the "new
 * conversation" picker, and asking for it on every visit to this page would
 * be a directory scan nobody is looking at.
 */
export function useIdeSessions(enabled: boolean, watch = false) {
  return useQuery({
    queryKey: keys.chats.ideSessions,
    queryFn: () => apiFetch<IdeSession[]>("/assistant/ide-sessions"),
    enabled,
    // The daemon re-reads the CLI's own store on every request, deliberately uncached, because it
    // changes whenever a session is typed into. That made the DATA live and left the window showing
    // a photograph: whichever sessions existed at the moment the door was opened, forever.
    //
    // `POLL.fast` is the cadence for "the state of the machine right now", which is what a
    // conversation somebody is in the middle of having is.
    //
    // Opt-in, because the two callers want different things from the same list. The editor door is
    // watching conversations that may be happening; the project form wants the directories in it as
    // suggestions, and a list of folders does not need re-reading every three seconds.
    refetchInterval: watch ? POLL.fast : false,
  });
}

/**
 * Where this conversation runs, and whether that gives it tools.
 *
 * Not polled: it changes when somebody changes it — points the conversation at a project, or wires
 * that project's hook — and both of those are mutations in this window that invalidate it. A timer
 * would be re-stating a filesystem to itself.
 */
/** One conversation on a relayed turn's path. */
export interface ChainStep {
  chat_id: string;
  /** Null on a conversation nobody has named yet. */
  title: string | null;
}

/**
 * The whole path a relayed turn travelled, root first — including the
 * conversation reading it.
 *
 * `enabled` because this is asked on hover and never on load: the transcript is
 * polled every second and a half while a turn is live, and a chain is read when
 * somebody actually wants one. Fetching it for every relayed turn on every poll
 * would pay a query per turn to answer a question almost nobody asks.
 *
 * `staleTime: Infinity` because a chain cannot change. It is the history of one
 * turn, and history does not get rewritten — so once fetched, hovering again
 * costs nothing.
 */
export function useRelayChain(chatId: string, turnId: number, enabled: boolean) {
  return useQuery({
    queryKey: keys.chats.relayChain(chatId, turnId),
    queryFn: () =>
      apiFetch<{ chain: ChainStep[] }>(
        `/assistant/chats/${encodeURIComponent(chatId)}/turns/${turnId}/chain`,
      ),
    enabled,
    staleTime: Infinity,
  });
}

/**
 * Hands a turn to another conversation, as the person rather than as the model.
 *
 * `chatId` is the DESTINATION, matching the daemon's route and every other
 * `/assistant/chats/{id}/...` call in this file. The source is derived from the
 * turn, because a turn belongs to exactly one conversation and sending it twice
 * would be inviting the two to disagree.
 *
 * Both sides are invalidated on success and neither is optimistic. The
 * destination gains a turn this window did not write and cannot predict the id
 * of; the sender gains a `relayed_to` entry only if the daemon actually admitted
 * the hop, which it may not — a cycle, a chain too deep, an owner who has walked
 * away. Drawing either before the answer would be drawing a message that may
 * never have left.
 */
export function useForwardTurn() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: ({
      toChatId,
      fromTurnId,
      text,
    }: {
      toChatId: string;
      fromTurnId: number;
      text: string;
    }) =>
      apiFetch<{ turn_id?: number; queued?: boolean }>(
        `/assistant/chats/${encodeURIComponent(toChatId)}/forward`,
        { method: "POST", body: JSON.stringify({ from_turn_id: fromTurnId, text }) },
      ),
    retry: false,
    onSuccess: (_result, { toChatId }) => {
      void queryClient.invalidateQueries({ queryKey: keys.chats.detail(toChatId) });
      void queryClient.invalidateQueries({ queryKey: keys.chats.all });
    },
  });
}

export function useChatProject(chatId: string) {
  return useQuery({
    queryKey: keys.chats.project(chatId),
    queryFn: () =>
      apiFetch<ChatProject>(`/assistant/chats/${encodeURIComponent(chatId)}/project`),
  });
}

/**
 * What was said in the conversation this one was picked up from, oldest first.
 *
 * Read on the request rather than held, matching the daemon's own posture: the store is
 * the CLI's and changes whenever a session is typed into, so anything kept here would be
 * a second copy of somebody else's truth. Not polled — opening a conversation is a click,
 * not a queue — but the key does sit under `keys.chats.all`, so a mutation in this
 * conversation refetches it, which is the direction that stays right.
 */
export function useIdeConversation(sessionId: string | null, watch = false) {
  return useQuery({
    queryKey: keys.chats.ideSession(sessionId ?? ""),
    queryFn: () =>
      apiFetch<Conversation>(`/assistant/ide-sessions/${encodeURIComponent(sessionId ?? "")}`),
    enabled: sessionId !== null,
    // Opt-in, because the two callers are asking different questions. The editor door is looking at
    // a conversation that may be happening right now and wants to see it move; a conversation that
    // was already picked up is showing where it CAME from, which is settled — polling that would be
    // re-reading somebody's history every few seconds to watch it not change.
    refetchInterval: watch ? POLL.fast : false,
  });
}

/**
 * What is different in this conversation's project, as `git diff` writes it.
 *
 * **`apiText`, never `apiFetch`**: the route answers with a bare string, and a
 * clean tree answers with an empty one — which `apiFetch` would try to parse as
 * JSON and refuse.
 *
 * On demand and never polled, and `enabled` is what makes that true: this walks
 * a working tree, and re-asking it on a timer would do that in the background
 * forever for a panel nobody has opened. Not cached beyond the open either —
 * `staleTime: 0` — because the answer changes the moment the conversation does
 * anything, and a stale diff is a worse answer than a slow one.
 */
export function useChatDiff(chatId: string, enabled: boolean) {
  return useQuery({
    queryKey: keys.chats.diff(chatId),
    queryFn: () => apiText(`/assistant/chats/${encodeURIComponent(chatId)}/diff`),
    enabled,
    staleTime: 0,
  });
}

/* --------------------------------------------------------------- writes -- */

/**
 * Say something in a conversation.
 *
 * `turn_id` comes back before the turn has answered, and is written straight
 * into the transcript's cache as a live turn — `asked` is what was just
 * typed, everything else is unknown until the next read. That optimistic
 * entry is what `useChatTranscript`'s `merge` protects from a history read
 * that has not caught up with it yet.
 */
export function useSendMessage(chatId: string) {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: ({ text, images }: { text: string; images: Attachment[] }) =>
      // `wait_if_busy` is what turns the old 409 into a place in the queue. A person looking at the
      // window would rather their words were kept than be told no and handed back an empty box —
      // the Telegram sidecar, which gives up on a turn after a timeout, would rather be refused,
      // and so it does not ask.
      apiFetch<{ turn_id?: number; queued?: boolean }>("/assistant/message", {
        method: "POST",
        body: JSON.stringify({ chat_id: chatId, text, images, wait_if_busy: true }),
      }),
    retry: false,
    onSuccess: (result, { text }) => {
      // Kept rather than sent: there is no turn to draw, and inventing one would put a bubble on
      // screen for a run that does not exist. The transcript's own read carries what is waiting, so
      // asking for it again is the whole of what this side has to do.
      if (result.turn_id === undefined) {
        void queryClient.invalidateQueries({ queryKey: keys.chats.detail(chatId) });
        void queryClient.invalidateQueries({ queryKey: keys.chats.all });
        return;
      }
      const optimistic: Turn = {
        id: result.turn_id,
        asked: text,
        answer: null,
        status: "pending",
        cost_usd: null,
        answeredBy: null,
        sessionId: null,
        // Not the pictures that were just sent: those are on disk under names only the daemon
        // knows, because it names them after the turn's own id. They arrive with the next read,
        // which is a beat later — and a wrong guess at a path would draw a broken image instead.
        images: [],
        // Nothing has run and nothing has been thought: this turn has not started.
        thought: [],
        thoughtTokens: null,
        // Nothing has been sent, so nothing has been measured. The daemon's reading arrives with
        // the turn it belongs to; inventing one here would draw a number this side made up.
        contextFill: null,
        rotatesAt: null,
        // Nothing has been run yet, and this turn has not even reached the CLI. The empty list is
        // the truth about it, not a placeholder — the live view replaces it as calls happen.
        did: [],
        // Nothing has been relayed by a turn that has not started. The daemon's own read replaces
        // this the moment one is.
        relayedTo: [],
        // Null, and it can be nothing else here: this optimistic row exists because the PERSON at
        // this window just sent the message. A relayed turn is never drawn this way — it is born in
        // another conversation and reaches this one through the daemon's own read.
        relayedFrom: null,
      };
      // A `Transcript`, because that is what this key holds. It used to hold a bare array, and
      // writing the old shape here does not fail a type check — `setQueryData` is TOLD the type —
      // it fails at runtime inside `merge`, on the one gesture the page exists for. What is handed
      // and what is queued are carried through untouched: neither is this write's business, and
      // dropping them would blank the notes above and below the transcript on every send.
      queryClient.setQueryData<Transcript>(keys.chats.detail(chatId), (current) => ({
        handed: current?.handed ?? [],
        queued: current?.queued ?? [],
        // Carried through for the reason the two above are, and it matters more: a question this
        // conversation is being HELD on, blanked by an optimistic write, would take the answer
        // buttons off the screen while the turn behind them went on waiting.
        asks: current?.asks ?? [],
        turns: merge(current?.turns ?? [], [optimistic]),
      }));
      // The list's "thinking…" reading and its `waiting` count both depend on
      // this conversation's state, and a person who just sent a message is
      // looking straight at it — a three-second wait for the badge to agree
      // reads as the shell not having noticed yet.
      void queryClient.invalidateQueries({ queryKey: keys.chats.all });
    },
  });
}

/**
 * Open a conversation. Answers `{ chat_id }` — the id the daemon minted, not
 * one the caller could have chosen.
 */
export function useCreateChat() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (body: NewChat) =>
      apiFetch<{ chat_id: string }>("/assistant/chats", {
        method: "POST",
        body: JSON.stringify({ brain: body.brain, continue_session: body.continueSession }),
      }),
    retry: false,
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: keys.chats.all });
    },
  });
}

/**
 * A turn while it is still being written.
 *
 * `undefined` is the daemon's `204`: nothing is writing. That covers a turn
 * that has ended and a turn this daemon never started, and it is never "the
 * turn said nothing" — so the caller falls back to saying it is thinking,
 * rather than drawing an answer of no words.
 *
 * Enabled only while the turn is live, which is also what stops the poll: a
 * turn that has landed has its answer in the transcript, and asking after it
 * would be asking the daemon to describe something it has already forgotten.
 */
export function useLiveTurn(turnId: number, alive: boolean) {
  return useQuery({
    queryKey: keys.chats.live(turnId),
    queryFn: () => apiFetch<LiveTurn | undefined>(`/assistant/${turnId}/live`),
    enabled: alive,
    refetchInterval: POLL.turn,
  });
}

/**
 * The names this conversation offers for an `@`, for what has been typed so far.
 *
 * `null` means no mention is being typed and nothing is asked at all — the query is disabled rather
 * than fired with an empty string, because those are different questions and only one of them is
 * worth a request.
 *
 * Previous data is kept while the next keystroke resolves. Without it the list empties and refills
 * on every letter, which reads as flickering rather than as narrowing.
 */
export function useChatFiles(chatId: string, query: string | null) {
  return useQuery({
    queryKey: keys.chats.files(chatId, query ?? ""),
    queryFn: () =>
      apiFetch<Mentions>(
        `/assistant/chats/${encodeURIComponent(chatId)}/files?q=${encodeURIComponent(query ?? "")}`,
      ),
    enabled: query !== null,
    placeholderData: keepPreviousData,
    // A checkout does not change between keystrokes. Re-walking it for a query already asked would
    // be a directory walk to learn nothing.
    staleTime: 10_000,
    retry: false,
  });
}

/**
 * The slash commands this conversation can run, narrowed by what has been typed.
 *
 * `null` disables it, exactly as the file completion does: no command is being typed, so nothing is
 * asked. No `rooted` in the answer — a conversation with no directory still has the person's own
 * commands and every installed plugin's, so there is no "nowhere to look" to report.
 */
export function useChatCommands(chatId: string, query: string | null) {
  return useQuery({
    queryKey: keys.chats.commands(chatId, query ?? ""),
    queryFn: () =>
      apiFetch<{ commands: Command[] }>(
        `/assistant/chats/${encodeURIComponent(chatId)}/commands?q=${encodeURIComponent(query ?? "")}`,
      ),
    enabled: query !== null,
    placeholderData: keepPreviousData,
    // Short, not zero: a command is a file somebody may have just written, and a picker that needed
    // a restart to notice it is a picker people stop trusting. Ten seconds is long enough to spare
    // the disk between keystrokes and short enough that a new command shows up while you look.
    staleTime: 10_000,
    retry: false,
  });
}

/**
 * Takes a message back off the queue before it is sent.
 *
 * A 404 is not an error worth showing: it means the daemon sent that message a moment before the
 * click landed, which is a race a person loses harmlessly. Either way the transcript is asked
 * again, and either way what is on screen becomes true.
 */
export function useDropQueued(chatId: string) {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (queuedId: number) =>
      apiFetch<void>(
        `/assistant/chats/${encodeURIComponent(chatId)}/queue/${queuedId}`,
        { method: "DELETE" },
      ),
    retry: false,
    onSettled: () => {
      void queryClient.invalidateQueries({ queryKey: keys.chats.detail(chatId) });
      void queryClient.invalidateQueries({ queryKey: keys.chats.all });
    },
  });
}

/**
 * Point a conversation at the project it is about.
 *
 * The only way a conversation opened here ever gets tools. The daemon refuses a path that is not an
 * absolute directory, so a typo comes back as a refusal rather than as a conversation that looks
 * fine until somebody asks it to read a file.
 *
 * It also drops the session the conversation was on, which is why the transcript is invalidated
 * too: the next turn starts a fresh one, in the new tree.
 */
export function useSetChatProject(chatId: string) {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (cwd: string) =>
      apiFetch<void>(`/assistant/chats/${encodeURIComponent(chatId)}`, {
        method: "PATCH",
        body: JSON.stringify({ cwd }),
      }),
    retry: false,
    onSettled: () => {
      void queryClient.invalidateQueries({ queryKey: keys.chats.project(chatId) });
      void queryClient.invalidateQueries({ queryKey: keys.chats.all });
    },
  });
}

/**
 * Put this conversation into planning, or take it out.
 *
 * A state and not a per-turn choice, because that is the shape the gesture has:
 * somebody says "plan this", reads it, then says "go". Two turns, one decision,
 * held between them.
 *
 * The project read is invalidated because it carries the flag, and the chat list
 * because a conversation that will not act is a different thing to be looking at.
 */
export function useSetPlanning(chatId: string) {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (planning: boolean) =>
      apiFetch<void>(`/assistant/chats/${encodeURIComponent(chatId)}`, {
        method: "PATCH",
        body: JSON.stringify({ plan_only: planning }),
      }),
    retry: false,
    onSettled: () => {
      void queryClient.invalidateQueries({ queryKey: keys.chats.project(chatId) });
      void queryClient.invalidateQueries({ queryKey: keys.chats.all });
    },
  });
}

/**
 * Wire the classifier hook in this conversation's project, which is what turns talk into tools.
 *
 * The same act the editor door offers before a pick-up, reached from the other side: there it is a
 * session that has a directory, here a conversation that was given one.
 */
export function useWireChatTools(chatId: string) {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: () =>
      apiFetch<void>(`/assistant/chats/${encodeURIComponent(chatId)}/tools`, { method: "POST" }),
    retry: false,
    onSettled: () => {
      void queryClient.invalidateQueries({ queryKey: keys.chats.project(chatId) });
    },
  });
}

/**
 * Say whether a held tool call may go ahead.
 *
 * There is a turn waiting on this answer, and a window of about forty-five
 * seconds before the daemon refuses on its own — so the transcript is
 * invalidated at once rather than on the next poll, and a 404 (the question
 * timed out, or the turn moved on) is not worth showing: the refetch that
 * follows says so by the question no longer being there.
 */
export function useAnswerAsk(chatId: string) {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: ({ id, allow }: { id: string; allow: boolean }) =>
      apiFetch<void>(`/assistant/asks/${encodeURIComponent(id)}`, {
        method: "POST",
        body: JSON.stringify({ allow }),
      }),
    retry: false,
    onSettled: () => {
      void queryClient.invalidateQueries({ queryKey: keys.chats.detail(chatId) });
    },
  });
}

/**
 * Stop a turn that is running.
 *
 * `POST /runs/{id}/cancel`, because a turn IS a run and that route has always
 * existed — what was missing was anywhere to press it from. The daemon aborts
 * the task, which drops the guard that holds the conversation's turn slot, so
 * the chat is answerable again immediately rather than after the run timeout.
 *
 * Both the transcript and the list are invalidated: the turn's row becomes
 * `cancelled`, and the list carries the ordering and the unread count, which
 * both move when a turn stops moving.
 */
export function useStopTurn(chatId: string) {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (turnId: number) => apiText(`/runs/${turnId}/cancel`, { method: "POST" }),
    retry: false,
    onSettled: () => {
      void queryClient.invalidateQueries({ queryKey: keys.chats.detail(chatId) });
      void queryClient.invalidateQueries({ queryKey: keys.chats.all });
    },
  });
}

/**
 * Give a project's tools to the conversations continued out of it.
 *
 * Writes this daemon's classifier hook into the directory the session was had
 * in — a real change to a folder the app does not own, which is why it is a
 * button somebody presses rather than something that happens on pick-up.
 *
 * The session list is invalidated on success because that is where the answer
 * shows: the row stops offering to fix what is now fixed.
 */
export function useWireIdeSessionTools() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (sessionId: string) =>
      apiFetch<void>(`/assistant/ide-sessions/${encodeURIComponent(sessionId)}/tools`, {
        method: "POST",
      }),
    retry: false,
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: keys.chats.ideSessions });
    },
  });
}

/**
 * Rename a conversation, change which model answers it, or both. `204`, so
 * no body.
 *
 * A brain change also drops the daemon's resumable session for this
 * conversation — the model taking over has not seen the turns the other one
 * answered — so the transcript is invalidated alongside the list: the next
 * turn's `session_id` will differ from the one before it, which is exactly
 * the restart `marksBetween` draws.
 */
export function usePatchChat() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: ({ chatId, title, brain }: ChatPatch) =>
      apiFetch<void>(`/assistant/chats/${encodeURIComponent(chatId)}`, {
        method: "PATCH",
        body: JSON.stringify({ title, brain }),
      }),
    retry: false,
    onSuccess: (_data, variables) => {
      void queryClient.invalidateQueries({ queryKey: keys.chats.all });
      void queryClient.invalidateQueries({ queryKey: keys.chats.detail(variables.chatId) });
    },
  });
}

/**
 * Ask the local model to name this conversation from what has been said in
 * it so far. `204`, so no body — the new title is read back from the list or
 * the transcript's next fetch, not returned here.
 */
export function usePostChatTitle() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (chatId: string) =>
      apiFetch<void>(`/assistant/chats/${encodeURIComponent(chatId)}/title`, { method: "POST" }),
    retry: false,
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: keys.chats.all });
    },
  });
}

/**
 * Mark a conversation read. Takes no body, and is invalidated into the list
 * so the badge and this row's own unread count both agree with the click
 * that caused them, rather than waiting for the next poll tick.
 */
export function usePostChatSeen() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (chatId: string) =>
      apiFetch<void>(`/assistant/chats/${encodeURIComponent(chatId)}/seen`, { method: "POST" }),
    retry: false,
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: keys.chats.all });
    },
  });
}

/**
 * Take a conversation off the list. Archive, never delete — every turn is a
 * billed run, and stays readable at `GET /assistant/chats/{chat_id}` after
 * this.
 */
export function useArchiveChat() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (chatId: string) =>
      apiFetch<void>(`/assistant/chats/${encodeURIComponent(chatId)}`, { method: "DELETE" }),
    retry: false,
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: keys.chats.all });
    },
  });
}
