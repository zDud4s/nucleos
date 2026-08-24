import { useEffect, useRef, useState } from "react";
import { Link, useNavigate, useParams } from "@tanstack/react-router";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuLabel,
  DropdownMenuSeparator,
  DropdownMenuSub,
  DropdownMenuSubContent,
  DropdownMenuSubTrigger,
  DropdownMenuRadioGroup,
  DropdownMenuCheckboxItem,
  DropdownMenuRadioItem,
  DropdownMenuTrigger,
} from "../ui/vendor/dropdown-menu";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "../ui/vendor/dialog";
import {
  CommandDialog,
  CommandEmpty,
  CommandGroup,
  CommandInput,
  CommandItem,
  CommandList,
} from "../ui/vendor/command";
import {
  ArrowUp,
  ChevronDown,
  ImagePlus,
  Plus,
  SquareCode,
} from "lucide-react";
import { isApiRefusal } from "../data/client";
import {
  useArchiveChat,
  useAssistantModels,
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
  useCommands,
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
  useStartConversation,
  useStopTurn,
  type Attachment,
  type ChatSummary,
  type Subagent,
  useClearContext,
  useDeniableTools,
  useFreshContext,
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
  Button,
  ConfirmButton,
  CostLine,
  ErrorNote,
  PageHeader,
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
  const summary =
    chatId === null ? undefined : rows.find((row) => row.chat_id === chatId);

  /**
   * The list is a panel you open, not a column you live with.
   *
   * Open by default, because somebody arriving at this page for the first time has to
   * be able to find a conversation without knowing a shortcut. Closed, the transcript
   * gets the whole width, which is what a page made of prose wants.
   */
  const [railOpen, setRailOpen] = useState(true);
  const [paletteOpen, setPaletteOpen] = useState(false);
  /**
   * The editor conversation being considered, if any.
   *
   * State and not a route, because there is nothing to route TO: an editor session is a file on
   * this machine, not a conversation this app has opened, and it has no id here until somebody
   * picks it up. Cleared the moment one is — by then it is a chat with a URL of its own.
   */
  const [pickingUp, setPickingUp] = useState<string | null>(null);
  const navigate = useNavigate();
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
      if (event.key.toLowerCase() !== "k" || !(event.ctrlKey || event.metaKey))
        return;
      event.preventDefault();
      setPaletteOpen((open) => !open);
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  return (
    /* The class that turns this route from a document into an application: see `.chats-app`, which
       stops the shell scrolling the whole page and hands the height to the two columns below. */
    <div className="chats-app">
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
              {!railOpen && unseen > 0 && (
                <span className="chats-unseen">{unseen}</span>
              )}
            </Button>
            <Button variant="ghost" onClick={() => setPaletteOpen(true)}>
              Find a conversation
              <kbd className="chats-kbd">Ctrl K</kbd>
            </Button>
          </>
        }
      />

      {stale && <StaleNote dataUpdatedAt={chats.dataUpdatedAt} />}
      {chats.isError && chats.data === undefined && (
        <ListError error={chats.error} />
      )}

      <ConversationPalette
        rows={rows}
        open={paletteOpen}
        onOpenChange={setPaletteOpen}
      />

      <div
        className={
          railOpen ? "chats-layout" : "chats-layout chats-layout-alone"
        }
      >
        {railOpen && (
          /* The ground that tells the list from the thread. See `.chats-rail`: this used to be a
             bordered panel beside another bordered panel, which is a settings screen, not a chat. */
          <div className="chats-rail">
            <ChatListPanel
              rows={rows}
              answered={chats.data !== undefined}
              selected={chatId}
              selectedLive={selectedLive}
              pickingUp={pickingUp}
              onPickUp={setPickingUp}
              onNew={() => {
                setPickingUp(null);
                void navigate({ to: "/chats" });
              }}
            />
          </div>
        )}

        <div className="chats-detail">
          {/* An editor conversation being considered wins the column: it is a decision in progress,
              and putting it anywhere else would mean choosing it and then hunting for what happened. */}
          {pickingUp !== null && (
            <PickUpPreview
              key={pickingUp}
              sessionId={pickingUp}
              onOpened={(opened) => {
                setPickingUp(null);
                void navigate({ to: `/chats/${opened}` });
              }}
            />
          )}
          {pickingUp === null && chatId === null && <NothingOpen />}
          {pickingUp === null && chatId !== null && (
            <ChatDetail
              key={chatId}
              chatId={chatId}
              summary={summary}
              transcript={transcript}
            />
          )}
        </div>
      </div>
    </div>
  );
}

/** One derived sentence about the whole list. */
function headlineFor(
  rows: ChatSummary[],
  answered: boolean,
): string | undefined {
  if (!answered) return undefined;
  if (rows.length === 0)
    return "no conversation has been opened from this window";
  const noun = rows.length === 1 ? "conversation" : "conversations";
  const unread = unreadTotal(rows);
  return unread === 0
    ? `${rows.length} ${noun}, nothing unread`
    : `${rows.length} ${noun}, ${unread} unread`;
}

function ListError({ error }: { error: unknown }) {
  if (isApiRefusal(error)) return <RefusalNote refusal={error} />;
  return (
    <ErrorNote>
      the núcleo did not answer — nothing is known about your conversations
    </ErrorNote>
  );
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
                {row.cwd !== null && (
                  <span className="chats-palette-where">{row.cwd}</span>
                )}
                {row.waiting > 0 && (
                  <span className="chats-palette-waiting">{row.waiting}</span>
                )}
              </CommandItem>
            );
          })}
        </CommandGroup>
      </CommandList>
    </CommandDialog>
  );
}

/**
 * One row of the sidebar, from either source.
 *
 * The list used to hold only conversations this app had opened; the ones still living in the editor
 * were behind a "From the editor" button, in a list of their own. Two lists of the same thing is
 * two places to look for a conversation you half remember — so they became one, and what tells them
 * apart is a mark on the row rather than which door you went through.
 */
export type ListRow =
  | { kind: "chat"; at: string | null; chat: ChatSummary }
  | { kind: "editor"; at: string; session: IdeSession };

/**
 * Both sources, newest first, with the editor sessions this app has ALREADY picked up left out —
 * those are conversations here now, and drawing them twice would offer to open a second copy of
 * something already open.
 */
export function mergeRows(
  chats: ChatSummary[],
  sessions: IdeSession[],
): ListRow[] {
  const pickedUp = new Set(
    chats
      .map((chat) => chat.ide_session_id)
      .filter((id): id is string => id !== null),
  );
  const rows: ListRow[] = [
    ...chats.map((chat) => ({
      kind: "chat" as const,
      at: chat.last_activity,
      chat,
    })),
    ...sessions
      .filter((session) => !pickedUp.has(session.session_id))
      .map((session) => ({
        kind: "editor" as const,
        at: session.last_activity,
        session,
      })),
  ];
  // ISO-8601 sorts correctly as text, which is why the daemon sends it. A conversation nobody has
  // spoken in has no activity at all and goes last rather than pretending to be old.
  return [...rows].sort((a, b) => (b.at ?? "").localeCompare(a.at ?? ""));
}

function ChatListPanel({
  rows,
  answered,
  selected,
  selectedLive,
  pickingUp,
  onPickUp,
  onNew,
}: {
  rows: ChatSummary[];
  answered: boolean;
  selected: string | null;
  selectedLive: boolean;
  pickingUp: string | null;
  onPickUp: (sessionId: string) => void;
  onNew: () => void;
}) {
  // Watched, because one of these may be being typed into in the editor while it is on screen here.
  const sessions = useIdeSessions(true, true);
  const listed = mergeRows(rows, sessions.data ?? []);

  return (
    <>
      <div className="chats-rail-actions">
        {/* Opens an empty chat, and asks nothing. It used to unfold a form here — two radio buttons
            for the model and a Start — which put a question before the only thing anybody came to
            do. The model is a control in the box now, answerable while you type the first sentence
            and changeable after it. */}
        <button type="button" className="chats-rail-new" onClick={onNew}>
          <Plus className="chats-rail-icon" aria-hidden="true" />
          New conversation
        </button>
      </div>

      <div className="chats-rail-scroll">
        {!answered && (
          <p className="chats-loading">reading your conversations…</p>
        )}
        {answered && listed.length === 0 && (
          <Teach title="No conversations yet">
            <p>
              Telegram&apos;s own conversations do not show up here — nothing
              has opened a row for them, because the only door into this list is
              the button above. Start one to see it appear.
            </p>
          </Teach>
        )}
        {sessions.isError && (
          <ErrorNote>your editor sessions could not be read</ErrorNote>
        )}
        {listed.length > 0 && (
          /* One list, both kinds. See `mergeRows`: what tells them apart is the mark on the row. */
          <ul className="chats-list" aria-label="Conversations">
            {listed.map((entry) =>
              entry.kind === "chat" ? (
                <ChatRow
                  key={entry.chat.chat_id}
                  row={entry.chat}
                  active={entry.chat.chat_id === selected}
                  live={entry.chat.chat_id === selected && selectedLive}
                />
              ) : (
                <EditorRow
                  key={entry.session.session_id}
                  session={entry.session}
                  active={entry.session.session_id === pickingUp}
                  onOpen={() => onPickUp(entry.session.session_id)}
                />
              ),
            )}
          </ul>
        )}
      </div>
    </>
  );
}

/**
 * A conversation still living in the editor, in the same list as the rest.
 *
 * Told apart by a mark and by what pressing it does, not by being somewhere else. It is not a link:
 * there is nothing at `/chats/...` to go to yet, and picking it up costs money and carries warnings
 * — so it opens the preview beside the list rather than doing anything.
 */
function EditorRow({
  session,
  active,
  onOpen,
}: {
  session: IdeSession;
  active: boolean;
  onOpen: () => void;
}) {
  const live = stillGoing(session.last_activity, Date.now());
  return (
    <li className={active ? "chats-row chats-row-active" : "chats-row"}>
      <button
        type="button"
        className="chats-row-link chats-row-editor"
        aria-pressed={active}
        /* Spelled out: without it the mark, the name and "happening now" concatenate into one
           run-on word, which is the same trap the nav items and the chat rows already document. */
        aria-label={`In the editor: ${session.title ?? session.session_id}${
          live ? ", happening now" : ""
        }`}
        onClick={onOpen}
      >
        <SquareCode className="chats-row-mark" aria-hidden="true" />
        <span className="chats-row-title">
          {session.title ?? session.session_id}
        </span>
        {live && (
          <span className="chats-row-live" aria-hidden="true">
            now
          </span>
        )}
      </button>
    </li>
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
  const parts = [
    row.title ?? row.first_message ?? "nothing said yet",
    row.brain,
  ];
  if (live) parts.push("thinking");
  if (row.waiting > 0) parts.push(`${row.waiting} unread`);
  return parts.join(", ");
}

function ChatRow({
  row,
  active,
  live,
}: {
  row: ChatSummary;
  active: boolean;
  live: boolean;
}) {
  // A conversation with no name AND nothing said in it has no name to show. Drawing "New
  // conversation" made a dozen of them into a dozen identical rows; saying what is true of them
  // instead makes them one visibly different kind of row you can skim past.
  const said = row.title !== null || row.first_message !== null;
  return (
    <li className={active ? "chats-row chats-row-active" : "chats-row"}>
      <Link
        className="chats-row-link"
        to={`/chats/${row.chat_id}`}
        aria-label={chatRowLabel(row, live)}
        aria-current={active ? "page" : undefined}
      >
        {/* The name, and only the name.
            The row used to carry the title, a coloured badge for the model, the working directory
            and a count — four things per row, eleven rows, and the ones that never vary said
            "cloud" and "c:\Projects\nucleos" over and over. A sidebar is for recognising a
            conversation, and what you recognise it by is what it is called. The directory is on the
            open conversation's own line and searchable in ⌘K; the model is on that line too. What
            stays here is what CHANGES: it is thinking, or it is holding something for you. */}
        <span
          className={
            said ? "chats-row-title" : "chats-row-title chats-row-unsaid"
          }
        >
          {row.title ?? row.first_message ?? "nothing said yet"}
        </span>
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

/* `NewChatForm` — two radio buttons for the model and a Start — stood here. The sidebar's button
   opens an empty chat now and the model is chosen in the box, so there is no form to fill in
   before saying the first thing. */
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
function WhatItCarries({
  view,
}: {
  view: ReturnType<typeof useIdeConversation>;
}) {
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
    return (
      <p className="chats-loading">reading what was said in the editor…</p>
    );
  }
  if (view.data === undefined) {
    return (
      <p className="chats-picked-up-unread">
        what was said in the editor could not be read
      </p>
    );
  }
  if (view.data.said.length === 0) {
    return <p className="chats-picked-up-cut">nobody spoke in this one.</p>;
  }
  const tail = view.data.said.slice(-SAMPLED);
  return (
    <>
      {(view.data.cut || tail.length < view.data.said.length) && (
        <p className="chats-picked-up-cut">
          the last {tail.length} of it — the rest opens with it
        </p>
      )}
      <ul className="chats-sample" aria-label="What was said, at the end">
        {tail.map((said, index) => (
          <li
            key={`sample-${index}`}
            className={
              said.aside
                ? "chats-sample-line chats-sample-aside"
                : "chats-sample-line"
            }
          >
            {!said.aside && (
              <span className="chats-said-who">
                {said.by_owner ? "you" : "núcleo"}
              </span>
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
/**
 * One editor conversation, and the decision to bring it here.
 *
 * Shown beside the list rather than inside it, because this is not a row's worth of information:
 * what it carries, what the model would be handed, whether the folder even lets it touch a file,
 * and what a resume would cost. A real pick-up of a session near the ceiling billed $1.72 for a
 * one-word answer, and that is the sort of thing this panel exists to say beforehand.
 */
function PickUpPreview({
  sessionId,
  onOpened,
}: {
  sessionId: string;
  onOpened: (chatId: string) => void;
}) {
  const sessions = useIdeSessions(true, true);
  // Watched, not merely read: this may be being typed into while somebody looks at it.
  const said = useIdeConversation(sessionId, true);
  const create = useCreateChat();
  const [model, setModel] = useState<string | null>(null);
  const [effort, setEffort] = useState<string | null>(null);
  const chosen = (sessions.data ?? []).find(
    (session) => session.session_id === sessionId,
  );

  if (chosen === undefined) {
    return (
      <div className="chats-editor-chosen">
        {sessions.data === undefined && !sessions.isError ? (
          <p className="chats-loading">reading your editor sessions…</p>
        ) : (
          <ErrorNote>
            that conversation is not on this machine any more
          </ErrorNote>
        )}
      </div>
    );
  }

  return (
    <div className="chats-editor-chosen">
      <div className="chats-editor-head">
        <h2 className="chats-editor-name">
          {chosen.title ?? chosen.session_id}
        </h2>
        <p className="chats-editor-where">{chosen.cwd}</p>
      </div>

      <WhatItCarries view={said} />
      <Sample view={said} />
      {!chosen.tools && <NoTools session={chosen} />}

      <div className="chats-editor-take">
        <Button
          type="button"
          intent="go"
          disabled={create.isPending}
          onClick={() =>
            create.mutate(
              {
                model: model ?? undefined,
                effort: effort ?? undefined,
                continueSession: chosen.session_id,
              },
              { onSuccess: (result) => onOpened(result.chat_id) },
            )
          }
        >
          Pick it up
        </Button>
        {/* The same question the front door asks, asked here for the same reason: it is answerable
            before the first turn and expensive to change after it. */}
        <ModelMenu
          model={model}
          disabled={create.isPending}
          onPick={setModel}
        />
        <EffortMenu
          model={model}
          effort={effort}
          disabled={create.isPending}
          onPick={setEffort}
        />
      </div>
      {create.isError && <CreateRefusal error={create.error} />}
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
        this session was had in a folder with no núcleo hook — continued here,
        it can talk about the code but <b>cannot read or change any file</b>,
        and cannot run anything
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
    return (
      <ErrorNote>
        the núcleo did not answer — the folder was left alone
      </ErrorNote>
    );
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
  if (!isApiRefusal(error))
    return (
      <ErrorNote>the núcleo did not answer — nothing was opened</ErrorNote>
    );
  return (
    <RefusalNote
      refusal={error}
      sentences={{
        not_found:
          "that session is not on this machine — pick another, or start fresh",
      }}
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
    /* No frame and no title. The page header already says Chats, and the conversation says its own
       name two lines below — a panel captioned "Conversation" around a conversation was a third
       label for a thing nobody was confused about, plus a border down both sides of the reading. */
    <section className="chats-detail-inner">
      {summary !== undefined && (
        <div className="chats-detail-head">
          <TitleEditor chatId={chatId} title={summary.title} />
          <ChatMeta chatId={chatId} />
        </div>
      )}

      {/* Everything that is a RECORD of the conversation scrolls; the head above and the box below
          do not. One scrollbar used to move all three, so reading the middle of a long transcript
          took the title, the model and the place you type off the screen together. */}
      <div className="chats-scroll">
        <Project chatId={chatId} />

        {stale && <StaleNote dataUpdatedAt={transcript.dataUpdatedAt} />}

        {summary !== undefined && summary.ide_session_id !== null && (
          <PickedUp view={pickedUp} handed={transcript.data?.handed ?? []} />
        )}

        {transcript.isError && transcript.data === undefined && (
          <TranscriptError error={transcript.error} />
        )}
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
      </div>

      <Composer chatId={chatId} chat={summary} />
    </section>
  );
}

/** Morning, afternoon or evening, as a pure function so it can be asserted without a clock. */
export function greetingFor(hour: number): string {
  if (hour < 5) return "Still up";
  if (hour < 12) return "Good morning";
  if (hour < 19) return "Good afternoon";
  return "Good evening";
}

/**
 * The front door: nothing open, so the page IS the box.
 *
 * It used to be a paragraph explaining that you should pick something from the list. That is a
 * sign pointing at a door rather than a door — every chat application opens on the thing you type
 * into, and choosing a model before you are allowed to type is a question asked in the wrong order.
 * Cloud answers unless somebody says otherwise, which is what the daemon defaults to anyway.
 */
function NothingOpen() {
  const navigate = useNavigate();
  const start = useStartConversation();

  return (
    <div className="chats-front">
      <div className="chats-front-inner">
        <h2 className="chats-front-greeting">
          {greetingFor(new Date().getHours())}
        </h2>
        <StartBox
          pending={start.isPending}
          onSay={(model, effort, text, images) =>
            start.mutate(
              {
                model: model ?? undefined,
                effort: effort ?? undefined,
                text,
                images,
              },
              {
                onSuccess: (opened) =>
                  void navigate({ to: `/chats/${opened.chat_id}` }),
              },
            )
          }
        />
        {start.isError && <CreateRefusal error={start.error} />}
        <p className="chats-front-note">
          Every turn is a billed run. Nothing here is ever quietly deleted —
          ending a conversation only archives it, and every turn it ever had
          stays readable.
        </p>
      </div>
    </div>
  );
}

/**
 * The box on the front door.
 *
 * Not the `Composer`, which reaches into an open conversation — but it carries the same slash. A
 * command does not need a conversation to exist: the personal ones and the installed plugins' are
 * the same wherever this ends up, so the front door offers exactly the ones that will still be
 * there after the first message. It used to offer nothing at all, and typing `/` where you land is
 * the first thing anybody does.
 *
 * An `@` is the other half and genuinely cannot work here: it names files inside the conversation's
 * folder, and there is no folder until one is opened. It says so in one line rather than swallowing
 * the gesture, because a list that never appears is indistinguishable from a feature that is broken.
 *
 * The model and the effort are held as state and written when the conversation is opened, because
 * there is nothing yet to write them to.
 */
function StartBox({
  pending,
  onSay,
}: {
  pending: boolean;
  onSay: (
    model: string | null,
    effort: string | null,
    text: string,
    images: Attachment[],
  ) => void;
}) {
  const [text, setText] = useState("");
  const [caret, setCaret] = useState(0);
  const [dismissed, setDismissed] = useState<string | null>(null);
  const [highlight, setHighlight] = useState(0);
  const [model, setModel] = useState<string | null>(null);
  const [effort, setEffort] = useState<string | null>(null);
  const [attached, setAttached] = useState<Attachment[]>([]);
  const box = useRef<HTMLTextAreaElement | null>(null);
  const sayable = text.trim() !== "" && !pending;

  const command = commandAt(text, caret);
  const mention = mentionAt(text, caret);
  const live = (at: { query: string } | null) =>
    at !== null && at.query !== dismissed ? at.query : null;
  const commands = useCommands(live(command));

  const choices: Choice[] =
    live(command) === null
      ? []
      : (commands.data?.commands ?? []).map((hit: Command) => ({
          key: hit.name,
          primary: `/${hit.name}${hit.hint === null ? "" : ` ${hit.hint}`}`,
          secondary: hit.description ?? hit.source,
          chosen: () => {
            const written = withCommand(text, command!, hit.name);
            setText(written.text);
            setDismissed(null);
            setHighlight(0);
            // The caret is the whole state this list reads from, and only the element can move it.
            // Next frame, because React has not rendered the new value yet at this point.
            requestAnimationFrame(() => {
              box.current?.focus();
              box.current?.setSelectionRange(written.caret, written.caret);
              setCaret(written.caret);
            });
          },
        }));

  // The one gesture with something to say and nothing to list. A conversation has no folder until
  // it is opened, so there are no names to complete — said out loud, because a list that silently
  // never appears reads as a broken feature rather than as an answered question.
  const noFolderYet = command === null && live(mention) !== null;

  const attach = async (files: FileList | File[] | null) => {
    const pictures = Array.from(files ?? []).filter(isPicture);
    if (pictures.length === 0) return;
    const read = await Promise.all(pictures.map(attachmentFrom));
    setAttached((was) => [...was, ...read].slice(0, MAX_PICTURES));
  };

  const say = () => {
    if (!sayable) return;
    onSay(model, effort, text.trim(), attached);
  };

  return (
    <>
      {noFolderYet && (
        <p className="chats-mentions-none">
          this conversation has no folder yet — open it, point it at a project,
          and an @ will name its files
        </p>
      )}
      {choices.length > 0 && (
        <Choices
          label="Commands to run"
          choices={choices}
          highlight={highlight}
          truncated={false}
        />
      )}
      <form
        className="chats-composer-box chats-front-box"
        onSubmit={(event) => {
          event.preventDefault();
          say();
        }}
      >
        {attached.length > 0 && (
          <ul className="chats-attached" aria-label="Attached pictures">
            {attached.map((picture, index) => (
              <li
                key={`start-attached-${index}`}
                className="chats-attached-item"
              >
                <img
                  className="chats-attached-thumb"
                  alt={`attached picture ${index + 1}`}
                  src={`data:${picture.media_type};base64,${picture.data}`}
                />
                <button
                  type="button"
                  className="chats-attached-drop"
                  aria-label={`Remove attached picture ${index + 1}`}
                  onClick={() =>
                    setAttached((was) => was.filter((_, at) => at !== index))
                  }
                >
                  ×
                </button>
              </li>
            ))}
          </ul>
        )}
        <textarea
          className="chats-composer-text"
          aria-label="Message"
          placeholder="Say something…"
          ref={box}
          rows={1}
          value={text}
          onPaste={(event) => {
            const pictures = Array.from(event.clipboardData.files).filter(
              isPicture,
            );
            if (pictures.length === 0) return;
            event.preventDefault();
            void attach(pictures);
          }}
          onChange={(event) => {
            setText(event.target.value);
            setCaret(event.target.selectionStart);
            setDismissed(null);
            setHighlight(0);
          }}
          // The caret moves without the text changing — arrows, a click, Home — and what is being
          // typed is read from where it IS.
          onSelect={(event) => setCaret(event.currentTarget.selectionStart)}
          onKeyDown={(event) => {
            if (listTookTheKey(event, choices, highlight, setHighlight)) return;
            if ((choices.length > 0 || noFolderYet) && event.key === "Escape") {
              event.preventDefault();
              setDismissed(live(command) ?? live(mention));
              return;
            }
            if (event.key !== "Enter" || event.shiftKey) return;
            event.preventDefault();
            say();
          }}
        />
        <div className="chats-composer-actions">
          <label className="chats-attach" title="Attach a picture">
            <ImagePlus className="chats-tool-icon" aria-hidden="true" />
            <span className="chats-offscreen">Attach a picture</span>
            <input
              type="file"
              accept="image/*"
              multiple
              aria-label="Attach a picture"
              onChange={(event) => {
                void attach(event.target.files);
                event.target.value = "";
              }}
            />
          </label>
          {/* Held as state, not written anywhere: there is no conversation to write it to until the
            first message opens one, and it travels with that message. */}
          <ModelMenu model={model} disabled={pending} onPick={setModel} />
          <EffortMenu
            model={model}
            effort={effort}
            disabled={pending}
            onPick={setEffort}
          />
          <span className="chats-composer-gap" />
          <button
            type="submit"
            className="chats-send"
            aria-label={pending ? "Opening the conversation" : "Send"}
            disabled={!sayable}
          >
            <ArrowUp className="chats-send-icon" aria-hidden="true" />
          </button>
        </div>
      </form>
    </>
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
function ChatMeta({ chatId }: { chatId: string }) {
  const project = useChatProject(chatId);
  const cwd = project.data?.cwd ?? null;
  const tools = project.data?.tools ?? false;
  // Held HERE and not inside `ChatHelpers`, because a dialog rendered inside `DropdownMenuContent`
  // unmounts the moment the menu closes — which the menu does on the very click that opens it. The
  // item lives in the menu; the dialog is its sibling.
  const [helpers, setHelpers] = useState(false);
  const [instructions, setInstructions] = useState(false);
  const row = useChatRow(chatId);
  const helperCount = row?.agents.length ?? 0;
  const instructed = (row?.system_prompt ?? "") !== "";

  return (
    <div className="chats-meta">
      <p className="chats-meta-line">
        {/* The directory is named here only when it is settled. While it is unknown, or
            while it is a state that needs teaching, `Project` below says so in full — a
            summary line is the wrong place to explain something.
            The model and plan-only were here too; both moved into the box, where the words
            they govern are being written. What is left is where this runs. */}
        {cwd !== null && tools && (
          <span className="chats-meta-where">{cwd}</span>
        )}
      </p>

      <DropdownMenu>
        <DropdownMenuTrigger
          className="chats-meta-more"
          aria-label="Conversation settings"
        >
          ⋯
        </DropdownMenuTrigger>
        {/* One thing left in it, and it is the one thing that must not be a menu ITEM: see
            `ArchiveControl`, whose two-click interlock a menu item would collapse. */}
        <DropdownMenuContent align="end" className="chats-meta-menu">
          {/* Settings, not gestures. The model and the effort sit in the box because they are
              changed while writing the message they govern; these three are decided once and left
              alone, so they belong behind the ⋯ rather than in a row you look at all day. */}
          <ChatReach chatId={chatId} />
          <ChatCeiling chatId={chatId} />
          <ChatFallback chatId={chatId} />
          {/* A menu ITEM and not a submenu: helpers are written, not picked. Three text fields and
              two selects do not fit in a menu — and a Radix menu closes on the first keystroke that
              looks like typeahead, which is every keystroke. */}
          <DropdownMenuItem onSelect={() => setHelpers(true)}>
            Helpers
            <span className="chats-tool-why">
              {helperCount === 0 ? "none" : `${helperCount}`}
            </span>
          </DropdownMenuItem>
          {/* Also written rather than picked, and for the same reason a dialog. */}
          <DropdownMenuItem onSelect={() => setInstructions(true)}>
            Standing instructions
            <span className="chats-tool-why">
              {instructed ? "set" : "none"}
            </span>
          </DropdownMenuItem>
          <ChatDenials chatId={chatId} />
          <DropdownMenuSeparator />
          {/* Content and not items, like `ArchiveControl` below: the stronger of the two is a
              `ConfirmButton`, whose two-click interlock a menu item would collapse into one. */}
          <ContextControls chatId={chatId} />
          <DropdownMenuSeparator />
          {/* The one thing that must not be a menu ITEM: see `ArchiveControl`, whose two-click
              interlock a menu item would collapse. */}
          <ArchiveControl chatId={chatId} />
        </DropdownMenuContent>
      </DropdownMenu>

      <ChatHelpers chatId={chatId} open={helpers} onOpenChange={setHelpers} />
      <ChatInstructions
        chatId={chatId}
        open={instructions}
        onOpenChange={setInstructions}
      />
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
      {cwd !== null && !tools && (
        <ProjectWithoutTools chatId={chatId} cwd={cwd} />
      )}
      {cwd !== null && session !== null && (
        <CarryOn cwd={cwd} session={session} />
      )}
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

  const folders = Array.from(
    new Set((sessions.data ?? []).map((session) => session.cwd)),
  );

  return (
    <div className="chats-project">
      <p className="chats-new-warning" role="status">
        this conversation has no project — it can talk about code and remember
        what was said, but it{" "}
        <b>cannot open a file, run a command, or change anything</b> on this
        machine.
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
        <Button
          type="submit"
          intent="go"
          disabled={point.isPending || path.trim() === ""}
        >
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
        this conversation is about {cwd}, which has no núcleo hook — it can talk
        about the code but <b>cannot read or change any file</b>, and cannot run
        anything
      </p>
      <Button
        type="button"
        disabled={wire.isPending}
        onClick={() => wire.mutate()}
      >
        Give it the tools
      </Button>
      {wire.isError && <WireRefusal error={wire.error} cwd={cwd} />}
    </div>
  );
}

function ProjectRefusal({ error }: { error: unknown }) {
  if (!isApiRefusal(error)) {
    return (
      <ErrorNote>
        the núcleo did not answer — the conversation was left as it was
      </ErrorNote>
    );
  }
  return (
    <RefusalNote
      refusal={error}
      sentences={{
        bad_request:
          "that has to be an absolute path to a folder that exists on this machine",
        conflict:
          "this conversation is answering — wait for the turn to end, then move it",
        not_found: "that conversation is no longer here",
      }}
    />
  );
}

function TranscriptError({ error }: { error: unknown }) {
  if (isApiRefusal(error))
    return (
      <RefusalNote
        refusal={error}
        sentences={{ not_found: "this conversation is gone or archived" }}
      />
    );
  return (
    <ErrorNote>
      the núcleo did not answer — nothing is known about this conversation
    </ErrorNote>
  );
}

/* ----------------------------------------------------------------- title -- */

/**
 * The conversation's name, and the way to change it.
 *
 * A name at rest; a field once you ask for one. It was a permanent input and two buttons before,
 * open on every visit whether anybody was renaming anything or not — a form standing on top of the
 * thing you came to read, costing a row of height every time.
 *
 * The draft is seeded when the editor opens rather than held from the first render. A conversation
 * can be renamed from elsewhere — the daemon names one by itself — and a draft that was set once at
 * mount would quietly write a stale name back over it.
 */
function TitleEditor({
  chatId,
  title,
}: {
  chatId: string;
  title: string | null;
}) {
  const [editing, setEditing] = useState(false);
  const [draft, setDraft] = useState("");
  const patch = usePatchChat();
  const auto = usePostChatTitle();

  const close = () => setEditing(false);
  const rename = () => {
    if (draft.trim() === "") return;
    patch.mutate({ chatId, title: draft.trim() }, { onSuccess: close });
  };

  if (!editing) {
    return (
      <div className="chats-title">
        <button
          type="button"
          className="chats-title-name"
          /* Spelled out, because the visible text is the NAME and a button whose whole accessible
             name is the conversation's title announces nothing about what pressing it does. */
          aria-label={`Rename this conversation — currently ${title ?? "unnamed"}`}
          onClick={() => {
            setDraft(title ?? "");
            setEditing(true);
          }}
        >
          {title ?? "New conversation"}
        </button>
      </div>
    );
  }

  return (
    <div className="chats-title">
      <input
        className="chats-title-input"
        aria-label="Conversation title"
        autoFocus
        value={draft}
        onChange={(event) => setDraft(event.target.value)}
        /* Enter commits and Escape abandons, which is what an in-place rename does everywhere.
           Without them the only way out of the field would be the mouse. */
        onKeyDown={(event) => {
          if (event.key === "Enter") {
            event.preventDefault();
            rename();
          }
          if (event.key === "Escape") {
            event.preventDefault();
            close();
          }
        }}
      />
      <Button
        disabled={draft.trim() === "" || patch.isPending}
        onClick={rename}
      >
        Rename
      </Button>
      <Button
        variant="ghost"
        disabled={auto.isPending}
        onClick={() => auto.mutate(chatId, { onSuccess: close })}
      >
        Name it locally
      </Button>
      <Button variant="ghost" onClick={close}>
        Cancel
      </Button>
      {patch.isError && <TitleRefusal error={patch.error} />}
      {auto.isError && <AutoTitleRefusal error={auto.error} />}
    </div>
  );
}

function TitleRefusal({ error }: { error: unknown }) {
  if (!isApiRefusal(error))
    return (
      <ErrorNote>
        the núcleo did not answer — the name was not changed
      </ErrorNote>
    );
  return (
    <RefusalNote
      refusal={error}
      sentences={{
        not_found:
          "this conversation is gone or archived, so there is nothing left to rename",
      }}
    />
  );
}

function AutoTitleRefusal({ error }: { error: unknown }) {
  if (!isApiRefusal(error))
    return (
      <ErrorNote>the núcleo did not answer — no name was proposed</ErrorNote>
    );
  return (
    <RefusalNote
      refusal={error}
      sentences={{
        unavailable: "no local model could name this — nothing has changed",
        conflict:
          "nothing has been said in this conversation yet, so there is nothing to name it after",
      }}
    />
  );
}

/* ----------------------------------------------------------------- model -- */

/**
 * Which model answers.
 *
 * The list comes from the daemon (`GET /assistant/models`) and is never written here. Neither agent
 * CLI can enumerate its own models, so any list is somebody's assertion — and an assertion written
 * into the window is one that goes stale where nobody who can fix it will look.
 *
 * `""` is the unpinned state: follow whatever the daemon is configured with. It is a real state and
 * it is named in the menu rather than left as an empty selection, because a control showing nothing
 * selected reads as broken.
 */
function ModelMenu({
  model,
  onPick,
  disabled = false,
  children,
}: {
  model: string | null;
  onPick: (model: string | null) => void;
  disabled?: boolean;
  children?: React.ReactNode;
}) {
  const catalogue = useAssistantModels();
  const localModel = useLocalModel();
  const localUnavailable = localModel.data?.available === false;

  const choices = catalogue.data?.choices ?? [];
  const chosen = choices.find((choice) => choice.id === model);
  // Unpinned shows the configured model's name, not a blank. Falling back to `model` covers the one
  // case the catalogue cannot explain: a conversation pinned to a name since removed from the
  // config. Showing the stale name is right — it is what the next turn will actually run.
  const shown = chosen?.label ?? model ?? catalogue.data?.configured ?? "model";

  return (
    <DropdownMenu>
      <DropdownMenuTrigger
        className="chats-tool"
        /* Spelled out: the visible text is a model's NAME, and a control whose whole accessible
           name is "Sonnet 5" announces a fact rather than something you can press. */
        aria-label={`Answered by ${shown} — change the model`}
        disabled={disabled}
      >
        {shown}
        <ChevronDown className="chats-tool-caret" aria-hidden="true" />
      </DropdownMenuTrigger>
      <DropdownMenuContent align="start" className="chats-meta-menu">
        <DropdownMenuLabel>Which model answers</DropdownMenuLabel>
        {catalogue.isError && (
          <DropdownMenuItem disabled>
            the núcleo did not say which models it has
          </DropdownMenuItem>
        )}
        <DropdownMenuRadioGroup
          value={model ?? ""}
          onValueChange={(picked) => onPick(picked === "" ? null : picked)}
        >
          {catalogue.data !== undefined && (
            <DropdownMenuRadioItem value="">
              Whatever is configured
              <span className="chats-tool-why">
                {catalogue.data.configured}
              </span>
            </DropdownMenuRadioItem>
          )}
          {choices.map((choice) => (
            <DropdownMenuRadioItem
              key={choice.id}
              value={choice.id}
              /* A local model can be configured and still not be running. The daemon lists it
                 because it is named; this is the separate question of whether it answers. */
              disabled={choice.brain === "local" && localUnavailable}
            >
              {choice.label}
              {choice.brain === "local" && (
                <span className="chats-tool-why">
                  {localUnavailable
                    ? "not running on this machine"
                    : "on this machine"}
                </span>
              )}
            </DropdownMenuRadioItem>
          ))}
        </DropdownMenuRadioGroup>
        {children}
      </DropdownMenuContent>
    </DropdownMenu>
  );
}

/**
 * How hard the model is asked to think.
 *
 * Its own control beside the model's rather than a submenu inside it. They are two decisions and a
 * person changes them separately — most often the effort, on a model they already chose — and a
 * dial buried one level down is one you have to remember is there.
 *
 * The levels are the CHOSEN model's own, which is why this reads the catalogue instead of taking a
 * list: they genuinely differ. `claude-haiku-4-5` takes no effort at all; Opus 4.6 takes `max` but
 * not `xhigh`. A shared list would offer levels that come back as an error.
 *
 * A model with no dial gets the button disabled rather than removed. Hiding it would make the row
 * jump as you switch models, and would read as a feature that is missing rather than one that does
 * not apply here.
 */
function EffortMenu({
  model,
  effort,
  onPick,
  disabled = false,
}: {
  model: string | null;
  effort: string | null;
  onPick: (effort: string | null) => void;
  disabled?: boolean;
}) {
  const catalogue = useAssistantModels();
  const chosen = catalogue.data?.choices.find((choice) => choice.id === model);
  // The union stands in only while nothing is pinned — the front door, where the model question is
  // still open. Never empty merely because a fetch is slow: a control greyed out by latency reads
  // as unavailable rather than as loading.
  const levels = chosen?.efforts ?? catalogue.data?.efforts ?? [];
  const hasDial = levels.length > 0;

  return (
    <DropdownMenu>
      <DropdownMenuTrigger
        className="chats-tool"
        /* Named for the setting and not for the behaviour. "Thinking at high…" was the first
           wording and it collided with the transcript's own rule that a turn which did not think
           offers no `thinking` control — two unrelated things answering to one word is how a
           person clicks the wrong one. */
        aria-label={
          hasDial
            ? `Effort: ${effort ?? "the model's own default"} — change it`
            : `${chosen?.label ?? "this model"} has no effort setting`
        }
        disabled={disabled || !hasDial}
      >
        {hasDial ? (effort ?? "effort") : "no effort"}
        <ChevronDown className="chats-tool-caret" aria-hidden="true" />
      </DropdownMenuTrigger>
      <DropdownMenuContent align="start" className="chats-meta-menu">
        <DropdownMenuLabel>How hard it thinks</DropdownMenuLabel>
        <DropdownMenuRadioGroup
          value={effort ?? ""}
          onValueChange={(picked) => onPick(picked === "" ? null : picked)}
        >
          <DropdownMenuRadioItem value="">
            the CLI's own default
          </DropdownMenuRadioItem>
          {levels.map((level) => (
            <DropdownMenuRadioItem key={level} value={level}>
              {level}
            </DropdownMenuRadioItem>
          ))}
        </DropdownMenuRadioGroup>
      </DropdownMenuContent>
    </DropdownMenu>
  );
}

/**
 * Both controls, wired to a conversation that already exists.
 *
 * Split from the menus themselves because the front door asks the same two questions with no chat
 * to answer them against: there, the answers are state until the first message creates something to
 * write them to.
 */
function ChatModelControls({
  chatId,
  model,
  effort,
}: {
  chatId: string;
  model: string | null;
  effort: string | null;
}) {
  const patch = usePatchChat();
  return (
    <>
      <ModelMenu
        model={model}
        disabled={patch.isPending}
        onPick={(picked) => patch.mutate({ chatId, model: picked })}
      >
        {patch.isError && <ModelRefusal error={patch.error} />}
      </ModelMenu>
      <EffortMenu
        model={model}
        effort={effort}
        disabled={patch.isPending}
        onPick={(picked) => patch.mutate({ chatId, effort: picked })}
      />
    </>
  );
}

/* `BrainPicker` — two buttons named Cloud and Local — stood here, then `BrainMenu`, which offered
   the same two words as a menu. Both asked which ROUTE answered; the pair above asks which MODEL
   and how hard, and the route follows from the answer. */

function ModelRefusal({ error }: { error: unknown }) {
  if (!isApiRefusal(error))
    return (
      <ErrorNote>
        the núcleo did not answer — the model was not changed
      </ErrorNote>
    );
  return (
    <RefusalNote
      refusal={error}
      sentences={{
        conflict:
          "a turn is in flight right now — the model cannot change until it settles",
      }}
    />
  );
}

/* ------------------------------------------------- reach, ceiling, fallback -- */

/** The conversation this control is about, or undefined while the list is still being read. */
function useChatRow(chatId: string): ChatSummary | undefined {
  const chats = useChats();
  return (chats.data ?? []).find((row) => row.chat_id === chatId);
}

/**
 * Which folders this conversation may reach beyond the one it runs in.
 *
 * Checkboxes over folders the editor already knows about, rather than a box to type a path into.
 * Every path here is one a real session ran in, so it is absolute and it exists — the two things
 * the daemon refuses a PATCH for. A text field would put the person in a position to fail a
 * validation they cannot see the rule for.
 *
 * A folder granted and later gone stays checked and stays listed, because it is what the row says
 * and hiding it would make the setting unreachable to turn off.
 */
function ChatReach({ chatId }: { chatId: string }) {
  const row = useChatRow(chatId);
  const patch = usePatchChat();
  // Not watched: these are wanted as a list of folders, and that does not need re-reading.
  const sessions = useIdeSessions(true);

  const granted = row?.extra_dirs ?? [];
  const known = Array.from(
    new Set([
      ...(sessions.data ?? []).map((session) => session.cwd),
      ...granted,
    ]),
  ).filter((folder) => folder !== row?.cwd);

  const toggle = (folder: string, on: boolean) => {
    const next = on
      ? [...granted, folder]
      : granted.filter((path) => path !== folder);
    patch.mutate({ chatId, extra_dirs: next });
  };

  return (
    <DropdownMenuSub>
      <DropdownMenuSubTrigger disabled={known.length === 0}>
        Also reaches
        <span className="chats-tool-why">
          {known.length === 0
            ? "no other folders known"
            : granted.length === 0
              ? "only its own folder"
              : `${granted.length} more`}
        </span>
      </DropdownMenuSubTrigger>
      <DropdownMenuSubContent className="chats-meta-menu">
        <DropdownMenuLabel>Folders its tools may open</DropdownMenuLabel>
        {known.map((folder) => (
          <DropdownMenuCheckboxItem
            key={folder}
            checked={granted.includes(folder)}
            disabled={patch.isPending}
            /* Radix closes the menu on select; several folders are usually granted together, so
               the default is fought here rather than making somebody reopen it each time. */
            onSelect={(event) => event.preventDefault()}
            onCheckedChange={(on) => toggle(folder, on === true)}
          >
            {folder}
          </DropdownMenuCheckboxItem>
        ))}
      </DropdownMenuSubContent>
    </DropdownMenuSub>
  );
}

/** The amounts offered as a ceiling. Presets rather than a number field: this is a guard rail
 *  somebody sets in a second, and a text input invites a typo that reads as a refused turn. */
const CEILINGS = [0.25, 0.5, 1, 2, 5, 10];

/**
 * The most one turn of this conversation may spend.
 *
 * The copy says "per turn" everywhere and never "budget", because the CLI's flag bounds one
 * invocation and the daemon spawns one per turn. Ten turns at the ceiling cost ten times it, and a
 * control that let somebody believe otherwise would be lying about money.
 */
function ChatCeiling({ chatId }: { chatId: string }) {
  const row = useChatRow(chatId);
  const patch = usePatchChat();
  const ceiling = row?.turn_budget_usd ?? null;

  return (
    <DropdownMenuSub>
      <DropdownMenuSubTrigger>
        Spends at most
        <span className="chats-tool-why">
          {ceiling === null ? "no ceiling" : `$${ceiling.toFixed(2)} a turn`}
        </span>
      </DropdownMenuSubTrigger>
      <DropdownMenuSubContent className="chats-meta-menu">
        <DropdownMenuLabel>Per turn, not per conversation</DropdownMenuLabel>
        <DropdownMenuRadioGroup
          value={ceiling === null ? "" : String(ceiling)}
          onValueChange={(picked) =>
            patch.mutate({
              chatId,
              turn_budget_usd: picked === "" ? null : Number(picked),
            })
          }
        >
          <DropdownMenuRadioItem value="">no ceiling</DropdownMenuRadioItem>
          {CEILINGS.map((amount) => (
            <DropdownMenuRadioItem key={amount} value={String(amount)}>
              ${amount.toFixed(2)} a turn
            </DropdownMenuRadioItem>
          ))}
        </DropdownMenuRadioGroup>
      </DropdownMenuSubContent>
    </DropdownMenuSub>
  );
}

/**
 * Who answers when the chosen model is overloaded or unavailable.
 *
 * One name, where the daemon accepts a list. A single fallback is the whole of what anybody has
 * asked for, and an ordered list is a control with drag handles in it — the API keeps the door open
 * for the day that is worth building.
 */
function ChatFallback({ chatId }: { chatId: string }) {
  const row = useChatRow(chatId);
  const patch = usePatchChat();
  const catalogue = useAssistantModels();

  const named = (row?.fallback_model ?? "")
    .split(",")
    .filter((name) => name !== "");
  const choices = (catalogue.data?.choices ?? []).filter(
    (choice) => choice.brain === "cloud" && choice.id !== row?.model,
  );
  const shown = catalogue.data?.choices.find(
    (choice) => choice.id === named[0],
  );

  return (
    <DropdownMenuSub>
      <DropdownMenuSubTrigger disabled={choices.length === 0}>
        Falls back to
        <span className="chats-tool-why">
          {shown?.label ?? named[0] ?? "nobody"}
        </span>
      </DropdownMenuSubTrigger>
      <DropdownMenuSubContent className="chats-meta-menu">
        <DropdownMenuLabel>When the model is overloaded</DropdownMenuLabel>
        <DropdownMenuRadioGroup
          value={named[0] ?? ""}
          onValueChange={(picked) =>
            patch.mutate({
              chatId,
              fallback_model: picked === "" ? [] : [picked],
            })
          }
        >
          <DropdownMenuRadioItem value="">
            nobody
            <span className="chats-tool-why">the turn fails instead</span>
          </DropdownMenuRadioItem>
          {choices.map((choice) => (
            <DropdownMenuRadioItem key={choice.id} value={choice.id}>
              {choice.label}
            </DropdownMenuRadioItem>
          ))}
        </DropdownMenuRadioGroup>
      </DropdownMenuSubContent>
    </DropdownMenuSub>
  );
}

/* ------------------------------------------- instructions, denials, context -- */

/**
 * Standing instructions for one conversation, appended to the model's own system prompt.
 *
 * Appended and never substituted, which is the whole reason this is safe to expose. The flag that
 * REPLACES the system prompt exists and is deliberately unreachable from here: it would drop the
 * tool descriptions and the safety framing with it, and a conversation that lost those would read
 * as one whose model had quietly got worse.
 *
 * A draft with an explicit Save, like the helpers, and for the same reason: this text goes out on
 * every turn, and a patch per keystroke would send dozens of half-written sentences as though each
 * were somebody's finished instruction.
 */
function ChatInstructions({
  chatId,
  open,
  onOpenChange,
}: {
  chatId: string;
  open: boolean;
  onOpenChange: (open: boolean) => void;
}) {
  const row = useChatRow(chatId);
  const patch = usePatchChat();
  const [draft, setDraft] = useState("");

  const saved = row?.system_prompt;
  // Seeded once per opening, guarded by the ref for the reason `ChatHelpers` gives: the list behind
  // this is polled, so an effect that merely depended on it would take back what was being typed.
  const seeded = useRef(false);
  useEffect(() => {
    if (!open) {
      seeded.current = false;
      return;
    }
    if (seeded.current || saved === undefined) return;
    seeded.current = true;
    setDraft(saved ?? "");
  }, [open, saved]);

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="chats-helpers">
        <DialogHeader>
          <DialogTitle>Standing instructions</DialogTitle>
          <DialogDescription>
            Added to what this conversation's model is already told — on every
            turn, not just the first. Nothing here replaces the model's own
            instructions.
          </DialogDescription>
        </DialogHeader>

        <label className="chats-helper-field">
          <span>Instructions</span>
          <textarea
            rows={8}
            value={draft}
            onChange={(event) => setDraft(event.target.value)}
            placeholder="Answer in European Portuguese. Prefer the smallest correct change."
          />
        </label>

        <DialogFooter>
          <Button
            variant="approve"
            disabled={patch.isPending}
            onClick={() =>
              patch.mutate(
                // Blank goes as an explicit `null`. `undefined` would be dropped by
                // `JSON.stringify` and read as "leave it alone", which is the one thing emptying
                // the box is not.
                { chatId, system_prompt: draft.trim() === "" ? null : draft },
                { onSuccess: () => onOpenChange(false) },
              )
            }
          >
            Save
          </Button>
        </DialogFooter>
        {patch.isError && <HelperRefusal error={patch.error} />}
      </DialogContent>
    </Dialog>
  );
}

/**
 * What this conversation may not reach for.
 *
 * A DENY list and not an allow list, and the difference is not cosmetic: the CLI's allow-listing
 * flag does not restrict anything — it grants permission on top of what is already permitted — so a
 * control built on it would read as a restriction and be none. Everything here can only take
 * something away.
 *
 * Checkboxes over the names the daemon serves, never a box to type in. A typed rule that matches no
 * tool is reported as one line on the CLI's stderr, which in this app is a restriction somebody set
 * and nobody applied.
 */
function ChatDenials({ chatId }: { chatId: string }) {
  const row = useChatRow(chatId);
  const patch = usePatchChat();
  const deniable = useDeniableTools();

  const denied = row?.denied_tools ?? [];
  const tools = deniable.data?.tools ?? [];

  const toggle = (name: string, on: boolean) => {
    const next = on
      ? [...denied, name]
      : denied.filter((tool) => tool !== name);
    patch.mutate({ chatId, denied_tools: next });
  };

  return (
    <DropdownMenuSub>
      <DropdownMenuSubTrigger disabled={tools.length === 0}>
        Cannot use
        <span className="chats-tool-why">
          {denied.length === 0 ? "nothing barred" : `${denied.length} barred`}
        </span>
      </DropdownMenuSubTrigger>
      <DropdownMenuSubContent className="chats-meta-menu chats-denials">
        <DropdownMenuLabel>
          Tools this conversation may not reach for
        </DropdownMenuLabel>
        {tools.map((name) => (
          <DropdownMenuCheckboxItem
            key={name}
            checked={denied.includes(name)}
            disabled={patch.isPending}
            /* Several are usually barred together, so the menu is kept open. */
            onSelect={(event) => event.preventDefault()}
            onCheckedChange={(on) => toggle(name, on === true)}
          >
            {name}
          </DropdownMenuCheckboxItem>
        ))}
      </DropdownMenuSubContent>
    </DropdownMenuSub>
  );
}

/**
 * The two ways to start this conversation over, which are deliberately two.
 *
 * They answer different questions. "This is getting expensive" wants a fresh window with a short
 * replay of what was recently said — which the daemon already does by itself once a context grows
 * past its threshold, and which until now nobody could ask for before the bill arrived. "We are
 * done with that, start again" wants the next turn told nothing at all. One button would have to
 * guess which was meant.
 *
 * Neither deletes anything. Every turn stays in the transcript, still readable and still costing
 * what it cost, and the transcript draws a mark where a clear happened — the same position the rest
 * of this app takes about rewriting history.
 */
function ContextControls({ chatId }: { chatId: string }) {
  const fresh = useFreshContext();
  const clear = useClearContext();

  return (
    <div className="chats-context">
      <Button disabled={fresh.isPending} onClick={() => fresh.mutate(chatId)}>
        Fresh context
      </Button>
      <p className="chats-context-why">
        the next turn starts on a new window and is told what was recently said
      </p>
      {/* The interlock, because this one cannot be undone by pressing it again: the floor only ever
          moves forward. `ConfirmButton` is why this block is menu CONTENT — a menu item would close
          on the first click and collapse two deliberate steps into one. */}
      <ConfirmButton
        label="Clear"
        confirmLabel="Clear — the turns stay, the model stops seeing them"
        onConfirm={() => clear.mutate(chatId)}
      />
      <p className="chats-context-why">and this one tells it nothing at all</p>
      {fresh.isError && <HelperRefusal error={fresh.error} />}
      {clear.isError && <HelperRefusal error={clear.error} />}
    </div>
  );
}

/* --------------------------------------------------------------- helpers -- */

/** A helper as it is being written, before anybody has agreed it is one. */
type HelperDraft = Subagent & { model: string | null; effort: string | null };

/** What a fresh row starts as. Named so "add" and "reset" cannot drift apart. */
function blankHelper(): HelperDraft {
  return { name: "", description: "", prompt: "", model: null, effort: null };
}

/**
 * Why the daemon would refuse this helper, in the words of what goes wrong — or null.
 *
 * A copy of the door's rules, and copies drift. This one is worth keeping anyway: the daemon
 * answers a bad set with a bare 400 and no body, so without this the whole dialog would say "no"
 * about a set of five helpers without saying which one or why. The door stays the authority — this
 * only ever refuses EARLIER, never instead, and anything it misses still comes back as a refusal.
 *
 * The rules themselves are not arbitrary: the CLI parses `--agents` inside a try/catch and answers
 * a throw with an empty agent list, so a helper it cannot build costs you every helper you wrote,
 * silently. That is what all of this is protecting against.
 */
function whyHelperIsRefused(
  helper: HelperDraft,
  others: HelperDraft[],
): string | null {
  const name = helper.name.trim();
  if (name === "") return "needs a name — it is what the model calls it by";
  if (name.startsWith("-"))
    return "cannot start with a dash: that reads as a flag";
  if (!/^[A-Za-z0-9_-]+$/.test(name))
    return "letters, digits, dashes and underscores only";
  if (others.some((other) => other !== helper && other.name.trim() === name))
    return "another helper already has this name, and one would replace the other";
  if (helper.description.trim() === "")
    return "needs a description — it is what the model reads to decide whether to use it";
  if (helper.prompt.trim() === "") return "needs instructions to run under";
  return null;
}

/**
 * The helpers this conversation may hand work to.
 *
 * A dialog and not a menu, because these are WRITTEN. A name, a sentence about when to use it, and
 * the instructions it runs under — that is three text fields, and a menu that closes on typeahead
 * cannot hold one of them.
 *
 * Edited as a draft and saved in one gesture. The other settings in this menu patch on the click
 * that changes them, which is right for a radio button and wrong here: a PATCH per keystroke would
 * send dozens of half-written helpers, and each one is a whole-set write the daemon would take at
 * face value.
 *
 * The set is saved WHOLE, which is also how the daemon takes it. So the answer to "two windows
 * saved at once" is the plain one — the last save wins — rather than a merge rule nobody can see.
 */
function ChatHelpers({
  chatId,
  open,
  onOpenChange,
}: {
  chatId: string;
  open: boolean;
  onOpenChange: (open: boolean) => void;
}) {
  const row = useChatRow(chatId);
  const patch = usePatchChat();
  const catalogue = useAssistantModels();
  const [draft, setDraft] = useState<HelperDraft[]>([]);

  const saved = row?.agents;
  // Seeded ONCE per opening, which the ref is what enforces. The list behind `saved` is polled, so
  // every poll hands back a new array — and an effect that merely depended on it would reseed the
  // draft mid-sentence and take back whatever had been typed since the last tick. Found by a test
  // that added a second helper and then waited: the wait was long enough for a poll to land.
  //
  // `saved === undefined` holds the seeding off until the list has actually arrived. Without it, a
  // dialog opened before the first fetch would seed itself empty and then never look again — the
  // conversation's real helpers invisible, and one Save away from being deleted.
  const seeded = useRef(false);
  useEffect(() => {
    if (!open) {
      seeded.current = false;
      return;
    }
    if (seeded.current || saved === undefined) return;
    seeded.current = true;
    setDraft(
      saved.map((agent) => ({
        ...agent,
        model: agent.model ?? null,
        effort: agent.effort ?? null,
      })),
    );
  }, [open, saved]);

  const choices = (catalogue.data?.choices ?? []).filter(
    (choice) => choice.brain === "cloud",
  );
  const refusals = draft.map((helper) => whyHelperIsRefused(helper, draft));
  const ready = refusals.every((why) => why === null);

  const change = (at: number, patched: Partial<HelperDraft>) =>
    setDraft((current) =>
      current.map((helper, index) =>
        index === at ? { ...helper, ...patched } : helper,
      ),
    );

  const save = () => {
    patch.mutate(
      // Trimmed here rather than left to the daemon: what is stored is what the next turn is
      // handed, and a helper called "reviewer " is one nothing can call by that name.
      {
        chatId,
        agents: draft.map((helper) => ({
          name: helper.name.trim(),
          description: helper.description.trim(),
          prompt: helper.prompt.trim(),
          model: helper.model,
          effort: helper.effort,
        })),
      },
      { onSuccess: () => onOpenChange(false) },
    );
  };

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="chats-helpers">
        <DialogHeader>
          <DialogTitle>Helpers</DialogTitle>
          <DialogDescription>
            Work this conversation can hand off. These are added to any the
            project already defines — they never hide them.
          </DialogDescription>
        </DialogHeader>

        {draft.length === 0 && (
          <p className="chats-helpers-none">
            None yet. A helper is a name, what it is for, and the instructions
            it runs under.
          </p>
        )}

        <ul className="chats-helpers-list">
          {draft.map((helper, index) => (
            // Keyed by position, deliberately. A helper's name is what somebody is editing — keying
            // by it would rebuild the field on every keystroke and take the caret with it.
            <li key={index} className="chats-helper">
              <label className="chats-helper-field">
                <span>Name</span>
                <input
                  value={helper.name}
                  onChange={(event) =>
                    change(index, { name: event.target.value })
                  }
                  placeholder="reviewer"
                />
              </label>
              <label className="chats-helper-field">
                <span>When to use it</span>
                <input
                  value={helper.description}
                  onChange={(event) =>
                    change(index, { description: event.target.value })
                  }
                  placeholder="Reviews a diff for correctness"
                />
              </label>
              <label className="chats-helper-field">
                <span>Instructions</span>
                <textarea
                  rows={3}
                  value={helper.prompt}
                  onChange={(event) =>
                    change(index, { prompt: event.target.value })
                  }
                  placeholder="You are a code reviewer. Read the diff and…"
                />
              </label>
              <div className="chats-helper-row">
                <label className="chats-helper-field">
                  <span>Model</span>
                  <select
                    value={helper.model ?? ""}
                    onChange={(event) =>
                      // Changing the model can strip an effort the new one does not take. Cleared
                      // here rather than left to be refused at the door, where the message would be
                      // about a field nobody touched.
                      change(index, {
                        model:
                          event.target.value === "" ? null : event.target.value,
                        effort: null,
                      })
                    }
                  >
                    <option value="">same as this conversation</option>
                    {choices.map((choice) => (
                      <option key={choice.id} value={choice.id}>
                        {choice.label}
                      </option>
                    ))}
                  </select>
                </label>
                <label className="chats-helper-field">
                  <span>Effort</span>
                  <select
                    value={helper.effort ?? ""}
                    onChange={(event) =>
                      change(index, {
                        effort:
                          event.target.value === "" ? null : event.target.value,
                      })
                    }
                  >
                    <option value="">its model's default</option>
                    {/* The levels the helper's OWN model takes, or the union while it inherits —
                        the same split the daemon checks against, so the menu cannot offer a level
                        the door would refuse. */}
                    {(helper.model === null
                      ? (catalogue.data?.efforts ?? [])
                      : (choices.find((choice) => choice.id === helper.model)
                          ?.efforts ?? [])
                    ).map((level) => (
                      <option key={level} value={level}>
                        {level}
                      </option>
                    ))}
                  </select>
                </label>
                <Button
                  variant="danger"
                  onClick={() =>
                    setDraft((current) =>
                      current.filter((_, at) => at !== index),
                    )
                  }
                >
                  Remove
                </Button>
              </div>
              {refusals[index] !== null && (
                <p className="chats-helper-why" role="alert">
                  {refusals[index]}
                </p>
              )}
            </li>
          ))}
        </ul>

        <DialogFooter>
          <Button
            onClick={() => setDraft((current) => [...current, blankHelper()])}
          >
            Add a helper
          </Button>
          <Button
            variant="approve"
            disabled={!ready || patch.isPending}
            onClick={save}
          >
            Save
          </Button>
        </DialogFooter>
        {patch.isError && <HelperRefusal error={patch.error} />}
      </DialogContent>
    </Dialog>
  );
}

/** Why the daemon would not take this set. Its own component for the reason `ArchiveRefusal` is:
 *  a refusal has a code and a sentence, and anything else is the daemon not answering at all. */
function HelperRefusal({ error }: { error: unknown }) {
  if (!isApiRefusal(error))
    return (
      <ErrorNote>
        the núcleo did not answer — the helpers were not saved
      </ErrorNote>
    );
  return <RefusalNote refusal={error} />;
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
        onConfirm={() =>
          archive.mutate(chatId, {
            onSuccess: () => void navigate({ to: "/chats" }),
          })
        }
      />
      {archive.isError && <ArchiveRefusal error={archive.error} />}
    </div>
  );
}

function ArchiveRefusal({ error }: { error: unknown }) {
  if (!isApiRefusal(error))
    return (
      <ErrorNote>
        the núcleo did not answer — the conversation was not archived
      </ErrorNote>
    );
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
    return (
      <p className="chats-loading">reading what was said in the editor…</p>
    );
  }
  if (view.data === undefined) {
    return (
      <p className="chats-picked-up-unread">
        what was said in the editor could not be read — only the turns below are
        shown
      </p>
    );
  }
  if (view.data.said.length === 0) {
    return (
      <p className="chats-picked-up-cut">
        this was picked up from a conversation in the editor that nobody spoke
        in.
      </p>
    );
  }
  return (
    <>
      {/* Said above the text, where the missing part would have been, rather than under it as a
          footnote. A person reads down from the top; the top is exactly where the gap is. */}
      {view.data.cut && (
        <p className="chats-picked-up-cut">
          older messages are not shown — this conversation was read from its
          recent end
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
              <span className="chats-said-who">
                {said.by_owner ? "you" : "núcleo"}
              </span>
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
        picked up here — everything above was said in the editor and read back
        out of its own file. None of it was a run, and none of it was billed
        here.
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
        this session was resumed, so the model has all of the above in its
        context.
      </p>
    );
  }
  return (
    <div className="chats-handed">
      <p className="chats-handed-line">
        this session was too large to resume, so it was not. The model was
        handed the last{" "}
        {handed.length === 1 ? "exchange" : `${handed.length} exchanges`} of it,
        word for word, in front of an empty context — everything above them is
        here for you to read, not something it remembers.
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
        <ul
          className="chats-handed-list"
          aria-label="What the model was handed"
        >
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
      {open && diff.isError && <ChangedRefusal error={diff.error} />}
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
    return (
      <p className="chats-changed-clean">nothing in this project has changed</p>
    );
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
    return (
      <ErrorNote>the núcleo did not answer — nothing could be read</ErrorNote>
    );
  }
  return (
    <RefusalNote
      refusal={error}
      sentences={{
        conflict:
          "this conversation has no project, so there is no working tree to compare",
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
  // Where this conversation was told to forget everything before, so the mark lands on the right
  // turn. Read here rather than inside each block: it is one fact about the whole transcript.
  const clearedAfter = useChatRow(chatId)?.cleared_after_run_id ?? null;

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
  if (turns.length === 0)
    return <p className="chats-empty">nothing has been said yet.</p>;
  return (
    <>
      <ul className="chats-turns" aria-label="Transcript">
        {turns.map((turn, index) => (
          <TurnBlock
            key={turn.id}
            turn={turn}
            previous={index === 0 ? null : turns[index - 1]}
            clearedAfter={clearedAfter}
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
  clearedAfter,
  chatId,
}: {
  turn: Turn;
  previous: Turn | null;
  clearedAfter: number | null;
  chatId: string;
}) {
  const marks = marksBetween(previous, turn, clearedAfter);
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
        <p className="chats-turn-answer chats-turn-answer-empty">
          no answer recorded
        </p>
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
    return (
      <p className={`chats-rich-heading chats-rich-heading-${line.level}`}>
        {inner}
      </p>
    );
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
function ContextFill({
  fill,
  rotatesAt,
}: {
  fill: number | null;
  rotatesAt: number | null;
}) {
  if (fill === null) return null;
  const k = (n: number) => `${(n / 1000).toFixed(1)}k`;
  if (rotatesAt === null)
    return <span className="chats-turn-fill">{k(fill)} of context</span>;
  // Near, not past. Past is too late to be a warning: the turn that crosses the line is the last
  // one that remembers, and this is drawn under it while the next one is still being typed.
  const near = fill >= rotatesAt * 0.85;
  return (
    <span
      className={
        near ? "chats-turn-fill chats-turn-fill-near" : "chats-turn-fill"
      }
    >
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
    <Button
      type="button"
      variant="ghost"
      disabled={stop.isPending}
      onClick={() => stop.mutate(turnId)}
    >
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
      <Thought
        thought={live.data?.thought ?? []}
        tokens={live.data?.thought_tokens ?? null}
      />
      {text !== "" && (
        <p className="chats-turn-answer chats-turn-writing">{text}</p>
      )}
      <Plan todos={planOf(live.data?.did ?? [])} />
      <WhatItDid did={live.data?.did ?? []} />
      <p className="chats-turn-live" ref={end}>
        {doing !== null
          ? `running ${doing}…`
          : text === ""
            ? "thinking…"
            : "writing…"}
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
  if (gone)
    return (
      <p className="chats-picture-gone">
        a picture sent here can no longer be read
      </p>
    );
  if (url === null)
    return <p className="chats-picture-gone">reading a picture…</p>;
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
        <img
          className="chats-picture"
          src={url}
          alt={`sent with this message: ${path}`}
        />
      </button>
      {open && (
        <PictureOverlay url={url} path={path} onClose={() => setOpen(false)} />
      )}
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
      <button
        type="button"
        className="chats-picture-close"
        aria-label="Close picture"
      >
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
function Thought({
  thought,
  tokens,
}: {
  thought: string[];
  tokens: number | null;
}) {
  const [open, setOpen] = useState(false);
  if (thought.length === 0 && tokens === null) return null;
  const size =
    tokens === null
      ? null
      : tokens >= 1000
        ? `${(tokens / 1000).toFixed(1)}k`
        : `${tokens}`;
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
          {open
            ? "hide thinking"
            : size === null
              ? "thinking"
              : `thinking · ~${size} tokens`}
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
        <li
          key={`todo-${index}`}
          className={`chats-plan-item chats-plan-${todo.status}`}
        >
          <span className="chats-plan-mark" aria-hidden="true">
            {todo.status === "completed"
              ? "✓"
              : todo.status === "in_progress"
                ? "→"
                : "·"}
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
          {call.detail !== null && (
            <span className="chats-turn-did-detail">{call.detail}</span>
          )}
        </li>
      ))}
    </ul>
  );
}

/**
 * The brain, restart and clear marks a transcript draws above one turn.
 *
 * The brain mark's copy is deliberately asymmetric: moving *to* the cloud is about where what you
 * type now goes, and moving *to* the local model is about where the answer comes from — the two
 * directions are not mirror images of the same fact.
 *
 * A clear restarts the session too, so it could carry both marks. It carries only this one, because
 * the restart note's own words — "was read the last few exchanges back" — are exactly what a clear
 * makes untrue.
 */
function MarkNote({ mark }: { mark: Mark }) {
  if (mark.kind === "cleared") {
    return (
      <p className="chats-mark chats-mark-restart" role="status">
        cleared here — everything above stays readable, and the model past this
        point was told none of it
      </p>
    );
  }
  if (mark.kind === "restart") {
    return (
      <p className="chats-mark chats-mark-restart" role="status">
        the conversation restarted here — the model past this point was read the
        last few exchanges back, and remembers nothing older than those
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
  turn_in_progress:
    "this conversation already has a turn in flight — it clears on its own once that turn answers",
  kill_switch:
    "the kill switch is engaged; nothing autonomous starts until it is released, and this cannot be sent either",
  no_local_model:
    "no local model is available on this machine, and this conversation is set to answer locally",
  errand_not_answering:
    "the errand behind this conversation is not answering right now",
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

/**
 * The keys a list under the caret owns while it is showing.
 *
 * Shared by both boxes rather than written twice. The arrows, the Enter and the Tab belong to the
 * list for as long as it is open — which is what every editor does and what the hand expects — and
 * two copies of that rule is how one of them comes to disagree the day somebody adds a key.
 *
 * Returns `true` when the key was the list's, so the caller knows not to send.
 */
function listTookTheKey(
  event: { key: string; preventDefault: () => void },
  choices: Choice[],
  highlight: number,
  setHighlight: (next: (was: number) => number) => void,
): boolean {
  if (choices.length === 0) return false;
  if (event.key === "ArrowDown") {
    event.preventDefault();
    setHighlight((was) => (was + 1) % choices.length);
    return true;
  }
  if (event.key === "ArrowUp") {
    event.preventDefault();
    setHighlight((was) => (was - 1 + choices.length) % choices.length);
    return true;
  }
  if (event.key === "Enter" || event.key === "Tab") {
    event.preventDefault();
    choices[Math.min(highlight, choices.length - 1)].chosen();
    return true;
  }
  return false;
}

function Composer({
  chatId,
  chat,
}: {
  chatId: string;
  /** The row, or undefined while the list is still being read. */
  chat: ChatSummary | undefined;
}) {
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
            chosen: () =>
              write(withMention(text, mention, hit.path, hit.is_dir)),
          }))
        : [];

  // A file gesture over a conversation with no directory is the one case with something to say and
  // nothing to list. A command gesture never has it: personal and plugin commands exist wherever
  // the conversation runs.
  const nowhere =
    command === null &&
    mention !== null &&
    live(mention) !== null &&
    files.data?.rooted === false;
  const open = choices.length > 0 || nowhere;

  // One place, two ways in: the button and the key. Duplicating the guards into the key handler is
  // how one of them ends up sending an empty turn six months from now.
  // A picture on its own is a message: "what is this?" is a reasonable thing to send with nothing
  // typed, and refusing it because the box is empty would be the window deciding what counts.
  const sayable =
    (text.trim() !== "" || attached.length > 0) && !send.isPending;
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
                onClick={() =>
                  setAttached((was) => was.filter((_, at) => at !== index))
                }
              >
                ×
              </button>
            </li>
          ))}
        </ul>
      )}
      {/* One object you type into, with the actions inside it — see `.chats-composer-box`. The
          visible "Message" label went with the frame; the textarea has carried its own `aria-label`
          all along, so the accessible name is exactly what it was. */}
      <div className="chats-composer-box">
        <textarea
          className="chats-composer-text"
          placeholder="Say something…"
          ref={box}
          // Pasting is the gesture: a screenshot goes to the clipboard and then into the box, and
          // anything that made you save it to a file first would be a step nobody takes.
          onPaste={(event) => {
            const pictures = Array.from(event.clipboardData.files).filter(
              isPicture,
            );
            if (pictures.length === 0) return;
            // Only when there IS a picture: a plain text paste must stay a text paste.
            event.preventDefault();
            void attach(pictures);
          }}
          /* The floor, not the size. `field-sizing: content` grows the box from here; this is what
             it falls back to where that is unsupported. */
          rows={1}
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
            if (listTookTheKey(event, choices, highlight, setHighlight)) return;
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
        {/* The controls belong to the words being typed, so they live in the box with them — which
            is the one structure every reference for this page shares. They used to be scattered:
            the model and plan-only behind a `⋯` at the top of the page, the attach button in a
            strip under the box. Asking "which model, and does it plan or does it do" a screen away
            from the sentence those answers apply to is asking about the message somewhere the
            message is not. */}
        <div className="chats-composer-actions">
          {/* The way in for anything not on the clipboard. Hidden behind its own label because a
              bare file input is the one control on this page nobody can style into the others. */}
          <label className="chats-attach" title="Attach a picture">
            <ImagePlus className="chats-tool-icon" aria-hidden="true" />
            <span className="chats-offscreen">Attach a picture</span>
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
          {chat !== undefined && (
            <ChatModelControls
              chatId={chatId}
              model={chat.model}
              effort={chat.effort}
            />
          )}
          <Planning chatId={chatId} />
          <span className="chats-composer-gap" />
          <button
            type="submit"
            className="chats-send"
            aria-label="Send"
            disabled={!sayable}
          >
            <ArrowUp className="chats-send-icon" aria-hidden="true" />
          </button>
        </div>
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
            className={
              index === highlight
                ? "chats-mention chats-mention-on"
                : "chats-mention"
            }
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
        <li className="chats-mentions-cut">
          more than these — keep typing to narrow it
        </li>
      )}
    </ul>
  );
}

function MessageRefusal({ error }: { error: unknown }) {
  if (!isApiRefusal(error))
    return <ErrorNote>the núcleo did not answer — nothing was sent</ErrorNote>;
  return <RefusalNote refusal={error} sentences={MESSAGE_SENTENCES} />;
}
