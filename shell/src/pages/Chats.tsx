import { useEffect, useRef, useState } from "react";
import { Link, useNavigate, useParams } from "@tanstack/react-router";
import { isApiRefusal } from "../data/client";
import {
  useArchiveChat,
  useChatTranscript,
  useChats,
  useCreateChat,
  useIdeConversation,
  useIdeSessions,
  useWireIdeSessionTools,
  useLocalModel,
  usePatchChat,
  usePostChatSeen,
  usePostChatTitle,
  useSendMessage,
  type Brain,
  type ChatSummary,
  type IdeSession,
  type Turn,
} from "../data/chats";
import { anyTurnLive, marksBetween, turnIsLive, unreadTotal, type Mark } from "../lib/turns";
import {
  Badge,
  Button,
  ConfirmButton,
  CostLine,
  ErrorNote,
  PageHeader,
  Panel,
  RefusalNote,
  StaleNote,
  Teach,
} from "../ui";
import "./chats.css";

/**
 * Chats — one master-detail page serving `/chats` and `/chats/$chatId`.
 *
 * The list is the page's spine and is always on screen: `Chats.tsx:81` in
 * `Projects` is the model, and the reason repeats here — navigating between
 * two conversations must never blank the thing you navigated *from*. What
 * changes with `$chatId` is the second column: nothing chosen shows a Teach,
 * something chosen shows its transcript and composer beside the list rather
 * than instead of it.
 *
 * Only the *open* conversation's "thinking…" reading is knowable at all: the
 * list route (`GET /assistant/chats`) carries no turn-level state, and the
 * only route that does (`GET /assistant/chats/{chat_id}`) is fetched for one
 * conversation at a time. So the transcript query is lifted to this
 * top-level component and its liveness is threaded down to the one list row
 * it can actually speak to.
 */
export function Chats() {
  const params = useParams({ strict: false }) as { chatId?: string };
  const chatId = params.chatId ?? null;

  const chats = useChats();
  const transcript = useChatTranscript(chatId);

  const rows = chats.data ?? [];
  const stale = chats.isError && chats.data !== undefined;
  const selectedLive = chatId !== null && anyTurnLive(transcript.data);
  const summary = chatId === null ? undefined : rows.find((row) => row.chat_id === chatId);

  return (
    <>
      <PageHeader title="Chats" headline={headlineFor(rows, chats.data !== undefined)} />

      {stale && <StaleNote dataUpdatedAt={chats.dataUpdatedAt} />}
      {chats.isError && chats.data === undefined && <ListError error={chats.error} />}

      <div className="chats-layout">
        <ChatListPanel
          rows={rows}
          answered={chats.data !== undefined}
          selected={chatId}
          selectedLive={selectedLive}
        />

        <div className="chats-detail">
          {chatId === null && (
            <Teach title="Choose a conversation">
              <p>
                Nothing is open. Pick a conversation from the list, or start a new one — every turn
                is a billed run, so nothing here is ever quietly deleted; ending a conversation only
                archives it, and every turn it ever had stays readable.
              </p>
            </Teach>
          )}
          {chatId !== null && (
            <ChatDetail key={chatId} chatId={chatId} summary={summary} transcript={transcript} />
          )}
        </div>
      </div>
    </>
  );
}

/** One derived sentence about the whole list. */
function headlineFor(rows: ChatSummary[], answered: boolean): string | undefined {
  if (!answered) return undefined;
  if (rows.length === 0) return "no conversation has been opened from this window";
  const noun = rows.length === 1 ? "conversation" : "conversations";
  const unread = unreadTotal(rows);
  return unread === 0 ? `${rows.length} ${noun}, nothing unread` : `${rows.length} ${noun}, ${unread} unread`;
}

function ListError({ error }: { error: unknown }) {
  if (isApiRefusal(error)) return <RefusalNote refusal={error} />;
  return <ErrorNote>the núcleo did not answer — nothing is known about your conversations</ErrorNote>;
}

/* --------------------------------------------------------------- the list -- */

function ChatListPanel({
  rows,
  answered,
  selected,
  selectedLive,
}: {
  rows: ChatSummary[];
  answered: boolean;
  selected: string | null;
  selectedLive: boolean;
}) {
  const [composerOpen, setComposerOpen] = useState(false);
  const navigate = useNavigate();

  return (
    <Panel
      title="Conversations"
      aside={
        <Button variant="approve" aria-pressed={composerOpen} onClick={() => setComposerOpen((v) => !v)}>
          {composerOpen ? "Cancel" : "New conversation"}
        </Button>
      }
    >
      {composerOpen && (
        <NewChatForm
          onOpened={(chatId) => {
            setComposerOpen(false);
            void navigate({ to: `/chats/${chatId}` });
          }}
        />
      )}

      {!answered && <p className="chats-loading">reading your conversations…</p>}
      {answered && rows.length === 0 && (
        <Teach title="No conversations yet">
          <p>
            Telegram&apos;s own conversations do not show up here — nothing has opened a row for them,
            because the only door into this list is the button above. Start one to see it appear.
          </p>
        </Teach>
      )}
      {rows.length > 0 && (
        <ul className="chats-list" aria-label="Conversations">
          {rows.map((row) => (
            <ChatRow
              key={row.chat_id}
              row={row}
              active={row.chat_id === selected}
              live={row.chat_id === selected && selectedLive}
            />
          ))}
        </ul>
      )}
    </Panel>
  );
}

/**
 * What this row is called out loud.
 *
 * Spelled out rather than left to the name computation over the children, for
 * the same reason the sidebar's own nav items are: adjacent inline text —
 * title, brain badge, "thinking…", the unread count — concatenates with no
 * separator, and a screen reader would announce "hello therecloud3" instead
 * of a sentence.
 */
function chatRowLabel(row: ChatSummary, live: boolean): string {
  const parts = [row.title ?? row.first_message ?? "New conversation", row.brain];
  if (live) parts.push("thinking");
  if (row.waiting > 0) parts.push(`${row.waiting} unread`);
  return parts.join(", ");
}

function ChatRow({ row, active, live }: { row: ChatSummary; active: boolean; live: boolean }) {
  return (
    <li className={active ? "chats-row chats-row-active" : "chats-row"}>
      <Link
        className="chats-row-link"
        to={`/chats/${row.chat_id}`}
        aria-label={chatRowLabel(row, live)}
        aria-current={active ? "page" : undefined}
      >
        <span className="chats-row-title">{row.title ?? row.first_message ?? "New conversation"}</span>
        <Badge tone={row.brain === "local" ? "active" : "info"}>{row.brain}</Badge>
        {row.cwd !== null && <span className="chats-row-cwd">{row.cwd}</span>}
        {live && <span className="chats-row-live">thinking…</span>}
        {row.waiting > 0 && (
          <span className="chats-row-unread" aria-hidden="true">
            {row.waiting}
          </span>
        )}
      </Link>
    </li>
  );
}

/* ------------------------------------------------------ new conversation -- */

function NewChatForm({ onOpened }: { onOpened: (chatId: string) => void }) {
  const [brain, setBrain] = useState<Brain>("cloud");
  const [sessionId, setSessionId] = useState("");
  const localModel = useLocalModel();
  const ideSessions = useIdeSessions(true);
  const create = useCreateChat();
  const localUnavailable = localModel.data?.available === false;
  const chosen = (ideSessions.data ?? []).find((session) => session.session_id === sessionId);

  return (
    <form
      className="chats-new"
      onSubmit={(event) => {
        event.preventDefault();
        if (create.isPending) return;
        create.mutate(
          { brain, continueSession: sessionId === "" ? undefined : sessionId },
          { onSuccess: (result) => onOpened(result.chat_id) },
        );
      }}
    >
      <fieldset className="chats-new-brain">
        <legend>Answered by</legend>
        <label>
          <input
            type="radio"
            name="new-chat-brain"
            checked={brain === "cloud"}
            onChange={() => setBrain("cloud")}
          />
          Cloud
        </label>
        <label title={localUnavailable ? "no local model is available on this machine" : undefined}>
          <input
            type="radio"
            name="new-chat-brain"
            checked={brain === "local"}
            disabled={localUnavailable}
            onChange={() => setBrain("local")}
          />
          Local
        </label>
      </fieldset>

      <label className="chats-field">
        <span>Continue an IDE session (optional)</span>
        <select
          aria-label="Continue an IDE session"
          value={sessionId}
          onChange={(event) => setSessionId(event.target.value)}
        >
          <option value="">none — start fresh</option>
          {(ideSessions.data ?? []).map((session: IdeSession) => (
            <option key={session.session_id} value={session.session_id}>
              {(session.title ?? session.session_id) + " — " + session.cwd}
            </option>
          ))}
        </select>
      </label>

      {chosen !== undefined && !chosen.tools && <NoTools session={chosen} />}

      <Button type="submit" intent="go" disabled={create.isPending}>
        Start
      </Button>
      {create.isError && <CreateRefusal error={create.error} />}
    </form>
  );
}

/**
 * What a conversation continued in this session's project would NOT be able to do, and the one
 * press that fixes it.
 *
 * The daemon hands a continued turn the project's tools only where its classifier hook is wired,
 * and a directory without one — every fresh worktree, since `.claude/` is not committed — falls
 * back to the MCP server alone. Continuing a coding conversation there gets a model that cannot
 * open the file being discussed, and nothing said so until after the first turn came back.
 *
 * Said here rather than after the pick-up because here is where it can still change the decision:
 * wire the project, or pick a different session, or go on knowing what you are getting.
 */
function NoTools({ session }: { session: IdeSession }) {
  const wire = useWireIdeSessionTools();

  return (
    <div className="chats-new-notools">
      <p className="chats-new-warning" role="status">
        this session was had in a folder with no núcleo hook — continued here, it can talk about the
        code but <b>cannot read or change any file</b>, and cannot run anything
      </p>
      <Button
        type="button"
        disabled={wire.isPending}
        onClick={() => wire.mutate(session.session_id)}
      >
        Give it the tools
      </Button>
      {wire.isError && <WireRefusal error={wire.error} cwd={session.cwd} />}
    </div>
  );
}

function WireRefusal({ error, cwd }: { error: unknown; cwd: string }) {
  if (!isApiRefusal(error)) {
    return <ErrorNote>the núcleo did not answer — the folder was left alone</ErrorNote>;
  }
  return (
    <RefusalNote
      refusal={error}
      sentences={{
        conflict: `${cwd}/.claude/settings.json could not be read as JSON, so it was left exactly as it is — open it and it will say why`,
        not_found: "that session is not on this machine any more",
      }}
    />
  );
}

function CreateRefusal({ error }: { error: unknown }) {
  if (!isApiRefusal(error)) return <ErrorNote>the núcleo did not answer — nothing was opened</ErrorNote>;
  return (
    <RefusalNote
      refusal={error}
      sentences={{ not_found: "that session is not on this machine — pick another, or start fresh" }}
    />
  );
}

/* -------------------------------------------------------------- the detail -- */

function ChatDetail({
  chatId,
  summary,
  transcript,
}: {
  chatId: string;
  summary: ChatSummary | undefined;
  transcript: ReturnType<typeof useChatTranscript>;
}) {
  const seen = usePostChatSeen();
  const pickedUp = useIdeConversation(summary?.ide_session_id ?? null);
  const markedSeen = useRef(false);

  // Once per chat opened, after the transcript has loaded — not on every poll
  // tick that follows. `markedSeen` is fresh per mount, and `ChatDetail` is
  // keyed on `chatId` by its caller, so opening a different conversation is a
  // fresh mount and a fresh chance to mark it.
  useEffect(() => {
    if (transcript.data !== undefined && !markedSeen.current) {
      markedSeen.current = true;
      seen.mutate(chatId);
    }
  }, [transcript.data, chatId, seen]);

  const stale = transcript.isError && transcript.data !== undefined;

  return (
    <Panel title="Conversation">
      {summary !== undefined && (
        <div className="chats-detail-head">
          <TitleEditor chatId={chatId} title={summary.title} />
          <BrainPicker chatId={chatId} brain={summary.brain} />
          <ArchiveControl chatId={chatId} />
        </div>
      )}

      {stale && <StaleNote dataUpdatedAt={transcript.dataUpdatedAt} />}

      {summary !== undefined && summary.ide_session_id !== null && <PickedUp view={pickedUp} />}

      {transcript.isError && transcript.data === undefined && <TranscriptError error={transcript.error} />}
      {!transcript.isError && transcript.data === undefined && (
        <p className="chats-loading">reading the conversation…</p>
      )}
      {transcript.data !== undefined && (
        <Transcript turns={transcript.data} precededBy={(pickedUp.data ?? []).length > 0} />
      )}

      <Composer chatId={chatId} />
    </Panel>
  );
}

function TranscriptError({ error }: { error: unknown }) {
  if (isApiRefusal(error)) return <RefusalNote refusal={error} sentences={{ not_found: "this conversation is gone or archived" }} />;
  return <ErrorNote>the núcleo did not answer — nothing is known about this conversation</ErrorNote>;
}

/* ----------------------------------------------------------------- title -- */

function TitleEditor({ chatId, title }: { chatId: string; title: string | null }) {
  const [draft, setDraft] = useState(title ?? "");
  const patch = usePatchChat();
  const auto = usePostChatTitle();

  return (
    <div className="chats-title">
      <input
        className="chats-title-input"
        aria-label="Conversation title"
        value={draft}
        onChange={(event) => setDraft(event.target.value)}
      />
      <Button disabled={draft.trim() === "" || patch.isPending} onClick={() => patch.mutate({ chatId, title: draft.trim() })}>
        Rename
      </Button>
      <Button variant="ghost" disabled={auto.isPending} onClick={() => auto.mutate(chatId)}>
        Name it locally
      </Button>
      {patch.isError && <TitleRefusal error={patch.error} />}
      {auto.isError && <AutoTitleRefusal error={auto.error} />}
    </div>
  );
}

function TitleRefusal({ error }: { error: unknown }) {
  if (!isApiRefusal(error)) return <ErrorNote>the núcleo did not answer — the name was not changed</ErrorNote>;
  return (
    <RefusalNote
      refusal={error}
      sentences={{ not_found: "this conversation is gone or archived, so there is nothing left to rename" }}
    />
  );
}

function AutoTitleRefusal({ error }: { error: unknown }) {
  if (!isApiRefusal(error)) return <ErrorNote>the núcleo did not answer — no name was proposed</ErrorNote>;
  return (
    <RefusalNote
      refusal={error}
      sentences={{
        unavailable: "no local model could name this — nothing has changed",
        conflict: "nothing has been said in this conversation yet, so there is nothing to name it after",
      }}
    />
  );
}

/* ----------------------------------------------------------------- brain -- */

function BrainPicker({ chatId, brain }: { chatId: string; brain: Brain }) {
  const patch = usePatchChat();
  const localModel = useLocalModel();
  const localUnavailable = localModel.data?.available === false;

  return (
    <div className="chats-brain">
      <Button
        variant={brain === "cloud" ? "approve" : "ghost"}
        aria-pressed={brain === "cloud"}
        disabled={patch.isPending}
        onClick={() => patch.mutate({ chatId, brain: "cloud" })}
      >
        Cloud
      </Button>
      <Button
        variant={brain === "local" ? "approve" : "ghost"}
        aria-pressed={brain === "local"}
        disabled={patch.isPending || localUnavailable}
        title={localUnavailable ? "no local model is available on this machine" : undefined}
        onClick={() => patch.mutate({ chatId, brain: "local" })}
      >
        Local
      </Button>
      {patch.isError && <BrainRefusal error={patch.error} />}
    </div>
  );
}

function BrainRefusal({ error }: { error: unknown }) {
  if (!isApiRefusal(error)) return <ErrorNote>the núcleo did not answer — the model was not changed</ErrorNote>;
  return (
    <RefusalNote
      refusal={error}
      sentences={{ conflict: "a turn is in flight right now — the model cannot change until it settles" }}
    />
  );
}

/* --------------------------------------------------------------- archive -- */

function ArchiveControl({ chatId }: { chatId: string }) {
  const archive = useArchiveChat();
  const navigate = useNavigate();

  return (
    <div className="chats-archive">
      <ConfirmButton
        label="Archive"
        confirmLabel="Archive — every turn stays readable"
        onConfirm={() => archive.mutate(chatId, { onSuccess: () => void navigate({ to: "/chats" }) })}
      />
      {archive.isError && <ArchiveRefusal error={archive.error} />}
    </div>
  );
}

function ArchiveRefusal({ error }: { error: unknown }) {
  if (!isApiRefusal(error)) return <ErrorNote>the núcleo did not answer — the conversation was not archived</ErrorNote>;
  return <RefusalNote refusal={error} />;
}

/* ------------------------------------------------------- picked up here -- */

/**
 * What was said in the conversation this one was picked up from, drawn above the turns
 * the daemon ran because that is when it happened.
 *
 * Three answers, and they are deliberately three. A transcript this machine no longer
 * has is a 404, and the page then makes NO claim about that half — it draws the turns it
 * does have and says the other half could not be read. An empty list is the different
 * answer, a conversation nobody spoke in, and that one is said out loud. Collapsing the
 * two would tell somebody their conversation was empty because a file moved.
 */
function PickedUp({ view }: { view: ReturnType<typeof useIdeConversation> }) {
  if (view.data === undefined && !view.isError) {
    return <p className="chats-loading">reading what was said in the editor…</p>;
  }
  if (view.data === undefined) {
    return (
      <p className="chats-picked-up-unread">
        what was said in the editor could not be read — only the turns below are shown
      </p>
    );
  }
  if (view.data.length === 0) {
    return (
      <p className="chats-picked-up-cut">
        this was picked up from a conversation in the editor that nobody spoke in.
      </p>
    );
  }
  return (
    <>
      <ul className="chats-said" aria-label="Said in the editor">
        {view.data.map((said, index) => (
          // Keyed by position: these came from a file, in the order they are in it, and
          // nothing here reorders or removes one. A transcript has no id to key by.
          <li
            key={`said-${index}`}
            className={said.by_owner ? "chats-said-line chats-said-owner" : "chats-said-line"}
          >
            <span className="chats-said-who">{said.by_owner ? "you" : "núcleo"}</span>
            {/* Text, never markup — this is somebody else's file. */}
            <p className="chats-said-text">{said.text}</p>
          </li>
        ))}
      </ul>
      <p className="chats-picked-up-cut">
        picked up here — everything above was said in the editor and read back out of its
        own file. None of it was a run, and none of it was billed here.
      </p>
    </>
  );
}

/* ------------------------------------------------------------ transcript -- */

function Transcript({ turns, precededBy }: { turns: Turn[]; precededBy: boolean }) {
  // "nothing has been said yet" is a claim about the whole conversation, and a picked-up
  // one is full of what was said in the editor. Saying it over that is the wrong answer.
  if (turns.length === 0 && precededBy) return null;
  if (turns.length === 0) return <p className="chats-empty">nothing has been said yet.</p>;
  return (
    <ul className="chats-turns" aria-label="Transcript">
      {turns.map((turn, index) => (
        <TurnBlock key={turn.id} turn={turn} previous={index === 0 ? null : turns[index - 1]} />
      ))}
    </ul>
  );
}

function TurnBlock({ turn, previous }: { turn: Turn; previous: Turn | null }) {
  const marks = marksBetween(previous, turn);
  const live = turnIsLive(turn.status);

  return (
    <li className="chats-turn">
      {marks.map((mark, index) => (
        <MarkNote key={index} mark={mark} />
      ))}
      <p className="chats-turn-asked">{turn.asked}</p>
      {live && <p className="chats-turn-live">thinking…</p>}
      {!live && turn.answer !== null && <p className="chats-turn-answer">{turn.answer}</p>}
      {!live && turn.answer === null && (
        <p className="chats-turn-answer chats-turn-answer-empty">no answer recorded</p>
      )}
      <div className="chats-turn-foot">
        <CostLine costUsd={turn.cost_usd} inputTokens={null} outputTokens={null} cachedTokens={null} />
        <span className="chats-turn-id">#{turn.id}</span>
      </div>
    </li>
  );
}

/**
 * The brain and restart marks a transcript draws above one turn.
 *
 * The brain mark's copy is deliberately asymmetric: moving *to* the cloud is
 * about where what you type now goes, and moving *to* the local model is
 * about where the answer comes from — the two directions are not mirror
 * images of the same fact.
 */
function MarkNote({ mark }: { mark: Mark }) {
  if (mark.kind === "restart") {
    return (
      <p className="chats-mark chats-mark-restart" role="status">
        the model past this point does not remember anything above it — the conversation restarted
        here
      </p>
    );
  }
  const text =
    mark.to === "cloud"
      ? "moved to the cloud model — from here, what you type leaves this machine"
      : "moved to the local model — from here, this is answered on this machine";
  return (
    <p className="chats-mark chats-mark-brain" role="status">
      {text}
    </p>
  );
}

/* -------------------------------------------------------------- composer -- */

const MESSAGE_SENTENCES: Record<string, string> = {
  turn_in_progress: "this conversation already has a turn in flight — it clears on its own once that turn answers",
  kill_switch: "the kill switch is engaged; nothing autonomous starts until it is released, and this cannot be sent either",
  no_local_model: "no local model is available on this machine, and this conversation is set to answer locally",
  errand_not_answering: "the errand behind this conversation is not answering right now",
};

function Composer({ chatId }: { chatId: string }) {
  const [text, setText] = useState("");
  const send = useSendMessage(chatId);

  return (
    <form
      className="chats-composer"
      onSubmit={(event) => {
        event.preventDefault();
        if (text.trim() === "" || send.isPending) return;
        send.mutate(text.trim(), { onSuccess: () => setText("") });
      }}
    >
      <label className="chats-field">
        <span>Message</span>
        <textarea
          rows={3}
          aria-label="Message"
          value={text}
          onChange={(event) => setText(event.target.value)}
        />
      </label>
      <div className="chats-composer-actions">
        <Button type="submit" intent="go" disabled={text.trim() === "" || send.isPending}>
          Send
        </Button>
      </div>
      {send.isError && <MessageRefusal error={send.error} />}
    </form>
  );
}

function MessageRefusal({ error }: { error: unknown }) {
  if (!isApiRefusal(error)) return <ErrorNote>the núcleo did not answer — nothing was sent</ErrorNote>;
  return <RefusalNote refusal={error} sentences={MESSAGE_SENTENCES} />;
}
