import { useEffect, useRef, useState } from "react";
import { Link, useNavigate, useParams } from "@tanstack/react-router";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuLabel,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "../ui/vendor/dropdown-menu";
import {
  CommandDialog,
  CommandEmpty,
  CommandGroup,
  CommandInput,
  CommandItem,
  CommandList,
} from "../ui/vendor/command";
import { isApiRefusal } from "../data/client";
import {
  useArchiveChat,
  useChatTranscript,
  useChats,
  useCreateChat,
  useIdeConversation,
  useIdeSessions,
  useSetChatProject,
  useSetPlanning,
  useWireChatTools,
  useWireIdeSessionTools,
  useAnswerAsk,
  useChatCommands,
  useChatDiff,
  useChatFiles,
  useChatProject,
  useDropQueued,
  useLiveTurn,
  useLocalModel,
  usePatchChat,
  usePostChatSeen,
  usePostChatTitle,
  useSendMessage,
  useStopTurn,
  type Brain,
  type Attachment,
  type ChatSummary,
  type Command,
  type Exchange,
  type Ask,
  type Mention,
  type Waiting,
  type IdeSession,
  type ToolCall,
  type Turn,
} from "../data/chats";
import {
  anyTurnLive,
  marksBetween,
  planOf,
  turnIsLive,
  unreadTotal,
  type Mark,
  type Todo,
} from "../lib/turns";
import { blocks, lines, type Line as RichLine } from "../lib/rich";
import { commandAt, mentionAt, withCommand, withMention } from "../lib/mention";
import { fetchFileBlob } from "../data/files";
import { attachmentFrom, isPicture } from "../lib/picture";
import { stillGoing } from "../lib/editor";
import { diffLines } from "../lib/diff";
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
  const selectedLive = chatId !== null && anyTurnLive(transcript.data?.turns);
  const summary = chatId === null ? undefined : rows.find((row) => row.chat_id === chatId);

  /**
   * The list is a panel you open, not a column you live with.
   *
   * Open by default, because somebody arriving at this page for the first time has to
   * be able to find a conversation without knowing a shortcut. Closed, the transcript
   * gets the whole width, which is what a page made of prose wants.
   */
  const [railOpen, setRailOpen] = useState(true);
  const [paletteOpen, setPaletteOpen] = useState(false);
  const unseen = rows.reduce((total, row) => total + row.waiting, 0);

  /**
   * Ctrl+K, and Cmd+K for the same fingers on a Mac keyboard.
   *
   * On `window` rather than on a container because the point of it is to work while
   * the caret is in the composer, which is where it will be nearly every time.
   * `preventDefault` because Ctrl+K is a browser shortcut and the webview would
   * otherwise act on it as well.
   */
  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      if (event.key.toLowerCase() !== "k" || !(event.ctrlKey || event.metaKey)) return;
      event.preventDefault();
      setPaletteOpen((open) => !open);
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  return (
    <>
      <PageHeader
        title="Chats"
        headline={headlineFor(rows, chats.data !== undefined)}
        actions={
          <>
            <Button
              variant="ghost"
              aria-pressed={railOpen}
              /* Spelled out rather than left to the name computation over the
                 children, for the same reason the nav items and the chat rows are:
                 "Conversations" and a count in an adjacent span concatenate with no
                 separator, and a screen reader would announce "Conversations5". */
              aria-label={
                railOpen
                  ? "Hide conversations"
                  : unseen > 0
                    ? `Conversations, ${unseen} unseen`
                    : "Conversations"
              }
              onClick={() => setRailOpen((open) => !open)}
            >
              {railOpen ? "Hide conversations" : "Conversations"}
              {/* Answers that landed while you were elsewhere. Shown on the button
                  precisely because the list they are in may be closed — a count that
                  only appears once the list is open tells you what you already see. */}
              {!railOpen && unseen > 0 && <span className="chats-unseen">{unseen}</span>}
            </Button>
            <Button variant="ghost" onClick={() => setPaletteOpen(true)}>
              Find a conversation
              <kbd className="chats-kbd">Ctrl K</kbd>
            </Button>
          </>
        }
      />

      {stale && <StaleNote dataUpdatedAt={chats.dataUpdatedAt} />}
      {chats.isError && chats.data === undefined && <ListError error={chats.error} />}

      <ConversationPalette rows={rows} open={paletteOpen} onOpenChange={setPaletteOpen} />

      <div className={railOpen ? "chats-layout" : "chats-layout chats-layout-alone"}>
        {railOpen && (
          <ChatListPanel
            rows={rows}
            answered={chats.data !== undefined}
            selected={chatId}
            selectedLive={selectedLive}
          />
        )}

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

/**
 * Find a conversation by typing its name, rather than by reading down a list.
 *
 * The list panel answers "what have I got"; this answers "where is the one I mean",
 * and past a couple of dozen conversations those stop being the same question. It is
 * also what makes closing the list a real option rather than a way to lose things.
 *
 * The searchable text is the title AND the directory, because half of these are
 * remembered as "the one about the shell" rather than by whatever the daemon titled
 * them.
 */
function ConversationPalette({
  rows,
  open,
  onOpenChange,
}: {
  rows: ChatSummary[];
  open: boolean;
  onOpenChange: (open: boolean) => void;
}) {
  const navigate = useNavigate();

  return (
    <CommandDialog
      open={open}
      onOpenChange={onOpenChange}
      title="Find a conversation"
      description="Type to narrow the list. Enter opens the one highlighted."
      /* Escape closes it, and a palette is a thing you dismiss rather than
         close — the corner X is clutter that also has to be styled. */
      showCloseButton={false}
    >
      <CommandInput placeholder="Find a conversation…" />
      <CommandList>
        <CommandEmpty>No conversation matches that.</CommandEmpty>
        <CommandGroup>
          {rows.map((row) => {
            const name = row.title ?? row.first_message ?? "New conversation";
            return (
              <CommandItem
                key={row.chat_id}
                value={`${name} ${row.cwd ?? ""}`}
                onSelect={() => {
                  onOpenChange(false);
                  void navigate({ to: `/chats/${row.chat_id}` });
                }}
              >
                <span className="chats-palette-title">{name}</span>
                {row.cwd !== null && <span className="chats-palette-where">{row.cwd}</span>}
                {row.waiting > 0 && <span className="chats-palette-waiting">{row.waiting}</span>}
              </CommandItem>
            );
          })}
        </CommandGroup>
      </CommandList>
    </CommandDialog>
  );
}

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
 * What continuing this session would carry, and whether that is more than the daemon will resume.
 *
 * The question a person is actually asking at this button is "what happens if I press it", and
 * until now nothing answered it. A real pick-up of a session sitting around 180k of context resumed
 * blindly and billed $1.72 for a one-word answer: a resume re-sends the whole window as fresh
 * input, and a session last touched days ago has nothing cached to make that cheap.
 *
 * Two different futures, said plainly rather than as a number to interpret. Under the ceiling it is
 * continued where it left off, and costs what its context costs. Over it, the daemon refuses to
 * resume and starts fresh with a short replay — cheap, and forgetful, and better known in advance.
 */
function WhatItCarries({ view }: { view: ReturnType<typeof useIdeConversation> }) {
  const carries = view.data?.context_estimate ?? null;
  if (carries === null) return null;
  const ceiling = view.data?.context_rotates_at ?? null;
  const k = (n: number) => `${(n / 1000).toFixed(1)}k`;
  const over = ceiling !== null && carries > ceiling;
  return (
    <p className={over ? "chats-carries chats-carries-over" : "chats-carries"}>
      {`about ${k(carries)} of context`}
      {over
        ? " — past what this daemon resumes, so picking it up starts a fresh conversation with a short replay"
        : " — picked up where it left off, and its context is re-sent on the first turn"}
    </p>
  );
}

/**
 * The tail of a conversation, enough to recognise it by.
 *
 * NOT `PickedUp`, which draws the whole thing. This is a 20rem column beside the conversation list,
 * and rendering two hundred messages into it made the panel taller than the page and spilled the
 * preview out from under its own border. What a person is doing here is telling two afternoons
 * apart, and the last few lines do that.
 *
 * The END of it, because that is where a conversation is picked up from — the top of a long session
 * is the part nobody is coming back for.
 */
const SAMPLED = 6;

function Sample({ view }: { view: ReturnType<typeof useIdeConversation> }) {
  if (view.data === undefined && !view.isError) {
    return <p className="chats-loading">reading what was said in the editor…</p>;
  }
  if (view.data === undefined) {
    return <p className="chats-picked-up-unread">what was said in the editor could not be read</p>;
  }
  if (view.data.said.length === 0) {
    return <p className="chats-picked-up-cut">nobody spoke in this one.</p>;
  }
  const tail = view.data.said.slice(-SAMPLED);
  return (
    <>
      {(view.data.cut || tail.length < view.data.said.length) && (
        <p className="chats-picked-up-cut">the last {tail.length} of it — the rest opens with it</p>
      )}
      <ul className="chats-sample" aria-label="What was said, at the end">
        {tail.map((said, index) => (
          <li
            key={`sample-${index}`}
            className={said.aside ? "chats-sample-line chats-sample-aside" : "chats-sample-line"}
          >
            {!said.aside && (
              <span className="chats-said-who">{said.by_owner ? "you" : "núcleo"}</span>
            )}
            {/* Text, never markup, and never `Rich` either: a sample is for recognising a
                conversation, and a code block in a 20rem column is not that. */}
            <p className="chats-sample-text">{said.text}</p>
          </li>
        ))}
      </ul>
    </>
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
  const sessions = useIdeSessions(true, true);
  const [sessionId, setSessionId] = useState<string | null>(null);
  // Watched, not merely read: this is the one panel where the conversation on screen may be being
  // typed into while somebody looks at it.
  const said = useIdeConversation(sessionId, true);
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
                {/* Inside the button, so the mark is part of the row's own name and a screen reader
                    hears "happening now" along with the title rather than after a silence. */}
                {stillGoing(session.last_activity, Date.now()) && (
                  <span className="chats-editor-live">happening now</span>
                )}
              </button>
            </li>
          ))}
        </ul>
      )}

      {chosen !== undefined && (
        <div className="chats-editor-chosen">
          <WhatItCarries view={said} />
          <Sample view={said} />
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
          <ChatMeta chatId={chatId} brain={summary.brain} />
        </div>
      )}

      <Project chatId={chatId} />

      {stale && <StaleNote dataUpdatedAt={transcript.dataUpdatedAt} />}

      {summary !== undefined && summary.ide_session_id !== null && (
        <PickedUp view={pickedUp} handed={transcript.data?.handed ?? []} />
      )}

      {transcript.isError && transcript.data === undefined && <TranscriptError error={transcript.error} />}
      {!transcript.isError && transcript.data === undefined && (
        <p className="chats-loading">reading the conversation…</p>
      )}
      {transcript.data !== undefined && (
        <Transcript
          turns={transcript.data.turns}
          precededBy={(pickedUp.data?.said ?? []).length > 0}
          chatId={chatId}
        />
      )}
      {/* Below the transcript and above the box, which is where these words are in time: said
          after everything above them, and not yet said at all. */}
      {/* Above what is waiting to be said, because this is what everything else is waiting ON:
          a turn is held while a question stands, and the queue behind it cannot move until it is
          answered. */}
      <Asking asks={transcript.data?.asks ?? []} chatId={chatId} />
      <Changed chatId={chatId} />
      <Waiting queued={transcript.data?.queued ?? []} chatId={chatId} />

      <Composer chatId={chatId} />
    </Panel>
  );
}

/**
 * Where this conversation runs, and what that lets it do.
 *
 * Three states and each says a different thing, because they are three different situations and
 * running them together is how a person ends up guessing:
 *
 * - **no project** — it can talk and nothing else, and here is how to change that;
 * - **a project with no wired hook** — it has a tree and still cannot touch it, and here is the one
 *   press that fixes it;
 * - **both** — a quiet line naming the folder, so pressing the button leaves visible proof rather
 *   than silence.
 *
 * Read on demand and not polled: it changes when somebody changes it, and both of the somethings
 * are mutations in this window.
 */
/**
 * What this conversation is, in one quiet line, and everything else behind a menu.
 *
 * The line carries only what is consulted constantly — the directory it runs in, which
 * model answers, and whether it is in plan-only mode — because those three are what make
 * an answer make sense, and reaching for a menu to find out which model wrote something
 * is a click too many on every single read.
 *
 * Plan-only appears only while it is on. Silence is the normal state and the normal state
 * says nothing; a line that always reads "plan only: no" is a line nobody reads by the
 * second day.
 *
 * **Archive is inside the menu but is not a menu item, and that is not an oversight.**
 * `ArchiveControl` is a `ConfirmButton`, which is this app's one interlock: two clicks
 * with a dwell between them. A `DropdownMenuItem` closes the menu the moment it is
 * chosen, so the first click would dismiss the control before the second could confirm —
 * turning a deliberate two-step into a one-click irreversible action. Rendered as plain
 * content, the menu stays open and both clicks land.
 */
function ChatMeta({ chatId, brain }: { chatId: string; brain: Brain }) {
  const project = useChatProject(chatId);
  const cwd = project.data?.cwd ?? null;
  const tools = project.data?.tools ?? false;
  const planning = project.data?.planning ?? false;

  return (
    <div className="chats-meta">
      <p className="chats-meta-line">
        {/* The directory is named here only when it is settled. While it is unknown, or
            while it is a state that needs teaching, `Project` below says so in full — a
            summary line is the wrong place to explain something. */}
        {cwd !== null && tools && <span className="chats-meta-where">{cwd}</span>}
        <span className="chats-meta-brain">{brain}</span>
        {planning && <span className="chats-meta-planning">plan only</span>}
      </p>

      <DropdownMenu>
        <DropdownMenuTrigger className="chats-meta-more" aria-label="Conversation settings">
          ⋯
        </DropdownMenuTrigger>
        <DropdownMenuContent align="end" className="chats-meta-menu">
          <DropdownMenuLabel>Which model answers</DropdownMenuLabel>
          <BrainPicker chatId={chatId} brain={brain} />
          <DropdownMenuSeparator />
          <Planning chatId={chatId} />
          <DropdownMenuSeparator />
          <ArchiveControl chatId={chatId} />
        </DropdownMenuContent>
      </DropdownMenu>
    </div>
  );
}

function Project({ chatId }: { chatId: string }) {
  const project = useChatProject(chatId);

  // Nothing at all until it is known. A conversation is not "without a project" because the answer
  // has not arrived yet, and a note that appears and then retracts itself is worse than a late one.
  if (project.data === undefined) return null;
  const { cwd, tools, session } = project.data;

  return (
    <>
      {/* Only the states that need saying. The settled one — a directory, with tools
          wired — is named by `ChatMeta`'s quiet line instead of by a sentence of its
          own, because "exceptions dominate, the normal disappears" and a conversation
          that is set up correctly is the normal case. */}
      {cwd === null && <NoProject chatId={chatId} />}
      {cwd !== null && !tools && <ProjectWithoutTools chatId={chatId} cwd={cwd} />}
      {cwd !== null && session !== null && <CarryOn cwd={cwd} session={session} />}
    </>
  );
}

/**
 * Whether this conversation plans without acting.
 *
 * `--permission-mode plan` is what the daemon launches with, and it has always been able to — every
 * kind of run in this house could be put in planning except the kind a person is watching, which is
 * the one where it matters most. It is the mode you reach for before letting an agent near a
 * codebase.
 *
 * A state on the conversation rather than a choice per message: somebody says "plan this", reads
 * it, then says "go". Making it per-message would turn one decision into a thing to remember every
 * time.
 *
 * **It is not "changes nothing", and this said so until it was measured.** A planning turn still
 * reaches for tools — `Glob`, `Read`, and a `Write` that RAN, which the daemon's gate saw and the
 * transcript recorded. What it wrote was its own plan, as a document in the working directory;
 * what it did not do was the work. The label says that now, because a control promising more than
 * the mode delivers is worse than no control.
 */
function Planning({ chatId }: { chatId: string }) {
  const project = useChatProject(chatId);
  const set = useSetPlanning(chatId);
  const planning = project.data?.planning ?? false;

  return (
    <label
      className="chats-planning"
      title="answer with a plan instead of doing the work — it may still write the plan down"
    >
      <input
        type="checkbox"
        checked={planning}
        disabled={project.data === undefined || set.isPending}
        onChange={(event) => set.mutate(event.target.checked)}
      />
      Plan only
    </label>
  );
}

/**
 * How to carry this conversation on at a terminal.
 *
 * The loop closes both ways and always did: the daemon runs the CLI with a session id of its own, in
 * the conversation's directory, and the CLI keeps its transcripts one folder per project — so
 * `claude --resume <id>` from there continues it. Measured, with a word said only to the daemon
 * coming back out of a fresh CLI.
 *
 * What was missing was anybody being told. The id lived in a table and appeared nowhere a person
 * could read, which made the way back one only somebody who reads the daemon's source could find.
 *
 * Shown only where there is a directory to stand in, and only while the daemon would resume it
 * itself — a rotated conversation offers nothing rather than an id that leads somewhere it will not
 * go.
 */
function CarryOn({ cwd, session }: { cwd: string; session: string }) {
  return (
    <p className="chats-project-carry">
      to carry this on at a terminal:{" "}
      <code className="chats-project-command">
        cd {cwd} &amp;&amp; claude --resume {session}
      </code>
    </p>
  );
}

/**
 * What a conversation started here cannot do, and the way to change it.
 *
 * A conversation's working directory used to be written once, at creation, out of the editor
 * session it was picked up from — so one started here had none, and `tool_policy_for` answered
 * `McpOnly` for as long as it existed. No Bash, no Read, no Write, and nothing said so: you would
 * ask it to fix a file, watch it not fix the file, and have nowhere to find out why.
 *
 * The suggestions are the folders the editor's own sessions were had in, which is where somebody
 * asking this question almost always means. Typed rather than picked from a dialog because a native
 * folder picker is a Tauri plugin this app does not carry, and the daemon refuses a path that is not
 * an absolute directory — so a typo comes back as a sentence instead of as a broken conversation.
 */
function NoProject({ chatId }: { chatId: string }) {
  const point = useSetChatProject(chatId);
  // Not watched: these are wanted as a list of folders, and a list of folders does not need
  // re-reading every three seconds.
  const sessions = useIdeSessions(true);
  const [path, setPath] = useState("");

  const folders = Array.from(new Set((sessions.data ?? []).map((session) => session.cwd)));

  return (
    <div className="chats-project">
      <p className="chats-new-warning" role="status">
        this conversation has no project — it can talk about code and remember what was said, but it{" "}
        <b>cannot open a file, run a command, or change anything</b> on this machine.
      </p>
      <form
        className="chats-project-form"
        onSubmit={(event) => {
          event.preventDefault();
          if (point.isPending) return;
          point.mutate(path.trim());
        }}
      >
        <label htmlFor="chat-project">Project folder</label>
        <input
          id="chat-project"
          list="chat-project-folders"
          className="chats-project-path"
          placeholder="C:/Projects/something"
          value={path}
          onChange={(event) => setPath(event.target.value)}
        />
        <datalist id="chat-project-folders">
          {folders.map((folder) => (
            <option key={folder} value={folder} />
          ))}
        </datalist>
        <Button type="submit" intent="go" disabled={point.isPending || path.trim() === ""}>
          Use this project
        </Button>
      </form>
      {point.isError && <ProjectRefusal error={point.error} />}
    </div>
  );
}

/**
 * A conversation that has a tree and still cannot touch it.
 *
 * The daemon grants tools on a directory whose classifier hook is wired, and a fresh worktree has
 * none — `.claude/` is not committed. Same words as the editor door's own warning, because it is the
 * same situation reached from the other side.
 */
function ProjectWithoutTools({ chatId, cwd }: { chatId: string; cwd: string }) {
  const wire = useWireChatTools(chatId);

  return (
    <div className="chats-project">
      <p className="chats-new-warning" role="status">
        this conversation is about {cwd}, which has no núcleo hook — it can talk about the code but{" "}
        <b>cannot read or change any file</b>, and cannot run anything
      </p>
      <Button type="button" disabled={wire.isPending} onClick={() => wire.mutate()}>
        Give it the tools
      </Button>
      {wire.isError && <WireRefusal error={wire.error} cwd={cwd} />}
    </div>
  );
}

function ProjectRefusal({ error }: { error: unknown }) {
  if (!isApiRefusal(error)) {
    return <ErrorNote>the núcleo did not answer — the conversation was left as it was</ErrorNote>;
  }
  return (
    <RefusalNote
      refusal={error}
      sentences={{
        bad_request: "that has to be an absolute path to a folder that exists on this machine",
        conflict: "this conversation is answering — wait for the turn to end, then move it",
        not_found: "that conversation is no longer here",
      }}
    />
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
function PickedUp({
  view,
  handed,
}: {
  view: ReturnType<typeof useIdeConversation>;
  handed: Exchange[];
}) {
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
  if (view.data.said.length === 0) {
    return (
      <p className="chats-picked-up-cut">
        this was picked up from a conversation in the editor that nobody spoke in.
      </p>
    );
  }
  return (
    <>
      {/* Said above the text, where the missing part would have been, rather than under it as a
          footnote. A person reads down from the top; the top is exactly where the gap is. */}
      {view.data.cut && (
        <p className="chats-picked-up-cut">
          older messages are not shown — this conversation was read from its recent end
        </p>
      )}
      <ul className="chats-said" aria-label="Said in the editor">
        {view.data.said.map((said, index) => (
          // Keyed by position: these came from a file, in the order they are in it, and
          // nothing here reorders or removes one. A transcript has no id to key by.
          <li
            key={`said-${index}`}
            className={
              said.aside
                ? "chats-said-line chats-said-aside"
                : said.by_owner
                  ? "chats-said-line chats-said-owner"
                  : "chats-said-line"
            }
          >
            {/* No speaker on an aside. It is about the conversation, not a line of it, and a
                "núcleo" label over it would attribute words the model never said. */}
            {!said.aside && (
              <span className="chats-said-who">{said.by_owner ? "you" : "núcleo"}</span>
            )}
            {/* Text, never markup — this is somebody else's file. `Rich` never emits either:
                it returns data and this page decides what an element is. */}
            {said.aside || said.by_owner ? (
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
      <HowItContinued
        handed={handed}
        carries={view.data.context_estimate}
        rotatesAt={view.data.context_rotates_at}
      />
    </>
  );
}

/**
 * Whether the model REMEMBERS what is drawn above this, or was only handed the end of it.
 *
 * The window drew somebody's whole editor conversation and then a fresh turn under it, with no
 * seam. That reads as one continuous thing the model has all of — and for a session past the
 * daemon's ceiling it is false: that session is not resumed at all, and what the model was given
 * is the last few exchanges, verbatim, in front of an empty context.
 *
 * `handoff.rs` states the rule this pays: context pressure must leave an auditable record rather
 * than quietly erase how work continued. A compaction stored in a column and never shown is still
 * an erasure from where the person is standing — hence the disclosure, which is the audit.
 *
 * Three cases and not two, because the third is real: an empty `handed` on a session that IS over
 * the ceiling means it was picked up before any of this existed, or the tail could not be taken.
 * Neither "resumed" nor "handed" is true of it, so it is told nothing rather than told wrong.
 */
function HowItContinued({
  handed,
  carries,
  rotatesAt,
}: {
  handed: Exchange[];
  carries: number | null;
  rotatesAt: number;
}) {
  const [open, setOpen] = useState(false);

  if (handed.length === 0) {
    if (carries === null || carries > rotatesAt) return null;
    return (
      <p className="chats-picked-up-cut">
        this session was resumed, so the model has all of the above in its context.
      </p>
    );
  }
  return (
    <div className="chats-handed">
      <p className="chats-handed-line">
        this session was too large to resume, so it was not. The model was handed the last{" "}
        {handed.length === 1 ? "exchange" : `${handed.length} exchanges`} of it, word for word, in
        front of an empty context — everything above them is here for you to read, not something it
        remembers.
      </p>
      <button
        type="button"
        className="chats-handed-toggle"
        aria-expanded={open}
        onClick={() => setOpen(!open)}
      >
        {open ? "hide what it was handed" : "show what it was handed"}
      </button>
      {open && (
        <ul className="chats-handed-list" aria-label="What the model was handed">
          {handed.map(([asked, answered], index) => (
            // Keyed by position: this is a stored list nothing here reorders or removes from.
            <li key={`handed-${index}`} className="chats-handed-pair">
              <span className="chats-said-who">you</span>
              <p className="chats-said-text">{asked}</p>
              <span className="chats-said-who">núcleo</span>
              <p className="chats-said-text">{answered}</p>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}

/**
 * What is different in this conversation's project, without leaving the app.
 *
 * The question a person has after a coding turn is "what changed", and the transcript answers it
 * with the name of a tool and a path. To see what those did you had to go somewhere else, which is
 * the opposite of what a conversation about code is for.
 *
 * **It says what it is, and what it is not.** The daemon takes no snapshot before a turn, so this is
 * what is different NOW — the same thing after one turn, and not after three. Labelling it as what
 * the turn did would be the kind of note that reads like a fact and stops being one.
 *
 * Closed until asked. Opening it walks a working tree, and a panel that did that on arrival would do
 * it for every conversation somebody clicked past.
 */
function Changed({ chatId }: { chatId: string }) {
  const [open, setOpen] = useState(false);
  const diff = useChatDiff(chatId, open);

  return (
    <div className="chats-changed">
      <button
        type="button"
        className="chats-changed-toggle"
        aria-expanded={open}
        onClick={() => setOpen((was) => !was)}
      >
        {open ? "hide what is different" : "what is different in this project"}
      </button>
      {open && diff.isError && (
        <ChangedRefusal error={diff.error} />
      )}
      {open && diff.data === undefined && !diff.isError && (
        <p className="chats-loading">reading the project…</p>
      )}
      {open && diff.data !== undefined && <DiffView diff={diff.data} />}
    </div>
  );
}

/** One `git diff`, coloured. A clean tree is said in words rather than drawn as an empty box. */
function DiffView({ diff }: { diff: string }) {
  const lines = diffLines(diff);
  if (lines.length === 0) {
    return <p className="chats-changed-clean">nothing in this project has changed</p>;
  }
  return (
    <pre className="chats-diff" aria-label="What is different">
      {lines.map((line, at) => (
        // Keyed by position: a diff is read whole and redrawn whole, and nothing reorders inside it.
        <span key={`diff-${at}`} className={`chats-diff-${line.kind}`}>
          {line.text}
          {"\n"}
        </span>
      ))}
    </pre>
  );
}

function ChangedRefusal({ error }: { error: unknown }) {
  if (!isApiRefusal(error)) {
    return <ErrorNote>the núcleo did not answer — nothing could be read</ErrorNote>;
  }
  return (
    <RefusalNote
      refusal={error}
      sentences={{
        conflict: "this conversation has no project, so there is no working tree to compare",
        not_found: "that conversation is no longer here",
      }}
    />
  );
}

/**
 * What this conversation is waiting to be allowed to do.
 *
 * The wall this removes: the classifier sends everything not provably read-only for approval, a
 * conversation cannot park a proposal — one expects a worktree run to resume into and a chat has
 * none — so the answer used to be a refusal telling the person to go and do it somewhere else.
 * There was nowhere else. It is their window and they are looking at it, and the honest reply to
 * somebody who is watching is a question.
 *
 * Urgent on purpose. A turn is held while this stands and the daemon refuses on its own after about
 * forty-five seconds, because the CLI will not hold a hook call longer than that — so this is drawn
 * where the next thing would have appeared rather than tucked away somewhere tidy.
 */
function Asking({ asks, chatId }: { asks: Ask[]; chatId: string }) {
  const answer = useAnswerAsk(chatId);
  if (asks.length === 0) return null;
  return (
    <ul className="chats-asking" aria-label="Waiting to be allowed">
      {asks.map((ask) => (
        <li key={ask.id} className="chats-asking-line">
          <p className="chats-asking-what" role="status">
            this conversation wants to run <b>{ask.tool}</b>
            {ask.detail !== null && (
              <>
                {" "}
                — <code className="chats-asking-detail">{ask.detail}</code>
              </>
            )}
          </p>
          <div className="chats-asking-answer">
            <Button
              type="button"
              intent="go"
              disabled={answer.isPending}
              onClick={() => answer.mutate({ id: ask.id, allow: true })}
            >
              Allow it
            </Button>
            <Button
              type="button"
              disabled={answer.isPending}
              onClick={() => answer.mutate({ id: ask.id, allow: false })}
            >
              Refuse
            </Button>
          </div>
        </li>
      ))}
    </ul>
  );
}

/**
 * What was said to this conversation while it was busy, and has not been sent yet.
 *
 * Outside the transcript, deliberately. A turn is a run: it has an id, it has a cost, and it is in
 * the history for ever. These have none of that — nothing has been spawned, nothing is billed, and
 * a bubble that looked like a turn would be claiming one that does not exist. They leave this list
 * by becoming turns, on their own, the moment the conversation has a slot free.
 *
 * One can be taken back, by its own id and never by its place in the line. The front of this list
 * is sent while somebody is looking at it, so a position names a different message by the time the
 * button is pressed.
 */
function Waiting({ queued, chatId }: { queued: Waiting[]; chatId: string }) {
  const drop = useDropQueued(chatId);
  if (queued.length === 0) return null;
  return (
    <ul className="chats-waiting" aria-label="Waiting to be sent">
      {queued.map((message) => (
        // Keyed by the daemon's own id, not by position: the front of this list is sent while it
        // is on screen, and a key that moved with it would redraw the wrong row.
        <li key={message.id} className="chats-waiting-line">
          <span className="chats-waiting-who">you · waiting</span>
          {/* Verbatim, and not through `Rich`: it is what a person typed, and a message redrawn
              as bold is a message they did not write. */}
          <p className="chats-waiting-text">{message.text}</p>
          <button
            type="button"
            className="chats-waiting-drop"
            aria-label={`Do not send: ${message.text}`}
            disabled={drop.isPending}
            onClick={() => drop.mutate(message.id)}
          >
            don't send this
          </button>
        </li>
      ))}
    </ul>
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
      <TurnPictures paths={turn.images} />
      {!live && <Thought thought={turn.thought} tokens={turn.thoughtTokens} />}
      {!live && <Plan todos={planOf(turn.did)} />}
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
        {/* Money only. The daemon's turn rows carry no token breakdown — see `CostLineProps`. */}
        <CostLine costUsd={turn.cost_usd} />
        <ContextFill fill={turn.contextFill} rotatesAt={turn.rotatesAt} />
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
        {/* One flex item, not one per span. The row exists to hang the marker beside the text;
            left unwrapped, every word and every `code` chip became its own flex item — gapped
            apart and shrinkable on its own, so `budget_usd` was squeezed until it broke mid-name
            and stacked vertically. Seen in the app, in a bulleted answer. */}
        <span className="chats-rich-bullet-text">{inner}</span>
      </p>
    );
  }
  return <p className="chats-rich-line">{inner}</p>;
}

/**
 * How full the context was, and a word before the daemon starts a new one.
 *
 * The rotation used to arrive without a sound. A conversation ran, crossed the ceiling, and the
 * next turn began remembering nothing — and the first anybody heard of it was the restart mark
 * drawn after the fact, or a model suddenly asking what they were talking about.
 *
 * The ceiling is the daemon's, never this file's. It arrives on every turn precisely so this side
 * never keeps a copy of it, and a turn that arrives without one draws the count alone rather than
 * a proportion of a number nobody sent.
 */
function ContextFill({ fill, rotatesAt }: { fill: number | null; rotatesAt: number | null }) {
  if (fill === null) return null;
  const k = (n: number) => `${(n / 1000).toFixed(1)}k`;
  if (rotatesAt === null) return <span className="chats-turn-fill">{k(fill)} of context</span>;
  // Near, not past. Past is too late to be a warning: the turn that crosses the line is the last
  // one that remembers, and this is drawn under it while the next one is still being typed.
  const near = fill >= rotatesAt * 0.85;
  return (
    <span className={near ? "chats-turn-fill chats-turn-fill-near" : "chats-turn-fill"}>
      {`${k(fill)} of ${k(rotatesAt)}`}
      {near && " — the next turn may begin a fresh context"}
    </span>
  );
}

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
      <Thought thought={live.data?.thought ?? []} tokens={live.data?.thought_tokens ?? null} />
      {text !== "" && <p className="chats-turn-answer chats-turn-writing">{text}</p>}
      <Plan todos={planOf(live.data?.did ?? [])} />
      <WhatItDid did={live.data?.did ?? []} />
      <p className="chats-turn-live" ref={end}>
        {doing !== null ? `running ${doing}…` : text === "" ? "thinking…" : "writing…"}
      </p>
    </>
  );
}

/**
 * The pictures a turn was sent with, drawn under what was typed.
 *
 * Fetched one at a time, by path, from the files route — the bytes are on disk under the daemon's
 * own root and never on the transcript, so a conversation of forty turns costs forty short strings
 * to read and only the pictures actually on screen to draw.
 */
function TurnPictures({ paths }: { paths: string[] }) {
  if (paths.length === 0) return null;
  return (
    <ul className="chats-pictures" aria-label="Pictures sent with this message">
      {paths.map((path) => (
        <li key={path}>
          <TurnPicture path={path} />
        </li>
      ))}
    </ul>
  );
}

/**
 * One picture, fetched as bytes and held as an object URL for as long as it is on screen.
 *
 * Not an `<img src>` pointed at the route: every request to the daemon carries a token, and a
 * browser fetching an image never sends one. So the bytes come through the same door as everything
 * else and become a URL this document owns — revoked on the way out, because an object URL nobody
 * releases is a leak that lasts as long as the window does.
 */
function TurnPicture({ path }: { path: string }) {
  const [url, setUrl] = useState<string | null>(null);
  const [gone, setGone] = useState(false);
  const [open, setOpen] = useState(false);

  useEffect(() => {
    let live = true;
    let made: string | null = null;
    void fetchFileBlob(path)
      .then((blob) => {
        if (!live) return;
        made = URL.createObjectURL(blob);
        setUrl(made);
      })
      .catch(() => {
        if (live) setGone(true);
      });
    return () => {
      live = false;
      if (made !== null) URL.revokeObjectURL(made);
    };
  }, [path]);

  // Said, not left blank: a picture that was sent and can no longer be read is a fact about the
  // record, and an empty space where one was is indistinguishable from a turn that had none.
  if (gone) return <p className="chats-picture-gone">a picture sent here can no longer be read</p>;
  if (url === null) return <p className="chats-picture-gone">reading a picture…</p>;
  return (
    <>
      {/* A button and not a bare image: opening one is an action, and an image that grows when
          clicked without ever saying it could is a thing people find by accident. */}
      <button
        type="button"
        className="chats-picture-open"
        aria-label={`Open picture ${path}`}
        onClick={() => setOpen(true)}
      >
        <img className="chats-picture" src={url} alt={`sent with this message: ${path}`} />
      </button>
      {open && <PictureOverlay url={url} path={path} onClose={() => setOpen(false)} />}
    </>
  );
}

/**
 * One picture, filling the window, until it is dismissed.
 *
 * Escape closes it as well as the button, because a thing that covers the page and can only be left
 * by finding a small target is a thing that traps people. The same object URL the thumbnail is
 * already holding — fetching the bytes a second time to show the same picture larger would be
 * paying twice for one file.
 */
function PictureOverlay({
  url,
  path,
  onClose,
}: {
  url: string;
  path: string;
  onClose: () => void;
}) {
  useEffect(() => {
    const escape = (event: KeyboardEvent) => {
      if (event.key === "Escape") onClose();
    };
    window.addEventListener("keydown", escape);
    return () => window.removeEventListener("keydown", escape);
  }, [onClose]);

  return (
    <div
      className="chats-picture-overlay"
      role="dialog"
      aria-modal="true"
      aria-label={`Picture ${path}`}
      onClick={onClose}
    >
      <img className="chats-picture-full" src={url} alt={path} />
      <button type="button" className="chats-picture-close" aria-label="Close picture">
        close
      </button>
    </div>
  );
}

/**
 * That the model thought, and how much — because what it thought cannot be had.
 *
 * This began as "show the reasoning, folded shut", which is what the editor does. Asked of the CLI
 * directly, it cannot be done by anybody: every `thinking` block arrives as
 * `{"type":"thinking","thinking":"","signature":"…"}`, in the stream and in Claude Code's own
 * transcript files alike — 610 of them across one real session, not one with a word in it. What
 * does arrive is a running `thinking_tokens` estimate, and that is what this says.
 *
 * So the honest shape is a statement, not a disclosure: there is nothing to open. A toggle here
 * would promise reasoning this machine will never hold, which is worse than saying less. If the
 * words ever start arriving, `thought` carries them and they unfold under the same line.
 */
function Thought({ thought, tokens }: { thought: string[]; tokens: number | null }) {
  const [open, setOpen] = useState(false);
  if (thought.length === 0 && tokens === null) return null;
  const size = tokens === null ? null : tokens >= 1000 ? `${(tokens / 1000).toFixed(1)}k` : `${tokens}`;
  return (
    <div className="chats-thought">
      {thought.length === 0 ? (
        <p className="chats-thought-line">thought for ~{size} tokens</p>
      ) : (
        <button
          type="button"
          className="chats-thought-toggle"
          aria-expanded={open}
          onClick={() => setOpen(!open)}
        >
          {open ? "hide thinking" : size === null ? "thinking" : `thinking · ~${size} tokens`}
        </button>
      )}
      {open &&
        thought.map((text, index) => (
          <p key={`thought-${index}`} className="chats-thought-text">
            {text}
          </p>
        ))}
    </div>
  );
}

/**
 * The plan a turn worked through, which the page had as the word `TodoWrite`.
 *
 * A model that writes a list and then works down it is the shape of most real work, and none of it
 * reached here: the call carries no path and no command, so it arrived as a bare name beside the
 * others. Watching the ticks move is a good half of what a person is looking at when they look at
 * the editor, and it was the one thing this page could not show.
 *
 * Absent rather than empty when there is none, for the same reason `WhatItDid` is: a heading over
 * no rows reads as a plan that failed to load, which is a different and worse claim than a turn
 * that planned nothing.
 */
function Plan({ todos }: { todos: Todo[] }) {
  if (todos.length === 0) return null;
  return (
    <ul className="chats-plan" aria-label="The plan">
      {todos.map((todo, index) => (
        // Keyed by position: a plan is a list in an order somebody chose, and the same line can
        // legitimately appear twice.
        <li key={`todo-${index}`} className={`chats-plan-item chats-plan-${todo.status}`}>
          <span className="chats-plan-mark" aria-hidden="true">
            {todo.status === "completed" ? "✓" : todo.status === "in_progress" ? "→" : "·"}
          </span>
          <span className="chats-plan-text">{todo.text}</span>
        </li>
      ))}
    </ul>
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

/**
 * The brain and restart marks a transcript draws above one turn.
 *
 * The brain mark's copy is deliberately asymmetric: moving *to* the cloud is about where what you
 * type now goes, and moving *to* the local model is about where the answer comes from — the two
 * directions are not mirror images of the same fact.
 */
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

/**
 * How many pictures one message may carry.
 *
 * The daemon's own ceiling, said again here so the window stops before the refusal rather than
 * after it. A number duplicated across two codebases is one that drifts, and this one is worth the
 * risk: the alternative is letting somebody attach nine screenshots and telling them at Send.
 */
const MAX_PICTURES = 5;

/**
 * One thing the list can offer, whichever gesture opened it.
 *
 * A `@` and a `/` are the same move — type a sigil, narrow a list, choose — and the arrows, the
 * Enter and the highlight are identical for both. Only what is being listed differs, so that is the
 * only thing this carries: a strong half, a quiet half, and what picking it does.
 */
interface Choice {
  key: string;
  primary: string;
  secondary: string;
  chosen: () => void;
}

function Composer({ chatId }: { chatId: string }) {
  const [text, setText] = useState("");
  const [caret, setCaret] = useState(0);
  // Escape closes the list without closing what is being typed: the sigil and what follows it stay
  // in the box. Held as the query it was dismissed AT, so the next letter — a different question —
  // opens it again rather than leaving somebody stuck with a feature they turned off.
  const [dismissed, setDismissed] = useState<string | null>(null);
  const [highlight, setHighlight] = useState(0);
  const [attached, setAttached] = useState<Attachment[]>([]);
  const box = useRef<HTMLTextAreaElement | null>(null);
  const send = useSendMessage(chatId);

  // A picture is read here, in the window, and travels as base64 inside the message. Not as a path
  // for the model to go and read: it is part of what was said, and the CLI takes it that way —
  // measured, with a magenta square it correctly named.
  const attach = async (files: FileList | File[] | null) => {
    const pictures = Array.from(files ?? []).filter(isPicture);
    if (pictures.length === 0) return;
    const read = await Promise.all(pictures.map(attachmentFrom));
    setAttached((was) => [...was, ...read].slice(0, MAX_PICTURES));
  };

  // Never both: a command is only ever the first character of the box, and a mention needs
  // whitespace before it, so a live `/` means everything up to the caret has no space in it and
  // there is no mention to find. Computed separately all the same, because relying on that
  // reasoning silently would be relying on two functions in another module agreeing forever.
  const mention = mentionAt(text, caret);
  const command = commandAt(text, caret);
  const live = (at: { query: string } | null) =>
    at !== null && at.query !== dismissed ? at.query : null;

  const files = useChatFiles(chatId, command === null ? live(mention) : null);
  const commands = useChatCommands(chatId, live(command));

  // Writing the choice back means moving the caret, and only the element knows how. Set on the next
  // frame because React has not re-rendered the new value yet at the moment this is called.
  const write = (written: { text: string; caret: number }) => {
    setText(written.text);
    setDismissed(null);
    setHighlight(0);
    requestAnimationFrame(() => {
      box.current?.focus();
      box.current?.setSelectionRange(written.caret, written.caret);
      setCaret(written.caret);
    });
  };

  const choices: Choice[] =
    command !== null && live(command) !== null
      ? (commands.data?.commands ?? []).map((hit: Command) => ({
          key: hit.name,
          primary: `/${hit.name}${hit.hint === null ? "" : ` ${hit.hint}`}`,
          // The source is worth saying: two commands can share a name, and which one runs depends
          // on where it came from. Falls back to it when a command's file gives no description.
          secondary: hit.description ?? hit.source,
          chosen: () => write(withCommand(text, command, hit.name)),
        }))
      : mention !== null && live(mention) !== null
        ? (files.data?.hits ?? []).map((hit: Mention) => ({
            key: hit.path,
            primary: `${hit.name}${hit.is_dir ? "/" : ""}`,
            secondary: hit.path,
            chosen: () => write(withMention(text, mention, hit.path, hit.is_dir)),
          }))
        : [];

  // A file gesture over a conversation with no directory is the one case with something to say and
  // nothing to list. A command gesture never has it: personal and plugin commands exist wherever
  // the conversation runs.
  const nowhere =
    command === null && mention !== null && live(mention) !== null && files.data?.rooted === false;
  const open = choices.length > 0 || nowhere;

  // One place, two ways in: the button and the key. Duplicating the guards into the key handler is
  // how one of them ends up sending an empty turn six months from now.
  // A picture on its own is a message: "what is this?" is a reasonable thing to send with nothing
  // typed, and refusing it because the box is empty would be the window deciding what counts.
  const sayable = (text.trim() !== "" || attached.length > 0) && !send.isPending;
  const say = () => {
    if (!sayable) return;
    send.mutate(
      { text: text.trim(), images: attached },
      {
        onSuccess: () => {
          setText("");
          setAttached([]);
        },
      },
    );
  };

  return (
    <form
      className="chats-composer"
      onSubmit={(event) => {
        event.preventDefault();
        say();
      }}
    >
      {nowhere && (
        <p className="chats-mentions-none">
          this conversation has no directory, so there are no files to name here
        </p>
      )}
      {choices.length > 0 && (
        <Choices
          label={command === null ? "Files to mention" : "Commands to run"}
          choices={choices}
          highlight={highlight}
          truncated={command === null && files.data?.truncated === true}
        />
      )}
      {attached.length > 0 && (
        <ul className="chats-attached" aria-label="Attached pictures">
          {attached.map((picture, index) => (
            <li key={`attached-${index}`} className="chats-attached-item">
              <img
                className="chats-attached-thumb"
                alt={`attached picture ${index + 1}`}
                src={`data:${picture.media_type};base64,${picture.data}`}
              />
              <button
                type="button"
                className="chats-attached-drop"
                aria-label={`Remove attached picture ${index + 1}`}
                onClick={() => setAttached((was) => was.filter((_, at) => at !== index))}
              >
                ×
              </button>
            </li>
          ))}
        </ul>
      )}
      <label className="chats-field">
        <span>Message</span>
        <textarea
          ref={box}
          // Pasting is the gesture: a screenshot goes to the clipboard and then into the box, and
          // anything that made you save it to a file first would be a step nobody takes.
          onPaste={(event) => {
            const pictures = Array.from(event.clipboardData.files).filter(isPicture);
            if (pictures.length === 0) return;
            // Only when there IS a picture: a plain text paste must stay a text paste.
            event.preventDefault();
            void attach(pictures);
          }}
          rows={3}
          aria-label="Message"
          value={text}
          onChange={(event) => {
            setText(event.target.value);
            setCaret(event.target.selectionStart);
            setDismissed(null);
            setHighlight(0);
          }}
          // The caret moves without the text changing — arrows, a click, Home. What is being typed
          // is read from where the caret IS, so every one of those has to be heard or the list goes
          // stale against a position it no longer describes.
          onSelect={(event) => setCaret(event.currentTarget.selectionStart)}
          // Enter sends and Shift+Enter breaks the line, because that is what every chat anybody
          // has ever used does — and a textarea does the opposite by default, so the habit costs a
          // reach for the mouse on every single message.
          //
          // While a list is open those same keys belong to it. Not a special case bolted on: a list
          // under the caret owns the arrows and the Enter for as long as it is showing, which is
          // what every editor does and what the hand already expects.
          onKeyDown={(event) => {
            if (choices.length > 0) {
              if (event.key === "ArrowDown") {
                event.preventDefault();
                setHighlight((was) => (was + 1) % choices.length);
                return;
              }
              if (event.key === "ArrowUp") {
                event.preventDefault();
                setHighlight((was) => (was - 1 + choices.length) % choices.length);
                return;
              }
              if (event.key === "Enter" || event.key === "Tab") {
                event.preventDefault();
                choices[Math.min(highlight, choices.length - 1)].chosen();
                return;
              }
            }
            if (open && event.key === "Escape") {
              event.preventDefault();
              setDismissed(live(command) ?? live(mention));
              return;
            }
            if (event.key !== "Enter" || event.shiftKey) return;
            event.preventDefault();
            say();
          }}
        />
      </label>
      <div className="chats-composer-actions">
        {/* The way in for anything not on the clipboard. Hidden behind its own label because a bare
            file input is the one control on this page nobody can style into the others. */}
        <label className="chats-attach">
          <span>Attach a picture</span>
          <input
            type="file"
            accept="image/*"
            multiple
            aria-label="Attach a picture"
            onChange={(event) => {
              void attach(event.target.files);
              // Cleared so the same file chosen twice in a row is heard the second time.
              event.target.value = "";
            }}
          />
        </label>
        <Button type="submit" intent="go" disabled={!sayable}>
          Send
        </Button>
      </div>
      {send.isError && <MessageRefusal error={send.error} />}
    </form>
  );
}

/**
 * What the caret is offering, above the box rather than below it.
 *
 * Above because the box sits at the bottom of the window: a list drawn under it would open off the
 * edge of the panel, which is the one place it cannot be read.
 *
 * One component for files and for commands. The two are the same gesture with different contents,
 * and a second copy of this would be a second place for the highlight, the keys and the truncation
 * note to drift apart.
 */
function Choices({
  label,
  choices,
  highlight,
  truncated,
}: {
  label: string;
  choices: Choice[];
  highlight: number;
  truncated: boolean;
}) {
  return (
    <ul className="chats-mentions" aria-label={label}>
      {choices.map((choice, index) => (
        <li key={choice.key}>
          <button
            type="button"
            className={index === highlight ? "chats-mention chats-mention-on" : "chats-mention"}
            aria-current={index === highlight}
            // The mouse must not take focus off the box: the caret is the whole state this list
            // reads from, and a blur would move it before the click ever lands.
            onMouseDown={(event) => event.preventDefault()}
            onClick={choice.chosen}
          >
            <span className="chats-mention-name">{choice.primary}</span>
            <span className="chats-mention-path">{choice.secondary}</span>
          </button>
        </li>
      ))}
      {truncated && (
        <li className="chats-mentions-cut">more than these — keep typing to narrow it</li>
      )}
    </ul>
  );
}

function MessageRefusal({ error }: { error: unknown }) {
  if (!isApiRefusal(error)) return <ErrorNote>the núcleo did not answer — nothing was sent</ErrorNote>;
  return <RefusalNote refusal={error} sentences={MESSAGE_SENTENCES} />;
}
