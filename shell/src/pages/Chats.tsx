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
  useLiveTurn,
  useLocalModel,
  usePatchChat,
  usePostChatSeen,
  usePostChatTitle,
  useSendMessage,
  useStopTurn,
  type Brain,
  type ChatSummary,
  type IdeSession,
  type ToolCall,
  type Turn,
} from "../data/chats";
import { anyTurnLive, marksBetween, turnIsLive, unreadTotal, type Mark } from "../lib/turns";
import { blocks, lines, type Line as RichLine } from "../lib/rich";
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
  const [editorOpen, setEditorOpen] = useState(false);
  const navigate = useNavigate();

  const opened = (chatId: string) => {
    setComposerOpen(false);
    setEditorOpen(false);
    void navigate({ to: `/chats/${chatId}` });
  };

  return (
    <Panel
      title="Conversations"
      aside={
        <>
          <Button
            variant="ghost"
            aria-pressed={editorOpen}
            onClick={() => {
              setEditorOpen((v) => !v);
              setComposerOpen(false);
            }}
          >
            {editorOpen ? "Close" : "From the editor"}
          </Button>
          <Button
            variant="approve"
            aria-pressed={composerOpen}
            onClick={() => {
              setComposerOpen((v) => !v);
              setEditorOpen(false);
            }}
          >
            {composerOpen ? "Cancel" : "New conversation"}
          </Button>
        </>
      }
    >
      {composerOpen && <NewChatForm onOpened={opened} />}
      {editorOpen && <FromTheEditor onOpened={opened} />}

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
  const localModel = useLocalModel();
  const create = useCreateChat();
  const localUnavailable = localModel.data?.available === false;

  return (
    <form
      className="chats-new"
      onSubmit={(event) => {
        event.preventDefault();
        if (create.isPending) return;
        create.mutate({ brain }, { onSuccess: (result) => onOpened(result.chat_id) });
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

      <Button type="submit" intent="go" disabled={create.isPending}>
        Start
      </Button>
      {create.isError && <CreateRefusal error={create.error} />}
    </form>
  );
}

/**
 * The conversations you were having in the editor, and the one press that continues one here.
 *
 * A door of its own. Everything this page could already do with an editor session sat inside the
 * new-conversation form, in a field marked optional, below two radio buttons — reachable only by
 * somebody who had pressed a button labelled "New conversation" while looking for an old one. The
 * feature was complete and invisible, which from the outside is indistinguishable from missing.
 *
 * What was said is shown BEFORE the pick-up, not after. A cut title and a directory is not enough
 * to tell two afternoons of work apart, and the only way to find out which one this was used to be
 * to pick it up and read what came back.
 *
 * Cloud, and no choice offered. Continuing one of these means resuming a Claude Code session by its
 * id, which is a thing only the cloud brain can do; a Local option here would be a button that
 * quietly starts a fresh conversation instead of the one you chose.
 */
function FromTheEditor({ onOpened }: { onOpened: (chatId: string) => void }) {
  const sessions = useIdeSessions(true);
  const [sessionId, setSessionId] = useState<string | null>(null);
  const said = useIdeConversation(sessionId);
  const create = useCreateChat();
  const chosen = (sessions.data ?? []).find((session) => session.session_id === sessionId);

  return (
    <div className="chats-editor">
      {sessions.data === undefined && !sessions.isError && (
        <p className="chats-loading">reading your editor sessions…</p>
      )}
      {sessions.isError && <ErrorNote>your editor sessions could not be read</ErrorNote>}
      {sessions.data?.length === 0 && (
        <Teach title="No conversations from the editor">
          <p>
            Nothing on this machine has a transcript the daemon can read. These are the sessions the
            CLI writes as you work in a project — have one there and it shows up here.
          </p>
        </Teach>
      )}

      {(sessions.data ?? []).length > 0 && (
        <ul className="chats-editor-list" aria-label="Conversations in the editor">
          {(sessions.data ?? []).map((session: IdeSession) => (
            <li key={session.session_id}>
              <button
                type="button"
                className={
                  session.session_id === sessionId
                    ? "chats-editor-row chats-editor-row-open"
                    : "chats-editor-row"
                }
                aria-pressed={session.session_id === sessionId}
                onClick={() =>
                  setSessionId((open) => (open === session.session_id ? null : session.session_id))
                }
              >
                <span className="chats-editor-title">{session.title ?? session.session_id}</span>
                <span className="chats-editor-where">{session.cwd}</span>
              </button>
            </li>
          ))}
        </ul>
      )}

      {chosen !== undefined && (
        <div className="chats-editor-chosen">
          <PickedUp view={said} />
          {!chosen.tools && <NoTools session={chosen} />}
          <Button
            type="button"
            intent="go"
            disabled={create.isPending}
            onClick={() =>
              create.mutate(
                { brain: "cloud", continueSession: chosen.session_id },
                { onSuccess: (result) => onOpened(result.chat_id) },
              )
            }
          >
            Pick it up
          </Button>
          {create.isError && <CreateRefusal error={create.error} />}
        </div>
      )}
    </div>
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
        <Transcript
          turns={transcript.data}
          precededBy={(pickedUp.data ?? []).length > 0}
          chatId={chatId}
        />
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
            {/* Text, never markup — this is somebody else's file. `Rich` never emits either:
                it returns data and this page decides what an element is. */}
            {said.by_owner ? (
              <p className="chats-said-text">{said.text}</p>
            ) : (
              <div className="chats-said-text">
                <Rich text={said.text} />
              </div>
            )}
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

function Transcript({
  turns,
  precededBy,
  chatId,
}: {
  turns: Turn[];
  precededBy: boolean;
  chatId: string;
}) {
  const end = useRef<HTMLDivElement | null>(null);
  const last = turns.length === 0 ? null : turns[turns.length - 1];

  // A conversation is read at its end.
  //
  // Opening one at the top means scrolling past an afternoon of work to reach the sentence you came
  // back for, and on a conversation picked up from the editor that is somebody else's whole day
  // above the two turns you just had. Before the hooks below it, and above the early returns: the
  // rules of hooks do not bend for a component that sometimes has nothing to draw.
  //
  // `last?.status` alongside the count, because a turn that ENDS grows the page without adding a
  // row to it — the answer lands where "thinking…" was, and the bottom moves.
  useEffect(() => {
    end.current?.scrollIntoView({ block: "end" });
  }, [chatId, turns.length, last?.status]);

  // "nothing has been said yet" is a claim about the whole conversation, and a picked-up
  // one is full of what was said in the editor. Saying it over that is the wrong answer.
  if (turns.length === 0 && precededBy) return null;
  if (turns.length === 0) return <p className="chats-empty">nothing has been said yet.</p>;
  return (
    <>
      <ul className="chats-turns" aria-label="Transcript">
        {turns.map((turn, index) => (
          <TurnBlock
            key={turn.id}
            turn={turn}
            previous={index === 0 ? null : turns[index - 1]}
            chatId={chatId}
          />
        ))}
      </ul>
      <div ref={end} className="chats-turns-end" />
    </>
  );
}

function TurnBlock({
  turn,
  previous,
  chatId,
}: {
  turn: Turn;
  previous: Turn | null;
  chatId: string;
}) {
  const marks = marksBetween(previous, turn);
  const live = turnIsLive(turn.status);

  return (
    <li className="chats-turn">
      {marks.map((mark, index) => (
        <MarkNote key={index} mark={mark} />
      ))}
      <p className="chats-turn-who">you</p>
      {/* Verbatim, and not through `Rich`: their half is not markdown and is not read as any.
          Somebody who types two asterisks meant two asterisks, and a message redrawn as bold is a
          message they did not send. */}
      <p className="chats-turn-asked">{turn.asked}</p>
      <p className="chats-turn-who">núcleo</p>
      {live && <LiveAnswer turnId={turn.id} />}
      {live && <StopTurn chatId={chatId} turnId={turn.id} />}
      {!live && <WhatItDid did={turn.did} />}
      {!live && turn.answer !== null && (
        <div className="chats-turn-answer">
          <Rich text={turn.answer} />
        </div>
      )}
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
 * A model's answer, drawn as the shapes it was written in.
 *
 * The parser is in `lib/rich.ts` and returns data, never markup; every element below is chosen
 * here, from a closed set. So a transcript containing a script tag is a string containing a script
 * tag at every step of this, and there is no path by which one talks this into rendering HTML.
 *
 * Only the model's half goes through it. What a person typed is drawn exactly as they typed it.
 */
function Rich({ text }: { text: string }) {
  return (
    <>
      {blocks(text).map((block, index) =>
        block.kind === "code" ? (
          <pre key={index} className="chats-code">
            <code>{block.text}</code>
          </pre>
        ) : (
          <div key={index} className="chats-prose">
            {lines(block.text).map((line, at) => (
              <RichLineOut key={at} line={line} />
            ))}
          </div>
        ),
      )}
    </>
  );
}

function RichLineOut({ line }: { line: RichLine }) {
  const inner = line.spans.map((span, index) =>
    span.kind === "code" ? (
      <code key={index}>{span.text}</code>
    ) : span.kind === "strong" ? (
      <strong key={index}>{span.text}</strong>
    ) : (
      <span key={index}>{span.text}</span>
    ),
  );
  if (line.kind === "heading") {
    return <p className={`chats-rich-heading chats-rich-heading-${line.level}`}>{inner}</p>;
  }
  if (line.kind === "bullet") {
    return (
      <p className="chats-rich-bullet">
        <span aria-hidden="true">•</span>
        {inner}
      </p>
    );
  }
  return <p className="chats-rich-line">{inner}</p>;
}

/**
 * The brain and restart marks a transcript draws above one turn.
 *
 * The brain mark's copy is deliberately asymmetric: moving *to* the cloud is
 * about where what you type now goes, and moving *to* the local model is
 * about where the answer comes from — the two directions are not mirror
 * images of the same fact.
 */
/**
 * A turn as it happens: the words so far, and what it is doing between them.
 *
 * Its own component so the poll lives and dies with the live turn — mounted only where `TurnBlock`
 * has decided the turn is in flight, so a settled conversation asks the daemon nothing at all.
 *
 * Three states, and they are different claims. Nothing written and no tool is "thinking…", which is
 * what this said before and is still the honest answer while the daemon has nothing to show. A tool
 * running is named, because "thinking" over a command that is compiling something is the wrong word
 * for the wait. And words already written are shown as they arrive.
 */
/**
 * The way out of a turn that is going nowhere.
 *
 * Offered only while the turn is live, because that is the only time it means anything: cancelling
 * a run that has already landed would be asking the daemon to un-bill it.
 *
 * It is not a refusal of the answer — the turn is a run and stays in the history, cancelled, with
 * whatever it cost up to that point. That is the honest record and it is why this says "stop"
 * rather than "undo".
 */
function StopTurn({ chatId, turnId }: { chatId: string; turnId: number }) {
  const stop = useStopTurn(chatId);
  return (
    <Button type="button" variant="ghost" disabled={stop.isPending} onClick={() => stop.mutate(turnId)}>
      Stop
    </Button>
  );
}

function LiveAnswer({ turnId }: { turnId: number }) {
  const live = useLiveTurn(turnId, true);
  const text = live.data?.text ?? "";
  const doing = live.data?.doing ?? null;
  const end = useRef<HTMLParagraphElement | null>(null);

  // Follows itself down. The list above does not re-render while a turn writes -- the words arrive
  // on this component's own poll -- so the transcript's scroll effect never fires for any of it.
  useEffect(() => {
    end.current?.scrollIntoView({ block: "end" });
  }, [text, doing]);

  return (
    <>
      {text !== "" && <p className="chats-turn-answer chats-turn-writing">{text}</p>}
      <WhatItDid did={live.data?.did ?? []} />
      <p className="chats-turn-live" ref={end}>
        {doing !== null ? `running ${doing}…` : text === "" ? "thinking…" : "writing…"}
      </p>
    </>
  );
}

/**
 * What the turn ran, under what it said.
 *
 * A model that read four files and ran the tests, and one that answered from memory, write the same
 * shape of reply — and on a conversation picked up from the editor, which of the two happened is
 * most of what a person is asking. Nothing on this page said it before.
 *
 * Absent rather than empty when there is nothing: a heading over no rows reads as a turn whose
 * actions failed to load, which is a different and worse claim than a turn that acted on nothing.
 */
function WhatItDid({ did }: { did: ToolCall[] }) {
  if (did.length === 0) return null;
  return (
    <ul className="chats-turn-did" aria-label="What it did">
      {did.map((call, index) => (
        // Keyed by position: this is a record of what happened, in order, and nothing reorders or
        // removes an entry. The same tool on the same file twice is two real calls, not a duplicate.
        <li key={`${call.name}-${index}`}>
          <span className="chats-turn-did-name">{call.name}</span>
          {call.detail !== null && <span className="chats-turn-did-detail">{call.detail}</span>}
        </li>
      ))}
    </ul>
  );
}

function MarkNote({ mark }: { mark: Mark }) {
  if (mark.kind === "restart") {
    return (
      <p className="chats-mark chats-mark-restart" role="status">
        the conversation restarted here — the model past this point was read the last few exchanges
        back, and remembers nothing older than those
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
