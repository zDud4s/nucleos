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
    queryFn: async ({ client }): Promise<Turn[]> => {
      const rows = await apiFetch<AssistantTurnRow[]>(
        `/assistant/chats/${encodeURIComponent(chatId ?? "")}`,
      );
      const fresh = rows.map(turnFromRow);
      const local = client.getQueryData<Turn[]>(queryKey) ?? [];
      return merge(fresh, local);
    },
    enabled: chatId !== null,
    refetchInterval: (query) => (anyTurnLive(query.state.data) ? POLL.turn : POLL.fast),
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
export function useIdeSessions(enabled: boolean) {
  return useQuery({
    queryKey: keys.chats.ideSessions,
    queryFn: () => apiFetch<IdeSession[]>("/assistant/ide-sessions"),
    enabled,
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
export function useIdeConversation(sessionId: string | null) {
  return useQuery({
    queryKey: keys.chats.ideSession(sessionId ?? ""),
    queryFn: () =>
      apiFetch<Conversation>(`/assistant/ide-sessions/${encodeURIComponent(sessionId ?? "")}`),
    enabled: sessionId !== null,
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
    mutationFn: (text: string) =>
      apiFetch<{ turn_id: number }>("/assistant/message", {
        method: "POST",
        body: JSON.stringify({ chat_id: chatId, text }),
      }),
    retry: false,
    onSuccess: (result, text) => {
      const optimistic: Turn = {
        id: result.turn_id,
        asked: text,
        answer: null,
        status: "pending",
        cost_usd: null,
        answeredBy: null,
        sessionId: null,
        // Nothing has been sent, so nothing has been measured. The daemon's reading arrives with
        // the turn it belongs to; inventing one here would draw a number this side made up.
        contextFill: null,
        rotatesAt: null,
        // Nothing has been run yet, and this turn has not even reached the CLI. The empty list is
        // the truth about it, not a placeholder — the live view replaces it as calls happen.
        did: [],
      };
      queryClient.setQueryData<Turn[]>(keys.chats.detail(chatId), (current) =>
        merge(current ?? [], [optimistic]),
      );
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
