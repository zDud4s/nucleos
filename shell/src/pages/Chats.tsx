import {
  createContext,
  memo,
  useCallback,
  useContext,
  useEffect,
  useRef,
  useState,
  type Dispatch,
  type RefObject,
  type SetStateAction,
} from "react";
import { createPortal } from "react-dom";
import { Link, useNavigate, useParams } from "@tanstack/react-router";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuGroup,
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
  ArrowUp,
  ChevronDown,
  Mic,
  MicOff,
  MoreHorizontal,
  PanelRightOpen,
  ChevronRight,
  FileMinus,
  FilePen,
  FilePlus,
  FileSymlink,
  GitCompareArrows,
  LoaderCircle,
  SquarePen,
} from "lucide-react";
import { isApiRefusal } from "../data/client";
import {
  useArchiveChat,
  useAssistantModels,
  useModelGroups,
  useChatTranscript,
  useChats,
  useIdeConversation,
  useIdeSessions,
  useSetChatProject,
  useSetPermissionMode,
  useWireChatTools,
  useWireIdeSessionTools,
  useAnswerAsk,
  useChatCommands,
  useCommands,
  useChatDiff,
  useChatFiles,
  useChatProject,
  useDropQueued,
  useForwardTurn,
  useLiveTurn,
  useRelayChain,
  useLocalModel,
  useLocalPull,
  useLocalSize,
  usePullLocalModel,
  usePatchChat,
  usePostChatSeen,
  usePostChatTitle,
  useSendMessage,
  useStartConversation,
  useStopTurn,
  type Attachment,
  type ChatSummary,
  type PermissionMode,
  type Subagent,
  useClearContext,
  useDeniableTools,
  useFreshContext,
  useOlderTurns,
  useSaid,
  useTurnTools,
  type SaidHit,
  type Command,
  type Exchange,
  type Ask,
  type Mention,
  type Waiting,
  type IdeSession,
  type ToolCall,
  type Turn,
} from "../data/chats";
import { type RelaySent } from "../lib/turns";
import { type ChatNotice } from "../data/chats";
import { usePaletteGroup, usePaletteQuery } from "../ui";
import {
  anyTurnLive,
  marksBetween,
  planOf,
  turnIsLive,
  type Mark,
  type RelayedFrom,
  type Todo,
} from "../lib/turns";
import {
  blocks,
  lines,
  spans as spansOf,
  type Block as RichBlock,
  type Line as RichLine,
  type Span as RichSpan,
} from "../lib/rich";
import { openUrl } from "@tauri-apps/plugin-opener";
import { commandAt, mentionAt, withCommand, withMention } from "../lib/mention";
import { fetchFileBlob } from "../data/files";
import { attachmentFrom, isPicture } from "../lib/picture";
import { PlusMenu } from "../chats/PlusMenu";
import { Slider } from "../ui/Slider";
import { ProviderMark } from "../ui/ProviderMark";
import { displayName } from "../lib/modelName";
import { diffFiles, type DiffFile, type FileChange } from "../lib/diff";
import { Diff } from "../ui/Diff";
import { Popover as PopoverPrimitive } from "radix-ui";
import { useDictation, type DictationView } from "../data/dictation";
import { revise, type Provisional } from "../lib/provisional";
import type { LocalPull, ModelChoice, ModelGroup } from "../data/chats";
import {
  Button,
  ConfirmButton,
  Modal,
  CopyButton,
  CostLine,
  ErrorNote,
  IconButton,
  Quiet,
  RefusalNote,
  RelativeTime,
  StaleNote,
} from "../ui";
import { elapsedText } from "../lib/when";
import { SessionColumn } from "../chats/SessionColumn";
import { ChatTabs } from "../chats/ChatTabs";
import { ChatChips } from "../chats/AgentMap";
import { ProjectPicker } from "../chats/ProjectPicker";
import { holdMessage, takeHeld, useHeldMessage } from "../chats/held";
import { useChatTabs } from "../chats/tabs";

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
  /**
   * The editor conversation being considered, if any.
   *
   * State and not a route, because there is nothing to route TO: an editor session is a file on
   * this machine, not a conversation this app has opened, and it has no id here until somebody
   * picks it up.
   *
   * **It outranks the route, so everything that opens a conversation has to clear it.** That is
   * `openingAChat` below, and it is not a nicety: this is the only thing on the page whose value
   * decides what the right-hand column draws, and the column's other input is the URL. Being state
   * rather than a route means a `<Link>` cannot clear it on the way past — so the two disagreed,
   * and the disagreement was invisible in the worst way. The URL changed, the row lit up as
   * current, and the editor session stayed on screen: a conversation you had just pressed simply
   * would not open.
   *
   * Two exits, and both are needed. One is a session BECOMING a chat — by then it has a URL of its
   * own. The other is a chat being opened instead, which is the one that was missing.
   */
  const [pickingUp, setPickingUp] = useState<string | null>(null);
  /**
   * Every way a conversation gets opened runs through here, and there are three: a row in the list,
   * a conversation in the palette, and a turn found by searching.
   *
   * One function rather than three `setPickingUp(null)` calls, because the thing being stated is an
   * invariant — *the editor preview does not survive opening a conversation* — and an invariant
   * spelled three times is one somebody adds a fourth caller beside without noticing.
   *
   * It does not navigate. The list's rows are real `<Link>`s and should stay that way: middle-click
   * and copy-link are worth more than the symmetry of routing everything through one handler.
   */
  const openingAChat = () => setPickingUp(null);
  /**
   * A turn picked out of a search, waiting for its conversation to be on screen.
   *
   * Stamped like `reuse` is, and for the same reason: finding the SAME turn twice is a real
   * gesture — you jump to it, scroll away reading, and go looking for it again — and an effect
   * watching only the id would fire once and then quietly stop working.
   *
   * Held here rather than in the palette because the palette closes on the way: the thing that has
   * to remember is the page, which is still standing when the conversation finishes loading.
   */
  const [found, setFound] = useState<{
    chatId: string;
    turnId: number;
    at: number;
  } | null>(null);
  const navigate = useNavigate();
  /**
   * The conversations held open as tabs. Whether a tab is open lives only here and in
   * `localStorage`: the daemon knows nothing of it, so an archived or deleted chat is dropped
   * against the list once that has loaded.
   */
  const tabs = useChatTabs();
  const openTab = tabs.open;
  const pruneTabs = tabs.prune;
  useEffect(() => {
    if (chatId !== null) openTab(chatId);
  }, [chatId, openTab]);
  useEffect(() => {
    // The open conversation is always known, even when the list was fetched before it existed.
    if (chats.data !== undefined)
      pruneTabs([...chats.data.map((row) => row.chat_id), ...(chatId === null ? [] : [chatId])]);
  }, [chats.data, chatId, pruneTabs]);
  /** Closing the open tab moves to its neighbour, or back to the front door when it was alone. */
  const closeTab = (id: string) => {
    const next = tabs.close(id, chatId);
    if (id !== chatId) return;
    void navigate({ to: next === null ? "/chats" : `/chats/${next}` });
  };
  const unseen = rows.reduce((total, row) => total + row.waiting, 0);
  const query = usePaletteQuery();
  const said = useSaid(query);
  const needle = query.trim().toLowerCase();

  // Matched here rather than by cmdk, and `shouldFilter={false}` below is the other half of that.
  // The hits underneath were matched by the daemon against the whole text of a conversation, which
  // is text this list does not have — left to cmdk they would be filtered out again for not
  // containing the query in their own visible row.
  const named = rows.filter((row) => {
    if (needle === "") return true;
    const name = row.title ?? row.first_message ?? "New conversation";
    return `${name} ${row.cwd ?? ""}`.toLowerCase().includes(needle);
  });
  const hits = said.data ?? [];

  usePaletteGroup({
    id: ["chats", "named"].join("-"),
    heading: "Conversations",
    items: named.map((row) => {
      const name = row.title ?? row.first_message ?? "New conversation";
      return {
        id: `chat-${row.chat_id}`,
        label: <>
          <span className="chats-palette-title">{name}</span>
          {row.cwd !== null && <span className="chats-palette-where">{row.cwd}</span>}
          {row.waiting > 0 && <span className="chats-palette-waiting">{row.waiting}</span>}
        </>,
        match: `${name} ${row.cwd ?? ""}`,
        run: () => {
          openingAChat();
          void navigate({ to: `/chats/${row.chat_id}` });
        },
      };
    }),
  });
  usePaletteGroup(
    hits.length === 0
      ? null
      : {
          id: "chats-said",
          heading: "Said in a conversation",
          prematched: true,
          items: hits.map((hit) => ({
            id: `said-${hit.turn_id}`,
            label: <SaidRow hit={hit} />,
            match: hit.excerpt,
            run: () => {
              openingAChat();
              void navigate({ to: `/chats/${hit.chat_id}` });
              setFound({ chatId: hit.chat_id, turnId: hit.turn_id, at: Date.now() });
            },
          })),
        },
  );

  /** The conversation the page is about, when there is one; `null` on the front door. */
  const open = pickingUp === null && chatId !== null ? (summary ?? null) : null;

  return (
    /* The class that turns this route from a document into an application: see `.chats-app`, which
       stops the shell scrolling the whole page and hands the height to the two columns below. */
    <div className="chats-app">
      {/* No header band. The page's top row is the tabs strip, and the conversation's own controls
          ride at its end — so the heading the outline needs is said to a screen reader only. The
          subject is the conversation on screen when there is one, and "Chats" when there is not. */}
      <h1 className="sr-only">
        {open === null ? "Chats" : (open.title ?? "New conversation")}
      </h1>

      {stale && <StaleNote dataUpdatedAt={chats.dataUpdatedAt} />}
      {chats.isError && chats.data === undefined && (
        <ListError error={chats.error} />
      )}

      <div
        className={
          railOpen ? "chats-layout" : "chats-layout chats-layout-alone"
        }
      >
        <div className="chats-detail">
          {/* The top row of the conversation pane. Empty — no tabs, the list open, nothing
              chosen — it draws nothing at all: see `.chats-topbar:empty`. */}
          <div className="chats-topbar">
            <ChatTabs
              tabs={tabs.tabs}
              rows={rows}
              current={pickingUp === null ? chatId : null}
              selectedLive={selectedLive}
              onOpenChat={openingAChat}
              onClose={closeTab}
            />
            {open !== null && chatId !== null && (
              /* What the header band used to carry beside the conversation's name: where it runs,
                 the cache and agent chips, and the `⋯` — rename included now, see `ChatMenu`. */
              <div className="chats-topbar-actions">
                <ChatWhere chatId={chatId} />
                <ChatChips
                  turns={transcript.data?.turns}
                  chatTitle={open.title ?? "Conversation"}
                />
                <ChatMenu chatId={chatId} />
              </div>
            )}
            {!railOpen && (
              /* The way back to the list, at the end of the row, where the list's own edge was. Never only a shortcut:
                 a closed list with no visible way to reopen it is a dead end. */
              <span className="chats-topbar-show">
                <IconButton
                  /* The count is in the name, because the badge beside the glyph is drawn for
                     the eye only — "Show conversations" and "5" would otherwise read as one word. */
                  label={
                    unseen > 0
                      ? `Show conversations, ${unseen} unseen`
                      : "Show conversations"
                  }
                  icon={PanelRightOpen}
                  onClick={() => setRailOpen(true)}
                />
                {/* Answers that landed while you were elsewhere. Shown here precisely because the
                    list they are in is closed. */}
                {unseen > 0 && (
                  <span className="chats-unseen" aria-hidden="true">
                    {unseen}
                  </span>
                )}
              </span>
            )}
          </div>
          {/* An editor conversation opens in this column like any other, because to the person
              looking at the list it IS any other — see `EditorDetail`. */}
          {pickingUp !== null && (
            <EditorDetail
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
              /* Only when it is THIS conversation's turn. A jump left over from a search in
                 another chat would otherwise hunt for an id that is not on the page. */
              find={found !== null && found.chatId === chatId ? found : null}
            />
          )}
        </div>

        {railOpen && (
          /* The ground that tells the list from the thread. See `.chats-rail`: it sits on the
             RIGHT, after the conversation. */
          <div className="chats-rail">
            <SessionColumn
              rows={rows}
              answered={chats.data !== undefined}
              selected={chatId}
              selectedLive={selectedLive}
              pickingUp={pickingUp}
              onPickUp={setPickingUp}
              onOpenChat={openingAChat}
              onNew={() => {
                setPickingUp(null);
                void navigate({ to: "/chats" });
              }}
              onHide={() => setRailOpen(false)}
              openTabs={tabs.tabs}
            />
          </div>
        )}
      </div>
    </div>
  );
}

/**
 * Where this conversation runs, as one line at the end of the tabs strip.
 *
 * The directory is named here only when it is settled. While it is unknown, or while it
 * is a state that needs teaching, `Project` in the transcript says so in full — a
 * one-line slot is the wrong place to explain something.
 *
 * The "what is different" chip (`ProjectChanges`) rides beside it, and needs only a folder to
 * compare: it shows whenever there is one, settled or not.
 *
 * A second reader of `useChatProject` and never a second source: react-query answers both
 * this and the menu below out of one cache entry, so the line and the settings cannot
 * disagree about which folder a conversation is in.
 */
function ChatWhere({ chatId }: { chatId: string }) {
  const project = useChatProject(chatId);
  const cwd = project.data?.cwd ?? null;
  const tools = project.data?.tools ?? false;
  if (cwd === null) return null;
  return (
    <>
      {tools && (
        <p className="chats-where" title={cwd}>
          {cwd}
        </p>
      )}
      <ProjectChanges chatId={chatId} />
    </>
  );
}

function ListError({ error }: { error: unknown }) {
  if (isApiRefusal(error)) return <RefusalNote refusal={error} />;
  return (
    <ErrorNote>
      the núcleo did not answer — nothing is known about your conversations
    </ErrorNote>
  );
}

/** One search hit: whose half it was, what it said, and which conversation it was said in. */
function SaidRow({ hit }: { hit: SaidHit }) {
  return (
    <span className="chats-palette-said">
      <span className="chats-palette-said-head">
        <span className="chats-palette-said-who">
          {hit.side === "asked" ? "you" : "núcleo"}
        </span>
        <span className="chats-palette-title">
          {hit.title ?? "New conversation"}
        </span>
        <RelativeTime at={hit.created_at} />
      </span>
      <span className="chats-palette-said-text">{hit.excerpt}</span>
    </span>
  );
}

/* The column of sessions, its rows and the editor rows now live in `chats/SessionColumn`;
   `mergeRows` is re-exported so existing imports keep working. */
export { mergeRows, type ListRow } from "../chats/SessionColumn";

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
 * Two different futures, said plainly rather than as a number to interpret. Nearly every
 * conversation is continued where it left off, in a window widened to hold what it is carrying, and
 * costs what its context costs on the first turn. Past the largest window any model has there is
 * nothing to continue INTO, and it is handed the last few exchanges instead.
 *
 * That second case used to start at 140k, which made it the ordinary fate of a long afternoon. It
 * now starts near 190k, which is arithmetic rather than policy.
 */
function WhatItCarries({
  view,
}: {
  view: ReturnType<typeof useIdeConversation>;
}) {
  const carries = view.data?.context_estimate ?? null;
  if (carries === null) return null;
  const ceiling = view.data?.largest_window ?? null;
  const k = (n: number) => `${(n / 1000).toFixed(1)}k`;
  const over = ceiling !== null && carries > ceiling;
  return (
    <p className={over ? "chats-carries chats-carries-over" : "chats-carries"}>
      {`about ${k(carries)} of context`}
      {over
        ? " — larger than any window a model has, so picking it up hands the model the last few exchanges instead"
        : " — picked up where it left off, in a window wide enough to hold it"}
    </p>
  );
}

/**
 * An editor conversation, opened here exactly as one of this app's own.
 *
 * It used to be a panel: a name, a directory, six sampled lines, a warning, and a button marked
 * "Pick it up". Everything on it was true and it was still the wrong shape — clicking a
 * conversation in the list opened a FORM about a conversation, while clicking the one below it
 * opened the conversation. Two kinds of row that look identical must not open two kinds of thing.
 *
 * So this is `ChatDetail`'s skeleton, filled from a different source: a name, the folder, the whole
 * of what was said, and a box to type in. What was a decision with a button on it is now the first
 * thing you say — which is how the front door has always worked, and is the same gesture at both
 * doors.
 *
 * The pick-up has not stopped mattering; it has stopped being a screen. What it carries and whether
 * the folder gives it tools are said as notes above the conversation, which is exactly where a chat
 * of this app's own says the equivalent about its project. And nothing is spent until somebody
 * speaks: opening one of these reads a file and bills nothing.
 *
 * After the first message this view is replaced by `ChatDetail` on the new conversation — which
 * draws the same lines, in the same order, above the turn that was just sent. The seam between the
 * two is meant to be invisible, because there is nothing there to see.
 */
function EditorDetail({
  sessionId,
  onOpened,
}: {
  sessionId: string;
  onOpened: (chatId: string) => void;
}) {
  const sessions = useIdeSessions(true, true);
  // Watched, not merely read: this may be being typed into in the editor while it is on screen.
  const said = useIdeConversation(sessionId, true);
  const start = useStartConversation();
  const { box, noteScroll, keepUp } = useFollowsItsEnd();
  const zoom = useChatZoom();
  const chosen = (sessions.data ?? []).find(
    (session) => session.session_id === sessionId,
  );

  // Above the early return, because the rules of hooks do not bend for a session that is not on
  // this machine any more. This door had no end-scroll AT ALL: a conversation opened from the
  // editor is exactly the one whose history is long and whose last line is the only one you have
  // not read, and it opened at somebody else's first sentence of the day.
  useEffect(() => {
    keepUp();
  }, [keepUp, sessionId, said.data]);

  if (chosen === undefined) {
    return (
      <section className="chats-detail-inner">
        <div className="chats-scroll">
          {sessions.data === undefined && !sessions.isError ? (
            <p className="chats-loading">reading your editor sessions…</p>
          ) : (
            <ErrorNote>
              that conversation is not on this machine any more
            </ErrorNote>
          )}
        </div>
      </section>
    );
  }

  return (
    <section className="chats-detail-inner">
      <div className="chats-detail-head">
        <div className="chats-title">
          {/* Not a rename button. There is no row to write a name to until the first message
              opens one, and a control that silently does nothing is worse than none. */}
          <p className="chats-title-name chats-title-fixed">
            {chosen.title ?? chosen.session_id}
          </p>
        </div>
        {/* The same quiet line a conversation has, carrying the one thing that is knowable about
            this one: where it was had. No `⋯` — everything behind it writes to a chat row, and
            there is no chat row yet. */}
        <div className="chats-meta">
          <p className="chats-meta-line">
            <span className="chats-meta-where">{chosen.cwd}</span>
          </p>
        </div>
      </div>

      <div
        className={`chats-scroll ${zoom}`}
        ref={box}
        onScroll={noteScroll}
      >
        {/* Above the conversation, which is where `ChatDetail` puts the equivalent notes about a
            chat's own project. Two things are worth saying before anybody speaks, and both are
            about what the next turn would be, not about what this screen is. */}
        {!chosen.tools && <NoTools session={chosen} />}
        <WhatItCarries view={said} />
        <PickedUp view={said} handed={[]} already={false} />
      </div>

      <StartBox
        pending={start.isPending}
        placeholder="Carry on where you left off…"
        /* The `@` note, corrected for this one case: there IS a folder — it is the one the session
           was had in — and what is missing is a conversation here to ask about it. */
        noFolderNote={`this conversation is not open here yet — say something to carry it on, and an @ will name the files in ${chosen.cwd}`}
        /* The one door that can answer this before the conversation exists. `chosen.tools` is the
           same question the turn itself asks, already answered for this session — see
           `OfferedSession`. */
        tools={chosen.tools}
        onSay={(model, effort, mode, text, images) =>
          start.mutate(
            {
              model: model ?? undefined,
              effort: effort ?? undefined,
              permissionMode: mode,
              continueSession: chosen.session_id,
              text,
              images,
            },
            { onSuccess: (opened) => onOpened(opened.chat_id) },
          )
        }
      />
      {start.isError && <CreateRefusal error={start.error} />}
    </section>
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

/**
 * The sizes the conversation is read at, as percentages of the page's own.
 *
 * A ladder and not a multiplier, because the class is the only way to say this: the window runs
 * under a CSP with no `unsafe-inline`, so a `style` attribute computed from a number would be
 * dropped and every step would come out at 100. Same reason `.chats-rich-depth-*` is a ladder.
 *
 * Weighted downwards — five steps below the page's size and four above. Zooming a conversation is
 * nearly always an attempt to get MORE of it on the screen; the ones above exist for reading
 * something dense, not for the common case.
 */
const ZOOMS = [67, 75, 80, 90, 100, 110, 125, 150, 175, 200] as const;
const ZOOM_REST = 4;
const ZOOM_KEY = "chats.zoom";

/**
 * `Ctrl` and `+`/`-`, but only for the record of the conversation.
 *
 * Not the webview's own zoom, which was tried first and is wrong for this: it takes the whole
 * window with it — the rail, the page header, the list, the box you type into — and what somebody
 * asking for a smaller conversation wants is a smaller CONVERSATION. The chrome around it is
 * already the size it should be.
 *
 * On `window` rather than on the scrolling box, because the box is not focusable and a zoom that
 * only answered when you had clicked inside the transcript first would look broken. Mounted with
 * the conversation, so it listens only while there is one open.
 */
function useChatZoom() {
  const [step, setStep] = useState(() => {
    // `null` handled before `Number` sees it. `Number(null)` is 0, which is a perfectly valid
    // index into the ladder — so a window that had never been zoomed opened every conversation at
    // the smallest setting there is, and the default was unreachable until you pressed the keys.
    const kept = localStorage.getItem(ZOOM_KEY);
    const asStep = kept === null ? Number.NaN : Number(kept);
    return Number.isInteger(asStep) && asStep >= 0 && asStep < ZOOMS.length
      ? asStep
      : ZOOM_REST;
  });

  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      // `altKey` excluded so this never eats a combination somebody meant for the OS.
      if (!(event.ctrlKey || event.metaKey) || event.altKey) return;
      // `=` as well as `+`, because the unshifted key is what most keyboards actually send, and
      // `_` as well as `-` for the same reason on the other side.
      const move =
        event.key === "+" || event.key === "="
          ? 1
          : event.key === "-" || event.key === "_"
            ? -1
            : event.key === "0"
              ? 0
              : null;
      if (move === null) return;
      event.preventDefault();
      setStep((was) => {
        const next =
          move === 0
            ? ZOOM_REST
            : Math.min(ZOOMS.length - 1, Math.max(0, was + move));
        localStorage.setItem(ZOOM_KEY, String(next));
        return next;
      });
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  return `chats-zoom-${ZOOMS[step]}`;
}

/** Nearer the end than this and the reader is reading the end, not passing through it. */
const FOLLOWS_WITHIN_PX = 48;

/**
 * A box that opens at its end and keeps up with it until the reader goes elsewhere in it.
 *
 * A conversation is read at its end. Opening one at the top means scrolling past an afternoon of
 * work to reach the sentence you came back for, and on a conversation picked up from the editor
 * that is somebody else's whole day above the two turns you just had.
 *
 * **The box, not a sentinel element inside it.** That was the previous mechanism and it landed
 * short every single time, measurably: what it aimed at was the end of the TRANSCRIPT, and the
 * transcript is not the last thing in the box. A question the run is waiting on, the files it
 * changed and anything queued behind it are all drawn under it — and each of those is something
 * you would want to see more than the answer above it. `scrollHeight` is the end of the box
 * whatever happens to be in it, so nothing has to be remembered when something new is added.
 *
 * The other half is `follows`, which is what stops it fighting the reader: it is set from where
 * the reader actually left the box, so scrolling up to read something is enough to stop it, and
 * scrolling back down is enough to start it again. A ref rather than state on purpose — nothing
 * draws differently because of it, and state here would re-render the whole transcript on every
 * scroll event.
 */
/**
 * How something deep in the transcript reaches the box it is drawn into.
 *
 * A context and not two more props. `LiveAnswer` is the only thing that needs this and it sits
 * under `Transcript` and `TurnBlock`, neither of which has anything to do with scrolling — passing
 * a callback about the page's scrollbar through both would be threading a concern through two
 * components that never touch it, to reach one leaf.
 *
 * The default does nothing, so drawing a live turn outside a scrolling box — a test, or a door not
 * written yet — is not a crash. Nothing scrolls, which is the honest answer when there is no box.
 */
const KeepsUp = createContext<() => void>(() => {});

function useFollowsItsEnd() {
  const box = useRef<HTMLDivElement | null>(null);
  const follows = useRef(true);

  /** Belongs on the box's own `onScroll`. Where the reader is is what decides this. */
  const noteScroll = useCallback(() => {
    const b = box.current;
    if (b === null) return;
    follows.current =
      b.scrollHeight - b.clientHeight - b.scrollTop <= FOLLOWS_WITHIN_PX;
  }, []);

  const keepUp = useCallback(() => {
    const b = box.current;
    if (b === null || !follows.current) return;
    b.scrollTop = b.scrollHeight;
  }, []);

  return { box, follows, noteScroll, keepUp };
}

function ChatDetail({
  chatId,
  summary,
  transcript,
  find,
}: {
  chatId: string;
  summary: ChatSummary | undefined;
  transcript: ReturnType<typeof useChatTranscript>;
  /** A turn to scroll to, from a search. See `found` in `Chats`. */
  find: { turnId: number; at: number } | null;
}) {
  const seen = usePostChatSeen();
  const pickedUp = useIdeConversation(summary?.ide_session_id ?? null);
  const markedSeen = useRef(false);
  const { box, follows, noteScroll, keepUp } = useFollowsItsEnd();
  const zoom = useChatZoom();
  /**
   * A question from the transcript, on its way into the box.
   *
   * Held here because the transcript and the composer are siblings and neither can hand the
   * other anything. It carries a stamp as well as the words, and the stamp is the point: put
   * the SAME question back twice and the text does not change, so an effect keyed on the text
   * alone would fire once and then quietly stop working.
   */
  const [reuse, setReuse] = useState<{ text: string; at: number } | null>(null);
  // Stable, because it is handed to every turn and `TurnBlock` is memoised.
  const reuseQuestion = useCallback((text: string) => setReuse({ text, at: Date.now() }), []);
  /**
   * Whether the project picker is open. Held here because two siblings open it: the line that
   * stands in for a missing project, and the composer when somebody speaks before choosing one.
   */
  const [picking, setPicking] = useState(false);

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

  // On the whole of `transcript.data` rather than on a count of turns: an answer landing where
  // "thinking…" was grows the page without adding a row to it, and so does a question the run
  // starts waiting on. Running every poll costs nothing — at the end it is an assignment that
  // changes no pixel, and away from the end `keepUp` declines to do anything at all.
  useEffect(() => {
    keepUp();
  }, [keepUp, chatId, transcript.data]);

  const stale = transcript.isError && transcript.data !== undefined;

  return (
    /* No frame and no title. The tab above already names the conversation — a panel captioned
       "Conversation" around a conversation was a label for a thing nobody was confused about, plus
       a border down both sides of the reading. */
    <section className="chats-detail-inner">
      {/* The head that was here — the name and the `⋯` — lives in the tabs strip above: the tab
          carries the name, and the strip's end carries the `⋯` (see `chats-topbar-actions`). */}

      {/* Everything that is a RECORD of the conversation scrolls; the header above and the box
          below do not. One scrollbar used to move all three, so reading the middle of a long
          transcript took the title, the model and the place you type off the screen together. */}
      <div
        className={`chats-scroll ${zoom}`}
        ref={box}
        onScroll={noteScroll}
      >
        <Project chatId={chatId} picking={picking} onPicking={setPicking} />

        {stale && <StaleNote dataUpdatedAt={transcript.dataUpdatedAt} />}

        {summary !== undefined && summary.ide_session_id !== null && (
          <PickedUp
            view={pickedUp}
            handed={transcript.data?.handed ?? []}
            already
          />
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
            notices={transcript.data.notices}
            precededBy={(pickedUp.data?.said ?? []).length > 0}
            chatId={chatId}
            more={transcript.data.more}
            find={find}
            follows={follows}
            keepUp={keepUp}
            onReuse={reuseQuestion}
          />
        )}
        {/* Below the transcript and above the box, which is where these words are in time: said
            after everything above them, and not yet said at all. */}
        {/* Above what is waiting to be said, because this is what everything else is waiting ON:
            a turn is held while a question stands, and the queue behind it cannot move until it is
            answered. */}
        <Asking asks={transcript.data?.asks ?? []} chatId={chatId} />
        <Waiting queued={transcript.data?.queued ?? []} chatId={chatId} />
      </div>

      <Composer
        chatId={chatId}
        chat={summary}
        reuse={reuse}
        onNeedsProject={() => setPicking(true)}
      />
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
          onSay={(model, effort, mode, text, images) =>
            start.mutate(
              {
                model: model ?? undefined,
                effort: effort ?? undefined,
                permissionMode: mode,
                text,
                images,
                // A conversation opened here has no project, and does not start until it has one:
                // the words wait in the new conversation, which opens its project picker.
                holdFirstMessage: true,
              },
              {
                onSuccess: (opened) => {
                  holdMessage(opened.chat_id, { text, images }, MAX_PICTURES);
                  void navigate({ to: `/chats/${opened.chat_id}` });
                },
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
  placeholder = "Say something…",
  noFolderNote = "this conversation has no folder yet — open it, point it at a project, and an @ will name its files",
  tools = false,
}: {
  pending: boolean;
  onSay: (
    model: string | null,
    effort: string | null,
    mode: PermissionMode,
    text: string,
    images: Attachment[],
  ) => void;
  /** What the empty box invites. The front door says one thing; a conversation being carried on
      from the editor says another, and both are the same gesture. */
  placeholder?: string;
  /* `standing` was here — `front` for the middle of an empty page and `thread` for the
     foot of a conversation — and the only thing it decided was whether the box got a
     `--shadow-md`. Nothing else on the front door floats, so the shadow went and the
     prop with it: one box, one frame, wherever it stands. */
  /** What to say when an `@` cannot be answered here. See the call in `EditorDetail`. */
  noFolderNote?: string;
  /**
   * Whether the conversation this opens would get `Bash`, `Read` and `Write`.
   *
   * Passed in rather than read here, because the two doors know different things: an editor
   * session has been asked the question already and carries the answer, and the front door has no
   * directory yet to ask about. Absent means no, which greys the three rungs that are only about
   * what a hook lets through and leaves `Plan` and `Auto`, both of which mean something with no
   * tools at all.
   */
  tools?: boolean;
}) {
  const [text, setText] = useState("");
  const [caret, setCaret] = useState(0);
  const [dismissed, setDismissed] = useState<string | null>(null);
  const [highlight, setHighlight] = useState(0);
  const [model, setModel] = useState<string | null>(null);
  const [effort, setEffort] = useState<string | null>(null);
  const [mode, setMode] = useState<PermissionMode>("auto");
  const [attached, setAttached] = useState<Attachment[]>([]);
  const box = useRef<HTMLTextAreaElement | null>(null);
  const sayable = text.trim() !== "" && !pending;

  const dictation = useDictationInto(text, setText, setCaret, box);

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

  // See the same function in `Composer`: the "+" menu's way of typing an `@`.
  const mentionHere = () => {
    const at = box.current?.selectionStart ?? text.length;
    const lead = at > 0 && !/\s/.test(text[at - 1]) ? " " : "";
    const next = `${text.slice(0, at)}${lead}@${text.slice(at)}`;
    const place = at + lead.length + 1;
    setText(next);
    setDismissed(null);
    requestAnimationFrame(() => {
      box.current?.focus();
      box.current?.setSelectionRange(place, place);
      setCaret(place);
    });
  };

  const say = () => {
    if (!sayable) return;
    onSay(model, effort, mode, text.trim(), attached);
  };

  return (
    <>
      {noFolderYet && <p className="chats-mentions-none">{noFolderNote}</p>}
      {choices.length > 0 && (
        <Choices
          label="Commands to run"
          choices={choices}
          highlight={highlight}
          truncated={false}
        />
      )}
      <form
        /* One class in both places now. `.chats-front-box` existed only to add a
           `--shadow-md` on the front door, and nothing else on that page floats — the box
           IS the page there, not a dialog standing on it. */
        className="chats-composer-box"
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
        {/* The same one-line arrangement the conversation's own composer uses, and for the same
            reason: the microphone belongs on the line being written on, not in the row of settings
            about the message. `.chats-composer-line` keeps the button at the top as the box grows. */}
        <div className="chats-composer-line">
          <textarea
            className="chats-composer-text"
            aria-label="Message"
            placeholder={placeholder}
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
          <DictateToggle dictation={dictation} />
        </div>
        <div className="chats-composer-actions">
          <PlusMenu
            chatId={null}
            onPictures={(files) => void attach(files)}
            onText={(context) =>
              setText((was) => (was === "" ? context : `${was}\n${context}`))
            }
            onMention={mentionHere}
          />
          {/* Held as state, not written anywhere: there is no conversation to write it to until the
            first message opens one, and it travels with that message. */}
          <ModelMenu model={model} disabled={pending} onPick={setModel} />
          <EffortSlider
            model={model}
            effort={effort}
            disabled={pending}
            onPick={setEffort}
          />
          <span className="chats-composer-gap" />
          {/* The one word about the microphone, on a row that exists at this height whether it
              says anything or not. It was a paragraph under the box until 2026-09-20, and under
              the box is under a composer pinned to the foot of a conversation: every "nothing was
              heard" pushed the transcript up a line and the next press pulled it back down. It
              shrinks rather than grows, with the whole sentence on `title`. */}
          {dictation.trouble !== null && (
            <span className="chats-dictation-trouble" title={dictation.trouble}>
              {dictation.trouble}
            </span>
          )}
          {/* Here and not only in the conversation's own composer, which is where it went first and
              was the wrong half of the app: a session carried on from the editor is not a chat
              until its first message opens one, so until now the only way to reach a rung was to
              send a message on `auto` and change it afterwards -- which is exactly one message too
              late for the rung anybody reaches for. It travels on the opening call for the reason
              the model does, and is written into the row rather than after it for a reason the
              model does not have: see `chats::create_on`. */}
          <PermissionPicker
            mode={mode}
            onPick={setMode}
            tools={tools}
            barred={
              tools
                ? ""
                : "not until this conversation has a folder with the classifier hook wired"
            }
            disabled={pending}
          />
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
 * Dictation, wired into whichever box is being typed in.
 *
 * One copy for the two boxes that exist — the front door's and an open conversation's — because the
 * appending is not the obvious `setText(was + said)` and getting it subtly wrong in one of them would
 * read as the microphone misbehaving rather than as two implementations drifting.
 *
 * Added after what was typed, not instead of it: somebody who typed half a sentence and then reached
 * for the microphone meant to continue it. The separating space is added only where there is not
 * already one, so dictating twice in a row does not open a gap that widens on every turn. The caret
 * is moved to the end because the box is written into next, and a caret left where it was would put
 * the following word in the middle of what was just said.
 *
 * **The microphone owns a span of the box, and revises it.** Progressive dictation says the same
 * sentence several times over as the transcriber hears more of it, so every revision but the first
 * REPLACES the last — `lib/provisional.ts` does that arithmetic and says why. The span closes when
 * the sentence does, and the next sentence opens a new one after it.
 *
 * **What the person does to the box wins, always.** The span is only honoured while the box still
 * holds exactly what the microphone last left there; anything else — a word typed, a character
 * deleted, the box emptied by sending the message — makes the next revision open a fresh span at the
 * end instead of writing over text somebody has since made their own. Checked rather than signalled
 * from the textarea's `onChange`, because that would leave the same rule to be remembered at two
 * call sites and silently half-applied at the one that forgot.
 */
function useDictationInto(
  text: string,
  setText: Dispatch<SetStateAction<string>>,
  setCaret: Dispatch<SetStateAction<number>>,
  box: RefObject<HTMLTextAreaElement | null>,
): DictationView {
  /* The box's current text, and the microphone's span of it. Refs because a revision may land
     between two renders — a draft resolves right behind the final it was waiting on — and the state
     React would hand back is the one from before the last revision. Kept level with the state on
     every render, which is what makes a keystroke visible here. */
  const textRef = useRef(text);
  const regionRef = useRef<Provisional | null>(null);
  /** The last thing the microphone wrote, whole. A box that no longer matches it is not ours. */
  const wroteRef = useRef<string | null>(null);
  textRef.current = text;

  return useDictation((said, final) => {
    const was = textRef.current;
    const next = revise(was, wroteRef.current === was ? regionRef.current : null, said);
    textRef.current = next.text;
    regionRef.current = final ? null : next.region;
    wroteRef.current = next.text;
    setText(next.text);
    setCaret(next.caret);
    queueMicrotask(() => {
      const field = box.current;
      if (field === null) return;
      field.focus();
      field.setSelectionRange(next.caret, next.caret);
    });
  });
}

/**
 * Speaking into the box instead of typing into it.
 *
 * **What it is not: a spoken conversation.** That one hears a sentence, sends it as a turn, and plays
 * the answer out loud, never showing what it heard until it has already acted on it. This one
 * produces TEXT and stops — and by the owner's decision of 2026-09-19 it is what BOTH boxes on this
 * page do, the front door's and an open conversation's alike. The spoken conversation moves to the
 * Voice page.
 *
 * The reason is the same in both places and it is not a smaller ambition. On the front door the first
 * sentence is what CREATES the conversation, and one opened on a misheard sentence is a billed row
 * that can be archived and never deleted. In an open chat the sentence is a turn, and `câmbio` coming
 * back as `Campeu` — measured with the owner's voice on 2026-09-20 — is a turn spent on a question
 * nobody asked. Transcription is wrong often enough that the correction has to be possible.
 *
 * So what lands is a draft, in the box, with the caret after it — read it, fix the word it got
 * wrong, and press send.
 */
function DictateToggle({ dictation }: { dictation: DictationView }) {
  const listening = dictation.phase === "listening";
  const writing = dictation.phase === "transcribing";

  return (
    <button
      type="button"
      className={listening ? "chats-handsfree chats-handsfree-on" : "chats-handsfree"}
      aria-pressed={listening}
      aria-label={listening ? "Stop dictating" : "Dictate"}
      // Disabled only while the last sentence is being written down. Two recordings racing two
      // transcripts into one box is the state this forecloses.
      disabled={writing}
      title={
        listening
          ? "stop, and write down what was said"
          : "say it instead of typing it — the words land in the box for you to check before sending"
      }
      onClick={dictation.toggle}
    >
      {listening ? (
        <MicOff className="chats-tool-icon" aria-hidden="true" />
      ) : (
        <Mic className="chats-tool-icon" aria-hidden="true" />
      )}
      {listening && <span className="chats-handsfree-phase">listening</span>}
      {writing && <span className="chats-handsfree-phase">writing it down</span>}
    </button>
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
function ChatMenu({ chatId }: { chatId: string }) {
  const project = useChatProject(chatId);
  const cwd = project.data?.cwd ?? null;
  // Held HERE and not inside `ChatHelpers`, because a dialog rendered inside `DropdownMenuContent`
  // unmounts the moment the menu closes — which the menu does on the very click that opens it. The
  // item lives in the menu; the dialog is its sibling.
  const [helpers, setHelpers] = useState(false);
  const [instructions, setInstructions] = useState(false);
  const [renaming, setRenaming] = useState(false);
  const row = useChatRow(chatId);
  const helperCount = row?.agents.length ?? 0;
  const instructed = (row?.system_prompt ?? "") !== "";

  return (
    /* No line of its own any more. What this component used to carry beside the `⋯` — the
       directory — is the page header's headline now (`ChatWhere`), which is where "one
       derived sentence about the subject" belongs on every other page in the app. What is
       left here is the menu and the two dialogs it opens. */
    <>
      <DropdownMenu>
        <DropdownMenuTrigger
          className="chats-meta-more"
          aria-label="Conversation settings"
        >
          {/* The icon, not a `⋯` typed into the line. A horizontal-ellipsis character is
              punctuation: it sits on the text baseline, it takes the line's own size, and it
              cannot be centred in a button without fighting the font. */}
          <MoreHorizontal className="chats-meta-more-icon" aria-hidden="true" />
        </DropdownMenuTrigger>
        {/* One thing left in it, and it is the one thing that must not be a menu ITEM: see
            `ArchiveControl`, whose two-click interlock a menu item would collapse. */}
        <DropdownMenuContent align="end" className="chats-meta-menu">
          {/* Settings, not gestures. The model and the effort sit in the box because they are
              changed while writing the message they govern; these three are decided once and left
              alone, so they belong behind the ⋯ rather than in a row you look at all day. */}
          {/* The name was a click on the page's heading; with the header gone it is asked for
              here, beside the conversation's other settings. A dialog for `ChatHelpers`' reason:
              a field inside a menu closes the menu on the first keystroke. */}
          <DropdownMenuItem onSelect={() => setRenaming(true)}>Rename</DropdownMenuItem>
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
          {/* The way back to a terminal, moved in here from the top of the transcript. It is
              a fact about this conversation that never changes and is wanted about twice in
              its life, and it was a line of mono text above every reading of every chat that
              has a folder — which is nearly all of them. Content and not an item: there is
              nothing to select, and a menu item would close the menu on the click that
              selects the command to copy it. */}
          {cwd !== null && project.data?.session != null && (
            <CarryOn cwd={cwd} session={project.data.session} />
          )}
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
      <ChatRename
        chatId={chatId}
        title={row?.title ?? null}
        open={renaming}
        onOpenChange={setRenaming}
      />
      <ChatInstructions
        chatId={chatId}
        open={instructions}
        onOpenChange={setInstructions}
      />
    </>
  );
}

function Project({
  chatId,
  picking,
  onPicking,
}: {
  chatId: string;
  picking: boolean;
  onPicking: (open: boolean) => void;
}) {
  const project = useChatProject(chatId);

  // Nothing at all until it is known. A conversation is not "without a project" because the answer
  // has not arrived yet, and a note that appears and then retracts itself is worse than a late one.
  if (project.data === undefined) return null;
  const { cwd, tools } = project.data;

  return (
    <>
      {/* Only the states that need saying. The settled one — a directory, with tools
          wired — is named by `ChatMeta`'s quiet line instead of by a sentence of its
          own, because "exceptions dominate, the normal disappears" and a conversation
          that is set up correctly is the normal case. */}
      {cwd === null && (
        <NoProject chatId={chatId} picking={picking} onPicking={onPicking} />
      )}
      {cwd !== null && !tools && (
        <ProjectWithoutTools chatId={chatId} cwd={cwd} />
      )}
      {/* `CarryOn` was here, above every transcript. It is behind the `⋯` now — see
          `ChatMeta`. What is left in this block is the two states that need SAYING, which
          is what it was always for. */}
    </>
  );
}

/**
 * The six rungs, ordered from least to most that happens unasked.
 *
 * It used to be "the order the CLI's own picker shows them", and that stopped being the ordering
 * argument the moment a rung appeared that the CLI does not have. `dont_ask` sits BESIDE `auto`
 * rather than above it, and the placement is the claim: it allows precisely what `auto` allows —
 * same classifier, same rules — and refuses the remainder instead of asking about it. Nothing more
 * runs on it than on `auto`, so putting it any higher would read as a wider permission than it is.
 *
 * `needsTools` is which of them a conversation with no tools cannot stand on. `plan` is a flag the
 * CLI is launched with, so it means something whatever the conversation can reach; `auto` is the
 * neutral state and is what every conversation already has. The other four are entirely about what
 * the hook lets through, and a conversation whose turns get no `Bash`, `Read` or `Write` has nothing
 * for them to be about — `dont_ask` included, and it is the one whose flag is easiest to get wrong:
 * it shares `auto`'s permissions but not `auto`'s reason for being reachable, because choosing it is
 * choosing what the hook does with a refusal, and a conversation with no tools never gets there.
 */
const PERMISSION_RUNGS: {
  mode: PermissionMode;
  label: string;
  why: string;
  needsTools: boolean;
}[] = [
  {
    mode: "manual",
    label: "Manual",
    why: "asks before every edit and every command that changes something",
    needsTools: true,
  },
  {
    mode: "accept_edits",
    label: "Edit automatically",
    why: "edits without asking, and asks about everything else",
    needsTools: true,
  },
  {
    mode: "plan",
    label: "Plan",
    why: "answers with a plan — it may still write the plan down",
    needsTools: false,
  },
  {
    mode: "auto",
    label: "Auto",
    why: "runs what the rules recognise; a local judge may answer for the rest",
    needsTools: false,
  },
  {
    mode: "dont_ask",
    label: "Never ask",
    why: "runs what auto runs, and refuses the rest instead of asking",
    needsTools: true,
  },
  {
    mode: "bypass",
    label: "Bypass permissions",
    why: "asks about nothing except a delete inside the project",
    needsTools: true,
  },
];

/**
 * What this conversation may do without being asked.
 *
 * A checkbox stood here and could say one thing — plan, or don't — while the CLI underneath had a
 * whole ladder. This is that ladder, on the conversation rather than per message: somebody says
 * "plan this", reads it, then says "go". Making it per-message would turn one decision into a thing
 * to remember every time.
 *
 * **`plan` is not "changes nothing", and this said so until it was measured.** A planning turn still
 * reaches for tools — `Glob`, `Read`, and a `Write` that RAN, which the daemon's gate saw and the
 * transcript recorded. What it wrote was its own plan, as a document in the working directory; what
 * it did not do was the work. The line says that, because a control promising more than the mode
 * delivers is worse than no control.
 *
 * **The menu opens whatever the conversation can reach.** Greying the whole control would leave
 * somebody looking at a disabled thing with no way to find out why; the rows that cannot be chosen
 * say which of the two reasons applies, and one of them — an unwired hook — is a button away in the
 * panel above.
 */
function PermissionPicker({
  mode,
  onPick,
  tools,
  barred,
  disabled,
}: {
  mode: PermissionMode;
  onPick: (mode: PermissionMode) => void;
  /** Whether this conversation's turns get `Bash`, `Read` and `Write` at all. */
  tools: boolean;
  /** What to say on the rungs `tools` puts out of reach. */
  barred: string;
  disabled: boolean;
}) {
  const shown =
    PERMISSION_RUNGS.find((rung) => rung.mode === mode)?.label ?? "Auto";

  return (
    <DropdownMenu>
      <DropdownMenuTrigger
        className="chats-tool"
        /* Spelled out: the visible text is a mode's NAME, and a control whose whole accessible name
           is "Manual" announces a fact rather than something you can press. */
        aria-label={`Permissions: ${shown} — change what this conversation may do without asking`}
        disabled={disabled}
      >
        {shown}
        <ChevronDown className="chats-tool-caret" aria-hidden="true" />
      </DropdownMenuTrigger>
      <DropdownMenuContent align="start" className="chats-meta-menu">
        <DropdownMenuLabel>What it may do without asking</DropdownMenuLabel>
        <DropdownMenuRadioGroup
          value={mode}
          onValueChange={(picked) => onPick(picked as PermissionMode)}
        >
          {PERMISSION_RUNGS.map((rung) => {
            const unreachable = rung.needsTools && !tools;
            return (
              <DropdownMenuRadioItem
                className="chats-permission-item"
                key={rung.mode}
                value={rung.mode}
                disabled={unreachable}
              >
                <span className="chats-permission-name">{rung.label}</span>
                <span className="chats-permission-why">
                  {unreachable ? barred : rung.why}
                </span>
              </DropdownMenuRadioItem>
            );
          })}
        </DropdownMenuRadioGroup>
      </DropdownMenuContent>
    </DropdownMenu>
  );
}

/**
 * The rung of a conversation that EXISTS, read from the daemon and written back to it.
 *
 * Split from the picker above because the other door has no conversation to read: a session being
 * carried on from the editor is not a chat until its first message is sent, so its rung is local
 * state on the way into `POST /assistant/chats` rather than a PATCH. Same control, two lifetimes,
 * and the split is what stops the front door growing a fake `chatId` to satisfy a hook.
 */
function PermissionMenu({ chatId }: { chatId: string }) {
  const project = useChatProject(chatId);
  const set = useSetPermissionMode(chatId);
  /* Two different absences, and only one of them is actionable — which is why `cwd` travels beside
     `tools` at all. */
  const barred =
    project.data?.cwd == null
      ? "this conversation has no project directory"
      : "the classifier hook is not wired in this project";

  return (
    <PermissionPicker
      mode={project.data?.permission_mode ?? "auto"}
      onPick={(picked) => set.mutate(picked)}
      tools={project.data?.tools ?? false}
      barred={barred}
      disabled={project.data === undefined || set.isPending}
    />
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
 * A conversation with no project yet, which does not start until it has one.
 *
 * A conversation's working directory used to be written once, at creation, out of the editor
 * session it was picked up from — so one started here had none, and `tool_policy_for` answered
 * `McpOnly` for as long as it existed. No Bash, no Read, no Write. The fix used to be a banner with
 * a free-text folder field beside the transcript, which a person could read past and start talking
 * anyway — and the first turn then opened the session every later turn resumes, toolless.
 *
 * Now the choice comes first. The picker (`ProjectPicker`, in the app's own blurred-backdrop
 * `Modal`) opens by itself when the conversation is opened, offers the NucleOS projects plus Root,
 * and anything said meanwhile is held by the composer and sent once the project is set. Dismissing
 * it leaves this one line, which opens it again.
 */
function NoProject({
  chatId,
  picking,
  onPicking,
}: {
  chatId: string;
  picking: boolean;
  onPicking: (open: boolean) => void;
}) {
  const point = useSetChatProject(chatId);
  const held = useHeldMessage(chatId);

  // Once per opening of the conversation: `ChatDetail` is keyed on the chat, so this mount IS the
  // opening. Not on every render, or "Not now" would be answered by the dialog coming straight back.
  useEffect(() => {
    onPicking(true);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  return (
    <div className="chats-project">
      <p className="chats-new-warning" role="status">
        {held === null
          ? "this conversation has no project yet — nothing is sent until one is chosen"
          : "a message is held — it is sent as soon as this conversation has a project"}
      </p>
      <Button type="button" intent="go" onClick={() => onPicking(true)}>
        Choose a project
      </Button>
      <ProjectPicker
        open={picking}
        onOpenChange={onPicking}
        pending={point.isPending}
        holding={held !== null}
        onChoose={(cwd) => {
          if (point.isPending) return;
          point.mutate(cwd, { onSuccess: () => onPicking(false) });
        }}
        refusal={point.isError ? <ProjectRefusal error={point.error} /> : undefined}
      />
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
          "that folder does not exist on this machine — choose another",
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
 * The conversation's name, and the way to change it — a dialog opened from the `⋯`.
 *
 * It was the page's heading, clickable, until the header band went: the tabs strip is the top of
 * the page now and the tab already shows the name, so the rename moved in beside the other things
 * decided about a conversation once and left alone.
 *
 * The draft is seeded when the dialog opens rather than held from the first render. A conversation
 * can be renamed from elsewhere — the daemon names one by itself — and a draft that was set once at
 * mount would quietly write a stale name back over it. Seeded once per opening, guarded by the ref
 * for `ChatInstructions`' reason: the row behind it is polled.
 */
function ChatRename({
  chatId,
  title,
  open,
  onOpenChange,
}: {
  chatId: string;
  title: string | null;
  open: boolean;
  onOpenChange: (open: boolean) => void;
}) {
  const [draft, setDraft] = useState("");
  const patch = usePatchChat();
  const auto = usePostChatTitle();

  const seeded = useRef(false);
  useEffect(() => {
    if (!open) {
      seeded.current = false;
      return;
    }
    if (seeded.current) return;
    seeded.current = true;
    setDraft(title ?? "");
  }, [open, title]);

  const close = () => onOpenChange(false);
  const rename = () => {
    if (draft.trim() === "") return;
    patch.mutate({ chatId, title: draft.trim() }, { onSuccess: close });
  };

  return (
    <Modal
      open={open}
      onOpenChange={onOpenChange}
      title="Rename conversation"
      footer={
        <>
          <Button
            variant="ghost"
            disabled={auto.isPending}
            onClick={() => auto.mutate(chatId, { onSuccess: close })}
          >
            Name it locally
          </Button>
          <Button
            variant="approve"
            disabled={draft.trim() === "" || patch.isPending}
            onClick={rename}
          >
            Rename
          </Button>
        </>
      }
    >
      <div className="chats-helpers">
        <input
          className="chats-title-input"
          aria-label="Conversation title"
          autoFocus
          value={draft}
          onChange={(event) => setDraft(event.target.value)}
          /* Enter commits; Escape is the dialog's own way out. */
          onKeyDown={(event) => {
            if (event.key === "Enter") {
              event.preventDefault();
              rename();
            }
          }}
        />
        {patch.isError && <TitleRefusal error={patch.error} />}
        {auto.isError && <AutoTitleRefusal error={auto.error} />}
      </div>
    </Modal>
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
/**
 * The four answers to "where does this model actually run", in the order the menu offers them.
 *
 * Sections and not one flat list, because the four differ in what they cost and what they risk —
 * somebody else's data centre, a third party's, this disk, and not yet anywhere — and a person
 * scanning a flat list had to read the note at the end of every line to tell which was which.
 *
 * The order is deliberate: the CLI first because it is what almost every conversation runs on, and
 * the models this machine does not have last, because they are the only rows that cannot answer
 * today.
 *
 * `installed !== false` and not `=== true` for the local-and-present test: a daemon older than the
 * field sends no `installed` at all, and every local model it lists is one it found through
 * `/api/tags`. Reading absent as "not downloaded" would move the whole local menu of such a daemon
 * into the section that says it cannot answer.
 */
const MODEL_SECTIONS: {
  key: string;
  label: string;
  holds: (choice: ModelChoice) => boolean;
}[] = [
  {
    key: "cli",
    label: "On the agent CLI",
    holds: (choice) => choice.brain === "cloud",
  },
  {
    key: "hosted",
    label: "Through OpenRouter",
    holds: (choice) => choice.brain === "openrouter",
  },
  {
    key: "installed",
    label: "On this machine",
    holds: (choice) => choice.brain === "local" && choice.installed !== false,
  },
  {
    key: "absent",
    label: "Not downloaded yet",
    holds: (choice) => choice.brain === "local" && choice.installed === false,
  },
];

/**
 * The dead time immediately after a missing model is armed for download.
 *
 * Mirrors `ConfirmButton`'s own `DWELL_MS` and exists for the reason that component gives: a
 * double-click is one gesture, and without this it would both arm the row and confirm it, leaving
 * an interlock that defends against nothing. Duplicated rather than imported because that module
 * keeps it private, and the number is the same 300 ms on purpose — the two are one behaviour.
 */
const ARM_DWELL_MS = 300;

/**
 * A byte count as a person reads it. `5230000000` -> `5.2 GB`.
 *
 * Decimal gigabytes and not binary, because that is the unit the registry, Ollama and every model
 * card are written in — showing 4.9 GiB for a model everyone else calls 5.2 GB would be precisely
 * correct and read as a different model.
 */
function gigabytes(bytes: number): string {
  return `${(bytes / 1_000_000_000).toFixed(1)} GB`;
}

/**
 * One model this machine does not have: what it weighs, whether it fits, and the way to fetch it.
 *
 * Its own component rather than a branch inside the menu's `map`, because the size is a query per
 * model — two absent rows ask two different questions — and a hook cannot live in a loop. The
 * interlock stays in the parent, where it belongs: it is one armed row at a time across the whole
 * menu, not a fact about any single row.
 */
function MissingModelRow({
  choice,
  pull,
  refused,
  armed,
  busy,
  onSelect,
}: {
  choice: ModelChoice;
  pull: LocalPull | null;
  refused: boolean;
  armed: boolean;
  busy: boolean;
  onSelect: () => void;
}) {
  const size = useLocalSize(choice.id);
  const running = pull !== null && pull.state === "running";
  const fit = size.data?.fit ?? "unknown";
  const tooBig = fit === "too_big";
  const bytes = size.data?.bytes ?? null;

  /* What the row says when nothing is happening to it. The size leads, because it is the number
     the decision turns on and the one nothing else in this window can tell somebody.

     `too_big` names BOTH sides. "42.5 GB" alone says nothing about whether that is a lot; "42.5 GB
     — this machine has 15.8 GB" is a sentence somebody can act on, by picking a smaller
     quantisation or by buying memory. A bare refusal is a wall with no door in it.

     A model that fits gets no verdict at all, only its size. Writing "this will work" under every
     row that works is noise a person learns to skip, and skipping it is how they miss the one row
     that says something else. */
  const weight =
    bytes === null
      ? null
      : tooBig && size.data?.memory
        ? `${gigabytes(bytes)} — this machine has ${gigabytes(size.data.memory)}`
        : fit === "tight"
          ? `${gigabytes(bytes)} — fits, with little room left`
          : gigabytes(bytes);

  return (
    <DropdownMenuItem
      key={choice.id}
      /* A model that cannot run is closed rather than armed. Arming it would offer a second click
         that does nothing, which is a worse answer than a row that says why it is not offering. */
      disabled={running || busy || tooBig}
      /* Radix closes the menu on select; both halves of the interlock happen in here, and the
         download is watched from this same menu, so the default is fought exactly as
         `ExtraDirsMenu` below fights it. */
      onSelect={(event) => {
        event.preventDefault();
        onSelect();
      }}
    >
      {choice.label}
      <span className="chats-tool-why">
        {running
          ? /* The percentage when there is one, and Ollama's own word for what it is doing when
               there is not — every download opens on a frame that has not measured itself yet, and
               a bar at 0% would say it is stuck. */
            (pull.percent !== null ? `${pull.percent}%` : pull.status)
          : pull !== null && pull.state === "failed"
            ? /* What Ollama said, not a word of this window's own: the person is being told why a
                 download they asked for did not happen, and Ollama is the only party here that
                 knows. */
              (pull.error ?? "the download failed")
            : refused
              ? "the núcleo would not start the download"
              : armed
                ? /* Still leads with the weight, because the second click is the one that spends
                     it and this is the last moment it can be reconsidered. */
                  (weight === null
                    ? "click again to download it"
                    : `${weight} — click again to download`)
                : /* `unknown` degrades to exactly what this row said before it could ask: the
                     registry is a third party on the far side of the internet, and a window that
                     turned "could not ask" into a verdict would refuse a working model every time
                     the network hiccuped. */
                  (weight ?? "not downloaded")}
      </span>
    </DropdownMenuItem>
  );
}

/** The companies the model menu opens on, in order, and the mark each is drawn with. */
const COMPANIES: { provider: string; label: string; mark: string }[] = [
  { provider: "anthropic", label: "Claude", mark: "claude" },
  { provider: "openai", label: "GPT", mark: "codex" },
  { provider: "other", label: "Other", mark: "other" },
];

/** The daemon's provider word for a model id: from its group when listed, else from the id. */
function providerOf(id: string, groups: ModelGroup[] | undefined): string {
  const listed = groups?.find((group) => group.models.some((choice) => choice.id === id));
  if (listed) return listed.provider;
  const lower = id.toLowerCase();
  if (lower.startsWith("gpt") || /^o\d/.test(lower)) return "openai";
  if (lower.startsWith("claude") || ["opus", "sonnet", "haiku", "fable"].includes(lower))
    return "anthropic";
  return "other";
}

/** The mark drawn beside a model's name, or none for a provider without one. */
function markOf(provider: string): string | null {
  return provider === "other"
    ? null
    : (COMPANIES.find((company) => company.provider === provider)?.mark ?? null);
}

function ModelMenu({
  model,
  onPick,
  disabled = false,
  children,
  chatId,
}: {
  model: string | null;
  onPick: (model: string | null) => void;
  disabled?: boolean;
  children?: React.ReactNode;
  chatId?: string;
}) {
  const catalogue = useAssistantModels(chatId);
  // The agent-CLI section is drawn from the daemon's discovered, grouped list when it has one.
  const grouped = useModelGroups(chatId).data;
  const groups = grouped?.groups;
  const needsRoot = new Set(grouped?.needs_root ?? []);
  const localModel = useLocalModel();
  const localUnavailable = localModel.data?.available === false;
  const pull = useLocalPull();
  const startPull = usePullLocalModel();
  /* Which missing model is one click from being fetched. The `ConfirmButton` interlock, kept
     rather than borrowed: that component renders a `Button`, and a button inside a menu is not
     reachable by the menu's own arrow-key navigation, so importing it here would have cost the
     keyboard the row entirely. What is kept is the part that matters — arm, then confirm, in
     place, no dialog.

     Neither of that component's two timers is kept, and for reasons rather than by omission. Its
     4-second disarm defends a control that stays on screen after you look away; a menu closes the
     moment you look away, and closing resets this. Its 300 ms dwell IS kept, as `armedAt` below,
     because a double-click is one gesture and without it a double-click would arm and confirm in
     one motion — which is the whole interlock, defeated by a reflex. */
  const [armed, setArmed] = useState<string | null>(null);
  const armedAt = useRef(0);

  const choices = catalogue.data?.choices ?? [];
  const chosen =
    choices.find((choice) => choice.id === model) ??
    groups?.flatMap((group) => group.models).find((choice) => choice.id === model);
  // Unpinned shows the configured model's name, not a blank. A pin the daemon never labelled (a
  // name since removed from the config) is shown as a product name rather than as the raw id: it
  // is still what the next turn will actually run.
  // "Model" and not "model": the last fallback is this app's own word for the setting —
  // the ones before it are proper names — and a setting named in lower case beside a
  // caret read as terminal output rather than as a control.
  const configured = catalogue.data?.configured;
  const shown =
    chosen?.label ??
    (model ? displayName(model) : null) ??
    catalogue.data?.configured_label ??
    (configured ? displayName(configured) : null) ??
    "Model";
  const pick = (picked: string) => onPick(picked === "" ? null : picked);
  const configuredProvider = configured ? providerOf(configured, groups) : null;
  const shownProvider = markOf(providerOf(model ?? configured ?? "", groups));
  const companies = COMPANIES.map((company) => ({
    ...company,
    groups: (groups ?? []).filter(
      (group) => group.provider === company.provider && group.models.length > 0,
    ),
  })).filter(
    (company) => company.groups.length > 0 || company.provider === configuredProvider,
  );

  return (
    <DropdownMenu
      /* Closing is what disarms. A menu you walked away from is one you closed, which is why this
         needs none of `ConfirmButton`'s timer — see the comment on `armed` above. */
      onOpenChange={(open) => {
        if (!open) setArmed(null);
      }}
    >
      <DropdownMenuTrigger
        className="chats-tool"
        /* Spelled out: the visible text is a model's NAME, and a control whose whole accessible
           name is "Sonnet 5" announces a fact rather than something you can press. */
        aria-label={`Answered by ${shown} — change the model`}
        disabled={disabled}
      >
        {shownProvider !== null && <ProviderMark provider={shownProvider} size={14} />}
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
        {/* One row per company, and its models only on hover. The vendor is the first thing a
            person decides, and a menu that opened on every family of every vendor was a wall of
            names before anybody had chosen whose. Families stay inside a company's one list,
            split by a rule, rather than as headings of their own. */}
        {companies.map((company) => {
          const defaultHere =
            catalogue.data !== undefined && company.provider === configuredProvider;
          return (
            <DropdownMenuSub key={company.provider}>
              <DropdownMenuSubTrigger className="chats-model-company">
                <ProviderMark provider={company.mark} size={14} />
                {company.label}
              </DropdownMenuSubTrigger>
              <DropdownMenuSubContent className="chats-meta-menu chats-model-list">
                <DropdownMenuRadioGroup value={model ?? ""} onValueChange={pick}>
                  {/* The unpinned state, under the company its configured model belongs to. */}
                  {defaultHere && (
                    <DropdownMenuRadioItem value="">
                      Default
                      <span className="chats-tool-why">
                        {catalogue.data?.configured_label ?? catalogue.data?.configured}
                      </span>
                    </DropdownMenuRadioItem>
                  )}
                  {company.groups.map((group, index) => (
                    <DropdownMenuGroup
                      key={`${group.provider}/${group.family}`}
                      aria-label={group.label}
                    >
                      {(index > 0 || defaultHere) && <DropdownMenuSeparator />}
                      {group.models.map((choice) => {
                        const closed = needsRoot.has(choice.id);
                        return (
                          <DropdownMenuRadioItem
                            key={choice.id}
                            value={choice.id}
                            disabled={closed}
                          >
                            {choice.label}
                            {closed && (
                              /* Listed and closed rather than left out: Codex answers only in a
                                 conversation rooted in a project with the classifier hook wired,
                                 and a vendor missing from the menu reads as a missing feature. */
                              <span className="chats-tool-why">
                                needs a project with the hook wired
                              </span>
                            )}
                          </DropdownMenuRadioItem>
                        );
                      })}
                    </DropdownMenuGroup>
                  ))}
                </DropdownMenuRadioGroup>
              </DropdownMenuSubContent>
            </DropdownMenuSub>
          );
        })}
          {MODEL_SECTIONS.map((section) => {
            // Replaced by the companies above once the daemon has sent its groups.
            if (section.key === "cli" && groups !== undefined) return null;
            const held = choices.filter(section.holds);
            /* An empty section is not drawn at all rather than drawn empty: three of the four are
               empty on an untouched install, and headings over nothing would make the menu look
               like it had lost its contents. */
            if (held.length === 0) return null;
            return (
              <DropdownMenuSub key={section.key}>
                <DropdownMenuSubTrigger className="chats-model-company">
                  {section.label}
                </DropdownMenuSubTrigger>
                <DropdownMenuSubContent className="chats-meta-menu chats-model-list">
                <DropdownMenuRadioGroup value={model ?? ""} onValueChange={pick}>
                {held.map((choice) => {
                  /* A model this machine does not have is not a choice of who answers — it is an
                     action, and it is drawn as one. A `DropdownMenuItem` inside the radio group
                     rather than beside it: the sections above and below it are the same menu and
                     the same list of models, and moving one section out would put the row that
                     says "not downloaded yet" somewhere other than under its own heading. */
                  if (choice.installed === false) {
                    const mine =
                      pull.data && pull.data.model === choice.id ? pull.data : null;
                    /* A refusal from the daemon — a name its catalogue no longer carries, or
                       another download already running — belongs on the row that asked for it and
                       nowhere else. Without this the row would simply go back to saying "not
                       downloaded", which is the silent death this whole menu exists to stop. */
                    const refused =
                      startPull.isError && startPull.variables === choice.id;
                    return (
                      <MissingModelRow
                        key={choice.id}
                        choice={choice}
                        pull={mine}
                        refused={refused}
                        armed={armed === choice.id}
                        busy={startPull.isPending}
                        onSelect={() => {
                          if (armed !== choice.id) {
                            setArmed(choice.id);
                            armedAt.current = Date.now();
                            return;
                          }
                          /* The second half of a double-click lands here within the dwell and is
                             IGNORED — it does not confirm, and it does not disarm either, because
                             disarming would punish the reflex and make the row feel broken. */
                          if (Date.now() - armedAt.current < ARM_DWELL_MS) return;
                          setArmed(null);
                          startPull.mutate(choice.id);
                        }}
                      />
                    );
                  }

                  /* Two facts about a choice, read as one verdict on picking it: where the words
                     go, and whether the model can work the tools a turn reaches for. Composed into
                     one line rather than stacked as two spans, because the row is one line tall. */
                  const why = [
                    choice.brain === "local"
                      ? localUnavailable
                        ? "not running on this machine"
                        : "on this machine"
                      : choice.brain === "openrouter"
                        ? /* Said before the pick, not after: the local mark answers "is it running
                             here", and this one answers the question that matters just as much for
                             a hosted choice — whose machine the words end up on. */
                          "off this machine — sent to a third-party provider"
                        : null,
                    /* Only `false` earns a mark. Absent is "nobody asked", which is the ordinary
                       state of every choice nothing has introspected — marking that would put a
                       warning on most of the menu and teach people to read past it. */
                    choice.tools === false ? "cannot use tools" : null,
                  ].filter((note): note is string => note !== null);

                  return (
                    <DropdownMenuRadioItem
                      key={choice.id}
                      value={choice.id}
                      /* The local route being down is the one reason left that a row here cannot be
                         picked: a model that is not on the disk at all no longer reaches this
                         branch — it is the action above. This one is not answerable from the menu,
                         because what fixes it is starting Ollama and restarting the daemon. */
                      disabled={choice.brain === "local" && localUnavailable}
                    >
                      {choice.label}
                      {why.length > 0 && (
                        <span className="chats-tool-why">
                          {why.join(" · ")}
                        </span>
                      )}
                    </DropdownMenuRadioItem>
                  );
                })}
                </DropdownMenuRadioGroup>
                </DropdownMenuSubContent>
              </DropdownMenuSub>
            );
          })}
        {children}
      </DropdownMenuContent>
    </DropdownMenu>
  );
}

/**
 * How hard the model is asked to think, as a slider over the chosen model's own levels.
 *
 * Position 0 is "default" and writes `null`; the rest are the CLI's own words for the levels.
 *
 * The levels are the CHOSEN model's own, which is why this reads the catalogue instead of taking a
 * list: they genuinely differ. `claude-haiku-4-5` takes no effort at all; Opus 4.6 takes `max` but
 * not `xhigh`. A shared list would offer levels that come back as an error. A model with no levels
 * gets no slider at all, since a dial that turns nothing is only noise in a row that is already
 * crowded; the discovered groups are asked first, then the config's own list.
 */
function EffortSlider({
  model,
  effort,
  onPick,
  disabled = false,
  chatId,
}: {
  model: string | null;
  effort: string | null;
  onPick: (effort: string | null) => void;
  disabled?: boolean;
  chatId?: string;
}) {
  const catalogue = useAssistantModels(chatId);
  const groups = useModelGroups(chatId).data?.groups;
  const fromGroups = groups
    ?.flatMap((group) => group.models)
    .find((choice) => choice.id === model)?.efforts;
  const chosen = catalogue.data?.choices.find((choice) => choice.id === model);
  // The union stands in only while nothing is pinned — the front door, where the model question is
  // still open.
  const levels = fromGroups ?? chosen?.efforts ?? catalogue.data?.efforts ?? [];
  if (levels.length === 0) return null;
  const at = effort === null ? 0 : Math.max(0, levels.indexOf(effort) + 1);

  /* A menu holding the slider, not the slider itself in the row: the row is read all day and the
     effort is changed now and then, so the row carries only its current value. */
  return (
    <DropdownMenu>
      <DropdownMenuTrigger
        className="chats-tool"
        aria-label={`Effort ${effort ?? "default"} — change the effort`}
        disabled={disabled}
      >
        <span className="chats-effort-name">Effort</span>
        {effort ?? "default"}
        <ChevronDown className="chats-tool-caret" aria-hidden="true" />
      </DropdownMenuTrigger>
      <DropdownMenuContent align="start" className="chats-meta-menu chats-effort-menu">
        <DropdownMenuLabel>How hard it thinks</DropdownMenuLabel>
        {/* The menu's own arrow-key navigation would take the keys the slider moves on. */}
        <div className="chats-effort" onKeyDown={(event) => event.stopPropagation()}>
          <Slider
            steps={["default", ...levels]}
            value={at}
            label="Effort level"
            disabled={disabled}
            onChange={(index) => onPick(index === 0 ? null : levels[index - 1])}
          />
        </div>
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
        chatId={chatId}
        onPick={(picked) => patch.mutate({ chatId, model: picked })}
      >
        {patch.isError && <ModelRefusal error={patch.error} />}
      </ModelMenu>
      <EffortSlider
        model={model}
        effort={effort}
        disabled={patch.isPending}
        chatId={chatId}
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
    <Modal
      open={open}
      onOpenChange={onOpenChange}
      size="md"
      title="Standing instructions"
      description="Added to what this conversation's model is already told — on every turn, not just the first. Nothing here replaces the model's own instructions."
      footer={
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
      }
    >
      <div className="chats-helpers">
        <label className="chats-helper-field">
          <span>Instructions</span>
          <textarea
            rows={8}
            value={draft}
            onChange={(event) => setDraft(event.target.value)}
            placeholder="Answer in European Portuguese. Prefer the smallest correct change."
          />
        </label>
        {patch.isError && <HelperRefusal error={patch.error} />}
      </div>
    </Modal>
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
        variant="ghost"
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
type HelperDraft = Subagent & {
  model: string | null;
  effort: string | null;
  tools: string[] | null;
};

/** What a fresh row starts as. Named so "add" and "reset" cannot drift apart. No restriction —
 *  the helper inherits the parent's whole surface, exactly as it did before this field existed. */
function blankHelper(): HelperDraft {
  return {
    name: "",
    description: "",
    prompt: "",
    model: null,
    effort: null,
    tools: null,
  };
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
 *
 * `knownTools` is threaded in rather than read from `useDeniableTools` here — this function is not
 * a component, and a hook called from one would break the rules of hooks the moment two helpers
 * needed an answer in the same render.
 */
function whyHelperIsRefused(
  helper: HelperDraft,
  others: HelperDraft[],
  knownTools: string[],
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
  // Checked only once the served list has actually arrived — before that every name would read as
  // unknown, and a helper opened with tools already set would flash a refusal it does not deserve.
  // One membership check catches both a typo and a pattern like `Bash(git *)`: neither is ever a
  // name the door serves, so neither is ever a name this list contains.
  if (helper.tools !== null && knownTools.length > 0) {
    const unknown = helper.tools.find((tool) => !knownTools.includes(tool));
    if (unknown !== undefined)
      return `"${unknown}" is not a tool the daemon can grant — a pattern is refused the same as a name it does not know`;
  }
  return null;
}

/**
 * Which tools this helper may call, or "no restriction — inherits" for what it does today.
 *
 * A separate control from `ChatDenials` rather than a shared one: that one takes something away
 * from the whole conversation, this one GRANTS a subset to one helper, and null here means the
 * opposite of empty there — absent is "everything", not "nothing". Checkboxes over the served list
 * for the same reason `ChatDenials` uses them: a typed rule that matches no tool is a restriction
 * somebody set and nobody applied, reported nowhere this app's user would read it.
 *
 * The top row is its own checkbox rather than a toggle beside the list, so the default state — no
 * restriction — reads as a state you can SEE is selected, not as an absence of the others.
 */
function HelperToolsControl({
  tools,
  knownTools,
  onChange,
}: {
  tools: string[] | null;
  knownTools: string[];
  onChange: (tools: string[] | null) => void;
}) {
  const toggle = (name: string, on: boolean) => {
    const base = tools ?? [];
    onChange(on ? [...base, name] : base.filter((tool) => tool !== name));
  };

  const label =
    tools === null
      ? "inherits everything"
      : tools.length === 0
        ? "nothing granted"
        : `${tools.length} granted`;

  return (
    <div className="chats-helper-field">
      <span>Tools</span>
      <DropdownMenu>
        <DropdownMenuTrigger
          type="button"
          disabled={knownTools.length === 0}
          className="chats-helper-tools-trigger"
        >
          {label}
        </DropdownMenuTrigger>
        <DropdownMenuContent align="start" className="chats-meta-menu chats-denials">
          <DropdownMenuLabel>Tools this helper may call</DropdownMenuLabel>
          <DropdownMenuCheckboxItem
            checked={tools === null}
            onSelect={(event) => event.preventDefault()}
            onCheckedChange={(on) => onChange(on === true ? null : [])}
          >
            no restriction — inherits the conversation
          </DropdownMenuCheckboxItem>
          <DropdownMenuSeparator />
          {knownTools.map((name) => (
            <DropdownMenuCheckboxItem
              key={name}
              checked={tools !== null && tools.includes(name)}
              disabled={tools === null}
              /* Several are usually granted together, so the menu is kept open. */
              onSelect={(event) => event.preventDefault()}
              onCheckedChange={(on) => toggle(name, on === true)}
            >
              {name}
            </DropdownMenuCheckboxItem>
          ))}
        </DropdownMenuContent>
      </DropdownMenu>
    </div>
  );
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
  const deniable = useDeniableTools();
  const knownTools = deniable.data?.tools ?? [];
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
        tools: agent.tools ?? null,
      })),
    );
  }, [open, saved]);

  const choices = (catalogue.data?.choices ?? []).filter(
    (choice) => choice.brain === "cloud",
  );
  const refusals = draft.map((helper) =>
    whyHelperIsRefused(helper, draft, knownTools),
  );
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
        // Spread FIRST, then trim the three that need it. Naming every field by hand builds a NEW
        // object that happens to agree with the draft, and it agrees only for the fields that
        // existed the day it was written: `HelperDraft` is `Subagent` and nothing else, so any
        // property the draft holds belongs on the wire, and a new OPTIONAL field on `Subagent`
        // would type-check its way through a hand-written list while never being sent.
        agents: draft.map((helper) => ({
          ...helper,
          name: helper.name.trim(),
          description: helper.description.trim(),
          prompt: helper.prompt.trim(),
        })),
      },
      { onSuccess: () => onOpenChange(false) },
    );
  };

  return (
    <Modal
      open={open}
      onOpenChange={onOpenChange}
      size="md"
      title="Helpers"
      description="Work this conversation can hand off. These are added to any the project already defines — they never hide them."
      footer={
        <>
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
        </>
      }
    >
      <div className="chats-helpers">
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
                <HelperToolsControl
                  tools={helper.tools}
                  knownTools={knownTools}
                  onChange={(tools) => change(index, { tools })}
                />
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

        {patch.isError && <HelperRefusal error={patch.error} />}
      </div>
    </Modal>
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
        variant="quiet"
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
  already,
}: {
  view: ReturnType<typeof useIdeConversation>;
  handed: Exchange[];
  /**
   * Whether this conversation has been brought here yet.
   *
   * The lines above are identical either way — that is the point, and it is what makes the
   * moment of picking one up invisible. What differs is the seam UNDER them: before, it is the
   * place the conversation carries on from; after, it is a record of where it did.
   */
  already: boolean;
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
        {already
          ? "this was picked up from a conversation in the editor that nobody spoke in."
          : "nobody spoke in this one — saying something here starts it."}
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
              <span
                className={
                  said.by_owner
                    ? "chats-said-who chats-said-who-you"
                    : "chats-said-who"
                }
              >
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
      {/* The seam, and it says the same true thing from either side: everything above happened
          somewhere else and cost nothing here. Only the tense changes. */}
      <p className="chats-picked-up-cut">
        {already
          ? "picked up here — everything above was said in the editor and read back out of its own file. None of it was a run, and none of it was billed here."
          : "everything above was said in the editor, and reading it here cost nothing. Say something and it carries on from this line."}
      </p>
      {/* Only once it HAS continued. Before that there is nothing to disclose about how it did,
          and `WhatItCarries` above has already said what continuing would carry. */}
      {already && (
        <HowItContinued
          handed={handed}
          carries={view.data.context_estimate}
          largestWindow={view.data.largest_window}
        />
      )}
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
  largestWindow,
}: {
  handed: Exchange[];
  carries: number | null;
  largestWindow: number;
}) {
  const [open, setOpen] = useState(false);

  if (handed.length === 0) {
    if (carries === null || carries > largestWindow) return null;
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
        this session was larger than any window a model has, so there was
        nothing to resume it into. The model was handed the last{" "}
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
 * **Beside the folder, not under the transcript.** It is a fact about the project, and the
 * project's folder is already on the tabs row, so the question sits next to the thing it is about,
 * as a chip, rather than as a line of underlined text at the foot of a conversation where it read
 * like a footnote to the last answer. It opens a panel over the page and not a modal: this is a
 * place to look while the conversation stays where it is.
 *
 * **Files first, lines second.** What a person scans for is WHICH files and how much; the panel
 * leads with a count per kind of change and a row per file, and a file's lines open under its row.
 *
 * **It says what it is, and what it is not.** The daemon takes no snapshot before a turn, so this is
 * what is different NOW — the same thing after one turn, and not after three. Labelling it as what
 * the turn did would be the kind of note that reads like a fact and stops being one.
 *
 * Closed until asked. Opening it walks a working tree, and a chip that did that on arrival would do
 * it for every conversation somebody clicked past — so the chip says "Changes" until it has been
 * opened once, and then carries the counts it last read.
 */
function ProjectChanges({ chatId }: { chatId: string }) {
  const [open, setOpen] = useState(false);
  const diff = useChatDiff(chatId, open);
  const files = diff.data === undefined ? null : diffFiles(diff.data);
  const added = files?.reduce((sum, file) => sum + file.added, 0) ?? 0;
  const removed = files?.reduce((sum, file) => sum + file.removed, 0) ?? 0;

  const named =
    files === null
      ? "What is different in this project"
      : files.length === 0
        ? "What is different in this project, nothing changed"
        : `What is different in this project, ${plural(files.length, "file")} changed`;

  return (
    <PopoverPrimitive.Root open={open} onOpenChange={setOpen}>
      <PopoverPrimitive.Trigger className="chats-changes-chip" aria-label={named} title={named}>
        <GitCompareArrows className="chats-changes-glyph" strokeWidth={1.5} aria-hidden="true" />
        {files === null && <span>Changes</span>}
        {files !== null && files.length === 0 && <span>No changes</span>}
        {files !== null && files.length > 0 && (
          <>
            <span>{plural(files.length, "file")}</span>
            <LineCounts added={added} removed={removed} />
          </>
        )}
      </PopoverPrimitive.Trigger>
      <PopoverPrimitive.Portal>
        <PopoverPrimitive.Content
          className="chats-changes"
          aria-label="What is different"
          align="end"
          sideOffset={6}
          collisionPadding={12}
        >
          <div className="chats-changes-head">
            <p className="chats-changes-title">What is different in this project</p>
            <p className="chats-changes-note">
              The working tree against its last commit, as it is now — not only what this
              conversation did.
            </p>
          </div>
          {diff.isError && <ChangedRefusal error={diff.error} />}
          {files === null && !diff.isError && (
            <p className="chats-loading">reading the project…</p>
          )}
          {files !== null && files.length === 0 && (
            <p className="chats-changes-clean">nothing in this project has changed</p>
          )}
          {files !== null && files.length > 0 && (
            <ChangedFiles files={files} added={added} removed={removed} />
          )}
        </PopoverPrimitive.Content>
      </PopoverPrimitive.Portal>
    </PopoverPrimitive.Root>
  );
}

const CHANGE_GLYPH: Record<FileChange, typeof FilePen> = {
  modified: FilePen,
  added: FilePlus,
  deleted: FileMinus,
  renamed: FileSymlink,
};

/** The order the kinds are counted in: the common case first. */
const CHANGE_ORDER: FileChange[] = ["modified", "added", "deleted", "renamed"];

/**
 * `+12 −4`: the sign carries the meaning and the tone only reinforces it. A side with nothing on it
 * is left out — `−0` is a number to read that says nothing — and a file with no lines either way
 * (a binary, a bare rename) draws no count at all.
 */
function LineCounts({ added, removed }: { added: number; removed: number }) {
  if (added === 0 && removed === 0) return null;
  return (
    <span className="chats-changes-counts">
      {added > 0 && <span className="chats-changes-added">+{added}</span>}
      {removed > 0 && <span className="chats-changes-removed">−{removed}</span>}
    </span>
  );
}

/**
 * A file's slice of the diff from its first hunk on. The row above it already names the file and
 * what happened to it, so git's header lines would only say it a second time; a slice with no
 * hunk at all (a binary, a bare rename) keeps them, because then they are all there is.
 */
function fromFirstHunk(text: string): string {
  const at = text.search(/^@@ /m);
  return at < 0 ? text : text.slice(at);
}

/**
 * A count per kind of change, then a row per file whose lines open beneath it.
 *
 * A handful of files open together, because then the whole diff is the quickest read; past three
 * they start closed, and the rows are the overview.
 */
function ChangedFiles({
  files,
  added,
  removed,
}: {
  files: DiffFile[];
  added: number;
  removed: number;
}) {
  const [shown, setShown] = useState<ReadonlySet<number>>(
    () => new Set(files.length <= 3 ? files.map((_, at) => at) : []),
  );
  const toggle = (at: number) =>
    setShown((was) => {
      const next = new Set(was);
      if (next.has(at)) next.delete(at);
      else next.add(at);
      return next;
    });

  return (
    <div className="chats-changes-body">
      <div className="chats-changes-summary">
        <ul className="chats-changes-kinds" aria-label="Kinds of change">
          {CHANGE_ORDER.map((change) => {
            const count = files.filter((file) => file.change === change).length;
            if (count === 0) return null;
            const Glyph = CHANGE_GLYPH[change];
            return (
              <li key={change} className="chats-changes-kind">
                <Glyph className="chats-changes-glyph" strokeWidth={1.5} aria-hidden="true" />
                {count} {change}
              </li>
            );
          })}
        </ul>
        <LineCounts added={added} removed={removed} />
      </div>
      <ul className="chats-changes-files" aria-label="Changed files">
        {files.map((file, at) => {
          const Glyph = CHANGE_GLYPH[file.change];
          const cut = file.path.lastIndexOf("/");
          const open = shown.has(at);
          return (
            // Keyed by position: one reading of one diff, drawn whole and never reordered.
            <li key={`file-${at}`} className="chats-changes-file">
              <button
                type="button"
                className="chats-changes-row"
                aria-expanded={open}
                onClick={() => toggle(at)}
              >
                <ChevronRight
                  className="chats-changes-chevron"
                  strokeWidth={1.5}
                  aria-hidden="true"
                />
                <Glyph
                  className="chats-changes-glyph"
                  strokeWidth={1.5}
                  aria-hidden="true"
                />
                <span className="sr-only">{file.change}</span>
                <span className="chats-changes-path" title={file.path}>
                  {cut >= 0 && (
                    <span className="chats-changes-dir">{file.path.slice(0, cut + 1)}</span>
                  )}
                  <span className="chats-changes-name">{file.path.slice(cut + 1)}</span>
                </span>
                <LineCounts added={file.added} removed={file.removed} />
              </button>
              {open && <Diff text={fromFirstHunk(file.text)} label={`Changes to ${file.path}`} />}
            </li>
          );
        })}
      </ul>
    </div>
  );
}

function plural(count: number, noun: string): string {
  return `${count} ${noun}${count === 1 ? "" : "s"}`;
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
 *
 * **On `auto` a question can now answer itself, and it is drawn anyway.** A local judge races the
 * person inside the same window, so a question the judge approves appears and disappears in a couple
 * of seconds. Holding the render back until the judge has had its say would hide that — and an
 * automatic approval nobody ever sees is exactly the cost this feature is supposed to keep visible.
 * The flicker is the judge's work, shown.
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
  notices,
  precededBy,
  chatId,
  more,
  find,
  follows,
  keepUp,
  onReuse,
}: {
  turns: Turn[];
  notices: ChatNotice[];
  precededBy: boolean;
  chatId: string;
  /** Whether there are turns older than the first one here. See `Transcript.more`. */
  more: boolean;
  /** A turn to scroll to, from a search. See `found` in `Chats`. */
  find: { turnId: number; at: number } | null;
  /** Whether the box this is drawn in still follows its end. See `useFollowsItsEnd`. */
  follows: { current: boolean };
  /** Takes that box to its end, if it still follows it. Handed to the turns through `KeepsUp`. */
  keepUp: () => void;
  /** Puts a question that was already asked back in the box. See `TurnBlock`. */
  onReuse: (text: string) => void;
}) {
  const older = useOlderTurns(chatId);
  // Which turn is lit, and the stamp of the jump that lit it. The ref, because a jump can be
  // asked for before the conversation it belongs to has finished loading: the effect gives up
  // and the next render tries again, and this is what stops it doing the whole thing twice.
  const [lit, setLit] = useState<number | null>(null);
  const jumped = useRef<number | null>(null);
  // Where this conversation was told to forget everything before, so the mark lands on the right
  // turn. Read here rather than inside each block: it is one fact about the whole transcript.
  const clearedAfter = useChatRow(chatId)?.cleared_after_run_id ?? null;

  /**
   * The turn somebody searched for, brought into view and lit for a moment.
   *
   * By `id` on the element rather than through a map of refs: the id is already how the turn is
   * addressed — one conversation is open, so `turn-7` is unique on the page — and a ref map would
   * be a second index over the same list, kept in step by hand.
   *
   * A turn that is not on the page is not an error. A search can land on something older than the
   * hundred turns the transcript holds, and what happens then is that the conversation opens at
   * its recent end, with "earlier turns" above it. Nothing is claimed that is not true.
   */
  useEffect(() => {
    if (find === null || jumped.current === find.at) return;
    const at = document.getElementById(`turn-${find.turnId}`);
    if (at === null) return;
    jumped.current = find.at;
    // Said here rather than left to the scroll event this is about to cause, because the effect
    // that keeps the box at its end belongs to the PARENT and parent effects run after a child's.
    // Waiting for the event would mean the end-scroll ran first and threw the jump away.
    //
    // It also settles something the old guard only postponed: the end-scroll used to resume when
    // the highlight faded, so searching for a turn took you to it, gave you two and a half
    // seconds with it, and then dropped you at the bottom of the conversation.
    follows.current = false;
    at.scrollIntoView({ block: "center" });
    setLit(find.turnId);
    const dim = setTimeout(() => setLit(null), 2500);
    return () => clearTimeout(dim);
  }, [find, turns.length, follows]);

  // "nothing has been said yet" is a claim about the whole conversation, and a picked-up
  // one is full of what was said in the editor. Saying it over that is the wrong answer.
  //
  // A conversation holding only departmental reports is NOT empty, which is why `notices` counts
  // here. It is the shape of a conversation somebody opened, set a department going from, and has
  // not typed in since — and telling them nothing has been said over a screen of what a department
  // told them would be the window contradicting itself.
  if (turns.length === 0 && notices.length === 0 && precededBy) return null;
  if (turns.length === 0 && notices.length === 0)
    /* The app's one-line absence, said in the app's own component. `.chats-loading` beside it in
       the stylesheet stayed hand-rolled on purpose: a "reading…" line is a wait, not an absence,
       and `Quiet` is the muted rung because there the sentence is the content. */
    return <Quiet says="nothing has been said yet." />;
  return (
    /* Around the turns and not around the whole door, because this is the only subtree that draws
       one: a turn in flight is the one thing here that grows the page on a poll of its own. */
    <KeepsUp.Provider value={keepUp}>
      {/* Above the oldest turn on the page, because that is where the rest of the conversation
          is. It used to simply not be there: a hundred turns came back, the hundred-and-first was
          dropped in silence, and nothing distinguished a conversation that began where you were
          looking from one whose first afternoon had been cut off the top. */}
      {more && (
        <div className="chats-earlier">
          <Button
            variant="ghost"
            disabled={older.isPending}
            onClick={() => older.mutate()}
          >
            {older.isPending ? "reading…" : "Earlier turns"}
          </Button>
          {older.isError && (
            <ErrorNote>the earlier turns could not be read</ErrorNote>
          )}
        </div>
      )}
      <ul className="chats-turns" aria-label="Transcript">
        {interleave(turns, notices).map((entry, index, all) =>
          entry.kind === "notice" ? (
            <DepartmentSaid key={`notice-${entry.notice.id}`} notice={entry.notice} />
          ) : (
            <TurnBlock
              key={entry.turn.id}
              turn={entry.turn}
              previous={previousTurn(all, index)}
              clearedAfter={clearedAfter}
              chatId={chatId}
              onReuse={onReuse}
              /* The last one, and only the last one. A CSS animation plays when its element
                 mounts, so marking every turn would fade a forty-turn transcript in as a wall on
                 open; marking the last one means it plays once, on the turn that just arrived,
                 and the ones above it are already there.

                 Compared against the last TURN rather than against the last ENTRY, which is what
                 the interleaving changed: a department that spoke after the final turn would
                 otherwise take the animation off the turn that actually arrived and give it to
                 nothing, since a notice does not draw one. */
              arriving={entry.turn.id === turns[turns.length - 1]?.id}
              lit={lit === entry.turn.id}
            />
          ),
        )}
      </ul>
    </KeepsUp.Provider>
  );
}

/** One thing on the transcript: a turn, or a department speaking. */
type Entry = { kind: "turn"; turn: Turn; at: string } | { kind: "notice"; notice: ChatNotice; at: string };

/**
 * PURE: the two lists in one, oldest first.
 *
 * By `created_at` and not by id, because the two come from different tables with independent
 * sequences — notice 1 and turn 900 say nothing about which happened first. Ties break toward the
 * TURN, so a department reporting in the same second a turn landed reads as a remark on it rather
 * than as something the turn was answering; nothing was answering it either way, and one of the two
 * orders is less misleading.
 *
 * A stable sort, which `Array.prototype.sort` is required to be, so two notices written in the same
 * second keep the order they were written in.
 */
function interleave(turns: Turn[], notices: ChatNotice[]): Entry[] {
  const entries: Entry[] = [
    ...turns.map((turn): Entry => ({ kind: "turn", turn, at: turn.createdAt })),
    ...notices.map((notice): Entry => ({ kind: "notice", notice, at: notice.created_at })),
  ];
  return entries.sort((left, right) => {
    if (left.at !== right.at) return left.at < right.at ? -1 : 1;
    if (left.kind === right.kind) return 0;
    return left.kind === "turn" ? -1 : 1;
  });
}

/**
 * PURE: the turn a turn follows, skipping whatever a department said in between.
 *
 * `TurnBlock` uses its predecessor to decide which marks to draw above itself — a change of brain, a
 * restart, a rotated context — and all of those are facts about consecutive TURNS. Passing it a
 * notice, or the turn before a notice as though nothing intervened, are both wrong; only the first
 * is a type error, which is why this exists rather than an index arithmetic at the call site.
 */
function previousTurn(all: Entry[], index: number): Turn | null {
  for (let at = index - 1; at >= 0; at -= 1) {
    const entry = all[at];
    if (entry.kind === "turn") return entry.turn;
  }
  return null;
}

/**
 * What a department said here, drawn as a message and never as a turn.
 *
 * **Attributed, always, and that is the whole of what protects the reader.** The words come from an
 * agent that may have been reading the web all afternoon, and nothing filtered them: the tool that
 * writes one is graded `WritesOwn` precisely because a department ALREADY speaks to its owner
 * through its delivery, unfiltered, so a barrier on this door would have stood beside an open one.
 * What replaces the barrier is the reader being able to see whose words these are — so the source
 * is not a tooltip and not a hover, it is on the line.
 *
 * No answer, no cost, no status. A notice has none of those and drawing a turn's chrome around it
 * would be claiming a run that does not exist — the same reason `queued` is not drawn as a turn.
 */
function DepartmentSaid({ notice }: { notice: ChatNotice }) {
  return (
    <li className="chats-notice">
      <p className="chats-notice-who">
        <Link className="chats-notice-from" to={`/teams/runs/${notice.team_run_id}`}>
          {notice.from_agent_id}
        </Link>{" "}
        said this while working
      </p>
      <p className="chats-notice-body">{notice.body}</p>
    </li>
  );
}

/**
 * Memoised: the transcript polls every 1.5–3 s and holds up to a hundred turns a page, each one
 * parsing its answer's markdown when it renders. A settled turn is the same object from one poll to
 * the next (the incremental read leaves it alone, and structural sharing keeps it on a full one), so
 * only the turns that changed draw again.
 */
const TurnBlock = memo(function TurnBlock({
  turn,
  previous,
  clearedAfter,
  chatId,
  onReuse,
  arriving,
  lit,
}: {
  turn: Turn;
  previous: Turn | null;
  clearedAfter: number | null;
  chatId: string;
  /** Puts this question back in the box, unsent. */
  onReuse: (text: string) => void;
  /** Whether this is the turn at the end of the thread. See `Transcript`. */
  arriving: boolean;
  /** Whether a search just brought somebody here. See `Transcript`. */
  lit: boolean;
}) {
  const marks = marksBetween(previous, turn, clearedAfter);
  const live = turnIsLive(turn.status);
  const classes = ["chats-turn"];
  if (arriving) classes.push("chats-turn-arriving");
  if (lit) classes.push("chats-turn-lit");

  return (
    /* The id is how a search addresses this turn — see the jump in `Transcript`. One conversation
       is open at a time, so a turn's own number is unique on the page. */
    <li id={`turn-${turn.id}`} className={classes.join(" ")}>
      {marks.map((mark, index) => (
        <MarkNote key={index} mark={mark} />
      ))}
      <WhoAsked relayedFrom={turn.relayedFrom} chatId={chatId} turnId={turn.id} />
      <div className="chats-turn-said">
        {/* Verbatim, and not through `Rich`: their half is not markdown and is not read as any.
            Somebody who types two asterisks meant two asterisks, and a message redrawn as bold is a
            message they did not send. */}
        <p className="chats-turn-asked">{turn.asked}</p>
        {/* Editing, in the only sense this app can honestly offer.
            A turn is a billed run that already happened, and its answer is in the record; there is
            nothing to rewrite and no history to fork. What a person actually wants after a question
            that came back wrong is to ask a better version of it, and what stops them is retyping
            four lines. So this puts the words back in the box and stops — nothing is sent, nothing
            is deleted, and the turn above stays exactly as it was. */}
        <button
          type="button"
          className="chats-turn-again"
          aria-label="Put this question back in the box to change it"
          title="put it back in the box — nothing is sent until you send it"
          onClick={() => onReuse(turn.asked)}
        >
          <SquarePen className="chats-turn-again-icon" aria-hidden="true" />
        </button>
        {/* With the question, because they were sent with it. They used to be drawn UNDER the
            `núcleo` label — a person's own screenshots, filed in the model's half of the
            exchange — which nobody noticed while both halves were left-aligned and looked the
            same. Moving the question to the right made it obvious. */}
        <TurnPictures paths={turn.images} />
      </div>
      <p className="chats-turn-who">núcleo</p>
      {live && <LiveAnswer turnId={turn.id} since={turn.createdAt} />}
      {live && <StopTurn chatId={chatId} turnId={turn.id} />}
      {!live && <Thought thought={turn.thought} tokens={turn.thoughtTokens} />}
      {!live && <Plan todos={planOf(turn.did)} />}
      {!live && <WhatItDid did={turn.did} turnId={turn.id} settled />}
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
      {!live && <RelaySentNote sent={turn.relayedTo} />}
      {!live && <ForwardTurn chatId={chatId} turn={turn} />}
      <div className="chats-turn-foot">
        {/* Only once the turn has stopped moving. A copy control under an answer that is still
            being written would hand over half a sentence and call it the answer. */}
        {!live && turn.answer !== null && (
          <CopyButton value={turn.answer} label="this answer" />
        )}
        {/* Money only. The daemon's turn rows carry no token breakdown — see `CostLineProps`. */}
        <CostLine costUsd={turn.cost_usd} />
        <ContextFill fill={turn.contextFill} window={turn.window} />
        {/* When it was asked, relative, with the exact time on hover — the same reading the
            runs list gives, because it is the same question being asked of it. */}
        <RelativeTime at={turn.createdAt} />
        <span className="chats-turn-id">#{turn.id}</span>
      </div>
    </li>
  );
});

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
          /* The block, and the one gesture anybody performs on one. A wrapper rather than a
             button inside the `<pre>`: the `<pre>` scrolls sideways, and a control placed in a
             scrolling box slides out of its own corner the moment the code is wider than the
             column — which is exactly when somebody wants to copy it rather than read it. */
          <div key={index} className="chats-code-block">
            <pre className="chats-code">
              <code>{block.text}</code>
            </pre>
            <span className="chats-code-copy">
              <CopyButton value={block.text} label="this code" spoken={false} />
            </span>
          </div>
        ) : block.kind === "table" ? (
          <RichTable key={index} table={block} />
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

/**
 * A table, as a table.
 *
 * It used to be five rows of pipes: `blocks` had no idea one existed, so the whole thing went
 * through the prose path and came out as the characters it was made of. A comparison of three
 * years against three rules is the single most useful shape an agent writes, and it was the one
 * this drew worst.
 *
 * Scrolls inside its own frame rather than widening the column. A six-column table in a
 * conversation is ordinary, and the alternative to scrolling is either a transcript that scrolls
 * sideways as a whole or cells folded until the table stops being one.
 */
function RichTable({
  table,
}: {
  table: Extract<RichBlock, { kind: "table" }>;
}) {
  // The author's own alignment, mapped to a class rather than an inline style: this window runs
  // under a CSP with no `unsafe-inline`, so a `style` attribute is not a thing it can write.
  const align = (at: number) => {
    const set = table.align[at];
    return set === null || set === undefined
      ? "chats-table-cell"
      : `chats-table-cell chats-table-${set}`;
  };

  return (
    <div className="chats-table-wrap">
      <table className="chats-table">
        <thead>
          <tr>
            {table.head.map((cell, at) => (
              <th key={at} className={align(at)} scope="col">
                <RichSpans spans={spansOf(cell)} />
              </th>
            ))}
          </tr>
        </thead>
        <tbody>
          {table.rows.map((row, index) => (
            <tr key={index}>
              {row.map((cell, at) => (
                <td key={at} className={align(at)}>
                  <RichSpans spans={spansOf(cell)} />
                </td>
              ))}
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}

/** The pieces of one line, each drawn as what the parser said it was. */
function RichSpans({ spans }: { spans: RichSpan[] }) {
  return (
    <>
      {spans.map((span, index) =>
        span.kind === "code" ? (
          <code key={index}>{span.text}</code>
        ) : span.kind === "strong" ? (
          <strong key={index}>{span.text}</strong>
        ) : span.kind === "em" ? (
          <em key={index}>{span.text}</em>
        ) : span.kind === "link" ? (
          <RichLink key={index} text={span.text} href={span.href} />
        ) : (
          <span key={index}>{span.text}</span>
        ),
      )}
    </>
  );
}

/**
 * A link in an answer, opened by the OS rather than by this window.
 *
 * A `<button>` and not an `<a href>`, for the reason `lib/vscode.ts` gives at length: an external
 * URL from inside a webview is handled differently per platform and can simply be swallowed, while
 * the opener plugin crosses to the Rust side and asks the OS the way any other program would. It
 * also means no URL from a transcript is ever an `href` in this document.
 *
 * The scheme was already checked in the parser — a `javascript:` URL never became a link span at
 * all — and the plugin's own scope checks it again on the far side. The address is in the `title`
 * because a link whose destination you cannot see before pressing it is a link you should not
 * press, and this text came from a model.
 */
function RichLink({ text, href }: { text: string; href: string }) {
  return (
    <button
      type="button"
      className="chats-rich-link"
      title={href}
      /* Spelled out: the visible text is a phrase from a sentence, and a control whose whole
         accessible name is "calendário gregoriano" announces a noun rather than something that
         opens a browser. */
      aria-label={`Open ${href}`}
      onClick={() => {
        void openUrl(href).catch(() => {
          // Refused by the plugin's scope, or nothing on this machine claims the scheme. The
          // address is in the tooltip either way, which is the honest remainder of the request.
        });
      }}
    >
      {text}
    </button>
  );
}

/** One line of prose, drawn as the shape the parser found. */
function RichLineOut({ line }: { line: RichLine }) {
  if (line.kind === "blank") {
    // The paragraph break somebody typed. An empty `<p>` has no height, which is how every gap in
    // every answer was silently dropped and two thoughts came out as one.
    return <p className="chats-rich-gap" aria-hidden="true" />;
  }
  if (line.kind === "rule") {
    return <hr className="chats-rich-rule" />;
  }
  if (line.kind === "heading") {
    return (
      <p
        className={`chats-rich-heading chats-rich-heading-${Math.min(line.level, 4)}`}
      >
        <RichSpans spans={line.spans} />
      </p>
    );
  }
  if (line.kind === "quote") {
    return (
      <p className="chats-rich-quote">
        <RichSpans spans={line.spans} />
      </p>
    );
  }
  if (line.kind === "bullet") {
    return (
      <p
        className={`chats-rich-bullet chats-rich-depth-${Math.min(line.depth, 3)}`}
      >
        {/* The author's own marker, never renumbered — see `Line.marker`. A dash becomes a
            bullet because a dash is not a character anybody meant to read; a `1.` stays a `1.`
            because it is. */}
        <span className="chats-rich-marker" aria-hidden="true">
          {line.marker ?? "•"}
        </span>
        {/* One flex item, not one per span. The row exists to hang the marker beside the text;
            left unwrapped, every word and every `code` chip became its own flex item — gapped
            apart and shrinkable on its own, so `budget_usd` was squeezed until it broke mid-name
            and stacked vertically. Seen in the app, in a bulleted answer. */}
        <span className="chats-rich-bullet-text">
          <RichSpans spans={line.spans} />
        </span>
      </p>
    );
  }
  return (
    <p className="chats-rich-line">
      <RichSpans spans={line.spans} />
    </p>
  );
}

/**
 * How full the context was, and a word before it is summarised.
 *
 * What it warns about changed and the warning stayed, because what a person wants to know is the
 * same either way: this conversation is near the end of what it can hold. It used to be about to
 * be REPLACED — the next turn began remembering nothing, and the first anybody heard of it was a
 * restart mark drawn after the fact. It is now about to be SUMMARISED, in place, by the CLI, which
 * is mild enough that the sentence had to stop sounding like a threat.
 *
 * The window is the conversation's, never this file's. It arrives on every turn precisely so this
 * side keeps no copy of it — and it genuinely varies now, because a conversation picked up from the
 * editor is given a wider one — so a turn that arrives without it draws the count alone rather than
 * a proportion of a number nobody sent.
 */
function ContextFill({
  fill,
  window,
}: {
  fill: number | null;
  window: number | null;
}) {
  if (fill === null) return null;
  const k = (n: number) => `${(n / 1000).toFixed(1)}k`;
  if (window === null)
    return <span className="chats-turn-fill">{k(fill)} of context</span>;
  // Near, not past. Past is too late to be a warning: this is drawn under the last turn before the
  // summarising, while the next message is still being typed.
  const near = fill >= window * 0.85;
  return (
    <span
      className={
        near ? "chats-turn-fill chats-turn-fill-near" : "chats-turn-fill"
      }
    >
      {`${k(fill)} of ${k(window)}`}
      {/* Short, because of where it is: this hangs off the end of a reading, in the footing
          under every turn, and the full sentence it used to be ran the line to twice the
          width of the two numbers it was qualifying. What a person needs from it is the
          fact, not the explanation — and the explanation is one hover away. */}
      {near && " — earlier turns summarised soon"}
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
 * Its own component so the stream lives and dies with the live turn — mounted only where `TurnBlock`
 * has decided the turn is in flight, so a settled conversation asks the daemon nothing at all. The
 * poll is only the fallback for a stream that cannot open or drops.
 *
 * Three states, and they are different claims. Nothing written and no tool is "thinking…", which is
 * what this said before and is still the honest answer while the daemon has nothing to show. A tool
 * running is named, because "thinking" over a command that is compiling something is the wrong word
 * for the wait. And words already written are shown as they arrive.
 */
function LiveAnswer({ turnId, since }: { turnId: number; since: string }) {
  const live = useLiveTurn(turnId, true, { stream: true });
  const text = live.data?.text ?? "";
  const doing = live.data?.doing ?? null;
  const keepUp = useContext(KeepsUp);

  /*
   * Follows the words down, and its own here is the only place that can: the list above does not
   * re-render while a turn writes — the words arrive on this component's poll — so the door's
   * scroll effect never fires for any of it.
   *
   * `keepUp` and not `scrollIntoView` on a line of its own. This used to bring the spinner into
   * view, which is not the end of the box: `Stop`, the turn's footing and anything the run changed
   * are all drawn under it, so a turn in flight sat with its own controls off the bottom.
   *
   * And it went where it liked. `keepUp` declines when the reader has scrolled away, so reading
   * something further up while an answer writes is no longer a thing the page undoes once a
   * second.
   */
  useEffect(() => {
    keepUp();
  }, [keepUp, text, doing]);

  return (
    <>
      <Thought
        thought={live.data?.thought ?? []}
        tokens={live.data?.thought_tokens ?? null}
      />
      {text !== "" && (
        <div className="chats-turn-answer chats-turn-writing">
          <Rich text={text} />
        </div>
      )}
      <Plan todos={planOf(live.data?.did ?? [])} />
      {/* `settled={false}`: the answers are already on this stream — the live route reads it whole
          on every poll — and the database column they would be fetched from is not written until
          the turn ends. */}
      <WhatItDid did={live.data?.did ?? []} turnId={turnId} settled={false} />
      <p className="chats-turn-live">
        {/* Turning, because the three words below can stand unchanged for two minutes while a
            build runs and a page that never moves is a page that looks stopped. For anybody who
            asked for less motion the icon is removed outright rather than left to the global
            clamp in `base.css`, which would land it on its last frame and hold it there — a
            broken ring standing still says "damaged". The elapsed clock carries the proof of
            life instead; see `.chats-turn-spinner` in `chats.css`. */}
        <LoaderCircle className="chats-turn-spinner" aria-hidden="true" />
        <span>
          {doing !== null
            ? `running ${doing}…`
            : text === ""
              ? "thinking…"
              : "writing…"}
        </span>
        {/* The seconds, which are the part that says it is still alive. Deliberately outside
            anything a screen reader watches: a live region that re-announced a ticking clock
            once a second would make the page unusable for the person it was meant to help. */}
        <Elapsed since={since} />
      </p>
    </>
  );
}

/**
 * How long this turn has been going, ticking.
 *
 * Its own component, and that is the whole reason it exists: a second-by-second reading held in
 * `LiveAnswer` would re-render the words being written, the plan and the tool list once a second
 * for as long as a run lasts. Here the interval moves this span and nothing else.
 *
 * The interval is cleared on unmount, which is the moment the turn lands.
 */
function Elapsed({ since }: { since: string }) {
  const started = Date.parse(since);
  const [now, setNow] = useState(() => Date.now());

  useEffect(() => {
    const tick = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(tick);
  }, []);

  // A timestamp this side cannot read is not drawn as `NaN:aN`. The line above it already says
  // the turn is running, which is the part that matters.
  if (Number.isNaN(started)) return null;
  return (
    <span className="chats-turn-elapsed">{elapsedText(started, now)}</span>
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
 *
 * Rendered through a portal on `document.body`, and it has to be. `.chats-app` is a query container
 * now — that is what moved the layout's fold off the window and onto the column it actually governs
 * — and a query container is a containing block for `position: fixed` descendants. Left where it
 * was, this would have covered the main column and stopped at the rail, which is the one thing a
 * lightbox must not do. A React portal moves only the DOM node: events still bubble through the
 * React tree, so every handler above it is untouched.
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

  return createPortal(
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
    </div>,
    document.body,
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
/**
 * What the turn ran — and, when you open one, what it answered.
 *
 * The list said what the model REACHED FOR and never what it found: `Bash` beside
 * `cargo test dates::`, with no way to learn from the conversation whether the tests passed. The
 * paragraph underneath is the model's summary of exactly that, and a summary is the thing somebody
 * opening a tool call has decided not to take on trust.
 *
 * One open at a time. Two answers of thirty lines each, unfolded together in the middle of a
 * transcript, is a turn nobody can read past — and the gesture is "let me check that one", not
 * "expand everything".
 *
 * The answers are NOT on the transcript: see `ToolCall.result`. So this fetches them for its own
 * turn, once, the first time anything here is opened. A live turn is the exception and carries them
 * already — its stream is being read on every poll anyway, and it is one turn rather than a hundred.
 */
function WhatItDid({
  did,
  turnId,
  settled,
}: {
  did: ToolCall[];
  turnId: number;
  /** Whether the turn has ended. A live turn's answers are not in the database yet. */
  settled: boolean;
}) {
  const [open, setOpen] = useState<number | null>(null);
  const tools = useTurnTools(turnId, settled && open !== null);
  // The fetched list when there is one, and what the transcript gave otherwise. Same calls in the
  // same order either way — the daemon reads both from one column.
  const calls = tools.data?.did ?? did;

  // A call with a `parent` ran inside a subagent; the agent map shows it there, and listing it
  // here as well would bury the main agent's own steps. `index` stays the position in `did`.
  const mine = did.flatMap((call, index) => (call.parent ? [] : [{ call, index }]));
  if (mine.length === 0) return null;
  return (
    <ul className="chats-turn-did" aria-label="What it did">
      {mine.map(({ call, index }) => {
        // Keyed by position: this is a record of what happened, in order, and nothing reorders or
        // removes an entry. The same tool on the same file twice is two real calls, not a duplicate.
        const key = `${call.name}-${index}`;
        const shown = open === index;
        return (
          <li key={key}>
            <button
              type="button"
              className="chats-turn-did-open"
              aria-expanded={shown}
              aria-label={`${call.name}${call.detail === null ? "" : ` ${call.detail}`} — what it answered`}
              onClick={() => setOpen(shown ? null : index)}
            >
              <ChevronRight
                className={
                  shown
                    ? "chats-turn-did-caret chats-turn-did-caret-open"
                    : "chats-turn-did-caret"
                }
                aria-hidden="true"
              />
              <span className="chats-turn-did-name">{call.name}</span>
              {call.detail !== null && (
                <span className="chats-turn-did-detail">{call.detail}</span>
              )}
            </button>
            {shown && (
              <ToolAnswer
                call={calls[index] ?? call}
                loading={settled && tools.data === undefined && !tools.isError}
              />
            )}
          </li>
        );
      })}
    </ul>
  );
}

/**
 * What one tool said back.
 *
 * Absence is drawn as absence rather than as an empty box, and it is not one fact: a turn recorded
 * before the daemon kept these has nothing to show, and so does a tool that genuinely answered
 * nothing. Neither is a failure and the line says the true, narrow thing — nothing was recorded.
 */
function ToolAnswer({ call, loading }: { call: ToolCall; loading: boolean }) {
  if (loading) {
    return <p className="chats-tool-answer-note">reading what it answered…</p>;
  }
  const text = call.result ?? null;
  if (text === null || text === "") {
    return (
      <p className="chats-tool-answer-note">nothing was recorded for this one</p>
    );
  }
  const whole = call.result_chars ?? text.length;
  const cut = whole > text.length;
  return (
    <div className="chats-tool-answer">
      <pre
        className={
          call.result_failed === true
            ? "chats-tool-answer-text chats-tool-answer-failed"
            : "chats-tool-answer-text"
        }
      >
        <code>{text}</code>
      </pre>
      <div className="chats-tool-answer-foot">
        {/* What is NOT being shown, said plainly. A truncation presented as the whole answer is
            how somebody concludes a command printed nothing after the first thirty lines. */}
        {cut && (
          <span className="chats-tool-answer-cut">
            the first {text.length.toLocaleString()} of{" "}
            {whole.toLocaleString()} characters
          </span>
        )}
        {call.result_failed === true && (
          <span className="chats-tool-answer-error">it answered with an error</span>
        )}
        <CopyButton value={text} label="this tool's answer" />
      </div>
    </div>
  );
}

/**
 * Who put the words at the top of this turn there.
 *
 * "you" is the ordinary answer and was the only one until conversations could hand messages to each
 * other. A relayed turn drawn under "you" is not a missing decoration — it is the transcript
 * naming the wrong speaker, telling the person reading it that they said something they did not
 * say, in the one place they go to find out what was actually said.
 *
 * The name is a link because the conversation on the far side is a real place, and the next thing
 * somebody wants after "where did this come from" is to go and look. An unnamed conversation is
 * described rather than identified: most chats carry no title until the daemon has summarised one,
 * and printing a uuid at a person answers a question nobody asked — the link still goes there.
 */
function WhoAsked({
  relayedFrom,
  chatId,
  turnId,
}: {
  relayedFrom: RelayedFrom | null;
  chatId: string;
  turnId: number;
}) {
  if (relayedFrom === null)
    return <p className="chats-turn-who chats-turn-who-you">you</p>;
  return (
    <p className="chats-turn-who chats-turn-who-relayed">
      <Link className="chats-turn-relayed-from" to={`/chats/${relayedFrom.chatId}`}>
        {relayedFrom.title ?? "an unnamed conversation"}
      </Link>{" "}
      handed this over <RelayChain chatId={chatId} turnId={turnId} />
    </p>
  );
}

/**
 * What this turn handed to another conversation.
 *
 * Drawn from what the daemon WROTE, not from the tool call the model made — the two part company
 * every time a relay is refused, and a sender's transcript built from the asks would show messages
 * that never arrived. An empty list is the ordinary case and draws nothing.
 *
 * The words are shown, not just the destination. "Sent something to «planning»" is the shape that
 * makes somebody open the other conversation to find out what; the point of putting this here at
 * all is that they should not have to.
 */
function RelaySentNote({ sent }: { sent: RelaySent[] }) {
  if (sent.length === 0) return null;
  return (
    <ul className="chats-relay-sent" aria-label="Handed to other conversations">
      {sent.map((relay, index) => (
        <li key={index} className="chats-relay-sent-item">
          <span className="chats-relay-sent-to">
            {"handed to "}
            <Link className="chats-turn-relayed-from" to={`/chats/${relay.chat_id}`}>
              {relay.title ?? "an unnamed conversation"}
            </Link>
          </span>
          <span className="chats-relay-sent-body">{relay.body}</span>
        </li>
      ))}
    </ul>
  );
}

/**
 * The whole path a relayed turn travelled, opened on demand.
 *
 * Behind a button rather than always drawn, and fetched only once opened: with three hops allowed,
 * "B spoke to me" hides that A began it — but that is a question somebody asks occasionally, and
 * the transcript around it is re-read every second and a half.
 *
 * The conversation being read is the last step of its own chain, and is drawn like the rest. It is
 * where the path ENDS, and a path drawn without its destination is one you have to hold the missing
 * end of in your head.
 */
function RelayChain({ chatId, turnId }: { chatId: string; turnId: number }) {
  const [open, setOpen] = useState(false);
  const chain = useRelayChain(chatId, turnId, open);

  return (
    <span className="chats-relay-chain">
      <button
        type="button"
        className="chats-relay-chain-toggle"
        aria-expanded={open}
        onClick={() => setOpen((was) => !was)}
      >
        {open ? "hide the path" : "where did this start?"}
      </button>
      {open && chain.data !== undefined && (
        <ol className="chats-relay-chain-steps" aria-label="The path this turn travelled">
          {chain.data.chain.map((step) => (
            <li key={step.chat_id}>
              <Link className="chats-turn-relayed-from" to={`/chats/${step.chat_id}`}>
                {step.title ?? "an unnamed conversation"}
              </Link>
            </li>
          ))}
        </ol>
      )}
    </span>
  );
}

/**
 * Hands this turn to another conversation.
 *
 * The gesture the model already had and the person did not: `send_to_chat` is on the model's tool
 * list and there was no button anywhere that did the same thing.
 *
 * The text starts as what the turn answered and stays editable, because forwarding is rarely
 * verbatim — the useful version is usually "look at this, and here is why". What travels is what is
 * in the box when it is sent, never what the box was filled with.
 *
 * A turn with no answer offers nothing to forward. Drawing the button anyway would put an empty
 * message one click away, and an empty relay is a turn started in another conversation about
 * nothing.
 */
function ForwardTurn({ chatId, turn }: { chatId: string; turn: Turn }) {
  const [open, setOpen] = useState(false);
  const [text, setText] = useState("");
  const chats = useChats();
  const forward = useForwardTurn();

  if (turn.answer === null) return null;

  const elsewhere = (chats.data ?? []).filter((row) => row.chat_id !== chatId);

  return (
    <div className="chats-forward">
      <button
        type="button"
        className="chats-forward-open"
        aria-expanded={open}
        onClick={() => {
          setText(turn.answer ?? "");
          setOpen((was) => !was);
        }}
      >
        {open ? "cancel" : "hand to another conversation"}
      </button>
      {open && (
        <div className="chats-forward-panel">
          <label className="chats-forward-label" htmlFor={`forward-${turn.id}`}>
            What to send
          </label>
          <textarea
            id={`forward-${turn.id}`}
            className="chats-forward-text"
            value={text}
            onChange={(event) => setText(event.target.value)}
          />
          {elsewhere.length === 0 && (
            <p className="chats-forward-empty">there is no other conversation to hand this to.</p>
          )}
          <ul className="chats-forward-targets" aria-label="Hand it to">
            {elsewhere.map((row) => (
              <li key={row.chat_id}>
                <button
                  type="button"
                  className="chats-forward-target"
                  disabled={forward.isPending || text.trim() === ""}
                  onClick={() => {
                    forward.mutate(
                      { toChatId: row.chat_id, fromTurnId: turn.id, text },
                      { onSuccess: () => setOpen(false) },
                    );
                  }}
                >
                  {row.title ?? row.first_message ?? "New conversation"}
                </button>
              </li>
            ))}
          </ul>
          {/* The daemon's own word for why, not a sentence this side invented: `relay_cycle`,
              `relay_too_deep`, `owner_away`. A refusal shown as "something went wrong" is one
              nobody can act on, and each of these has a different answer. */}
          {forward.isError && (
            <p className="chats-forward-refused" role="status">
              {isApiRefusal(forward.error)
                ? (MESSAGE_SENTENCES[forward.error.code] ?? forward.error.code)
                : "could not hand it over"}
            </p>
          )}
        </div>
      )}
    </div>
  );
}

/**
 * The brain, restart and clear marks a transcript draws above one turn.
 *
 * The brain mark's copy is deliberately asymmetric: moving *to* the cloud is about where what you
 * type now goes, and moving *to* the local model is about where the answer comes from — the two
 * directions are not mirror images of the same fact.
 *
 * Moving *to* the hosted model earns a third sentence, not a shared one: it is not the CLI's
 * cloud — that word is spoken for — and unlike the local model it does not run on this machine
 * either, so falling into that branch would tell a specific lie, that an answer sent to somebody
 * else's server "was answered on this machine". What IS true of it, the way "leaves this machine"
 * is true of the cloud model, is that it goes to a third-party provider the owner chose from a
 * list, which is the fact this sentence states instead.
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
  if (mark.kind === "compacted") {
    return (
      <p className="chats-mark chats-mark-compacted" role="status">
        summarised here — the conversation filled its window, so everything
        above was condensed into a summary the model carries on from. Same
        conversation, shorter memory of its early part.
      </p>
    );
  }
  const text =
    mark.to === "cloud"
      ? "moved to the cloud model — from here, what you type leaves this machine"
      : mark.to === "openrouter"
        ? "moved to the hosted model — from here, what you type goes to a third-party provider you chose"
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
  // The relay's own refusals. Each has a different answer, which is why they are sentences here
  // rather than one "something went wrong": a cycle is a different conversation to pick, a chain
  // too deep is nothing you can fix from this window, and an absent owner clears by itself.
  no_such_destination: "that conversation no longer exists, or was archived",
  relay_to_self: "this is the conversation you are already in",
  relay_cycle:
    "that conversation already handed this one a message — passing it back would go in circles",
  relay_too_deep: "this has already been handed on as far as it goes",
  owner_away:
    "nothing is handed over while nobody is at the machine to see it arrive",
  telegram_origin:
    "a turn that arrived from Telegram cannot be handed to another conversation",
  unknown_origin:
    "this turn does not record where it came from, so it cannot be handed on",
  unknown_sender: "the turn being handed over could not be found",
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
  reuse,
  onNeedsProject,
}: {
  chatId: string;
  /** The row, or undefined while the list is still being read. */
  chat: ChatSummary | undefined;
  /** A question lifted out of the transcript, or null. See `ChatDetail`. */
  reuse?: { text: string; at: number } | null;
  /** Open the project picker: something was said to a conversation that has nowhere to run. */
  onNeedsProject?: () => void;
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
  const project = useChatProject(chatId);
  // Sendable only once the conversation is KNOWN to have a directory. Unknown is not "none", but
  // it is not "some" either, and what is said while it is being read waits the half-second.
  const rooted = project.data !== undefined && project.data.cwd !== null;
  const held = useHeldMessage(chatId);
  // Held here rather than inside the toggle, because the toggle and the line below the box are two
  // views of ONE microphone. Two `useDictation` calls would be two recordings.
  const dictation = useDictationInto(text, setText, setCaret, box);

  /**
   * A question put back in the box, and the caret at the end of it.
   *
   * It REPLACES what is in the box rather than appending to it, which is the honest reading of
   * the gesture: somebody pressed the pencil on a specific question because that is the one
   * they want to send a better version of. Appending would leave two half-questions in the box
   * and the send button armed.
   *
   * Keyed on the stamp and not the text — see `reuse` in `ChatDetail` for why.
   */
  const at = reuse?.at ?? null;
  const asked = reuse?.text ?? "";
  useEffect(() => {
    if (at === null) return;
    setText(asked);
    setCaret(asked.length);
    // Next frame, for the reason `write` gives: React has not re-rendered the new value yet, so
    // there is nothing to put a caret at the end OF until it has.
    requestAnimationFrame(() => {
      box.current?.focus();
      box.current?.setSelectionRange(asked.length, asked.length);
    });
    // `asked` is deliberately not a dependency: the stamp is what says this is a new gesture,
    // and including the text would re-run this on nothing whenever an identical question is
    // reused twice.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [at]);

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

  // "Mention a file…" in the "+" menu is the `@` gesture without the keyboard: put one at the caret,
  // after a space when the letter before it is not whitespace, and the completion opens by itself.
  const mentionHere = () => {
    const at = box.current?.selectionStart ?? text.length;
    const lead = at > 0 && !/\s/.test(text[at - 1]) ? " " : "";
    write({
      text: `${text.slice(0, at)}${lead}@${text.slice(at)}`,
      caret: at + lead.length + 1,
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
    if (!rooted) {
      // Not sent: a conversation does not start before it knows where it runs. See `held.ts`.
      holdMessage(chatId, { text: text.trim(), images: attached }, MAX_PICTURES);
      setText("");
      setAttached([]);
      if (project.data !== undefined) onNeedsProject?.();
      return;
    }
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

  // The held message goes out the moment there is somewhere for it to run. Taken out of the store
  // BEFORE the send, so a re-render mid-flight cannot send it twice; a send that fails puts the
  // words back in the box rather than losing them.
  useEffect(() => {
    if (!rooted || held === null) return;
    const message = takeHeld(chatId);
    if (message === null) return;
    send.mutate(message, {
      onError: () => {
        setText((was) => (was === "" ? message.text : `${message.text}

${was}`));
        setAttached((was) => [...message.images, ...was].slice(0, MAX_PICTURES));
      },
    });
    // `send` is a fresh object every render; the trigger is the project arriving or a hold landing.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [rooted, held, chatId]);

  // Back into the box, unsent: the way to change your mind about something held.
  const unhold = () => {
    const message = takeHeld(chatId);
    if (message === null) return;
    setText((was) => (was === "" ? message.text : `${message.text}

${was}`));
    setAttached((was) => [...message.images, ...was].slice(0, MAX_PICTURES));
    requestAnimationFrame(() => box.current?.focus());
  };

  return (
    <form
      className="chats-composer"
      onSubmit={(event) => {
        event.preventDefault();
        say();
      }}
    >
      {held !== null && (
        <div className="chats-held" role="status" aria-label="Held message">
          <p className="chats-held-text">
            {held.text}
            {held.images.length > 0 &&
              ` (+${held.images.length} ${held.images.length === 1 ? "picture" : "pictures"})`}
          </p>
          <p className="chats-held-why">
            held — not sent yet; it goes out as soon as this conversation has a project
          </p>
          <div className="chats-held-actions">
            <Button type="button" intent="go" onClick={() => onNeedsProject?.()}>
              Choose a project
            </Button>
            <Button type="button" variant="quiet" onClick={unhold}>
              Edit
            </Button>
          </div>
        </div>
      )}
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
        {/* Voice at the top right, away from send at the bottom right. They were neighbours in one
            row, and they are the two controls in this box that both mean "begin" — one by talking,
            one by sending — so a hand reaching for one was a hand next to the other.

            **On the line being typed on, as its sibling.** It had a row of its own first, and that
            row was height every composer paid for whether or not anybody ever talked. The corner is
            reachable without spending it: the button and the textarea share one flex line, the
            textarea takes what is left, and `align-items: flex-start` keeps the button at the top
            as the text grows down past it.

            Which also settles the objection that produced the extra row. This button GROWS when it
            is recording, gaining the phase label, so a corner held by `position: absolute` would
            have to reserve room for a width it only sometimes has — reserve for the small state and
            it covers the text mid-sentence, reserve for the large one and there is a hole in the box
            whenever nobody is talking. A flex sibling reserves nothing and overlaps nothing: when it
            grows, the textarea gives up the width. */}
        <div className="chats-composer-line">
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
          <DictateToggle dictation={dictation} />
        </div>
        {/* The controls belong to the words being typed, so they live in the box with them — which
            is the one structure every reference for this page shares. They used to be scattered:
            the model and plan-only behind a `⋯` at the top of the page, the attach button in a
            strip under the box. Asking "which model, and does it plan or does it do" a screen away
            from the sentence those answers apply to is asking about the message somewhere the
            message is not. */}
        <div className="chats-composer-actions">
          {/* The way in for anything not on the clipboard: pictures, small text files, more folders,
              the web. One "+" rather than a bare file input, which nobody can style into the others. */}
          <PlusMenu
            chatId={chatId}
            onPictures={(files) => void attach(files)}
            onText={(context) =>
              setText((was) => (was === "" ? context : `${was}\n${context}`))
            }
            onMention={mentionHere}
          />
          {chat !== undefined && (
            <ChatModelControls
              chatId={chatId}
              model={chat.model}
              effort={chat.effort}
            />
          )}
          <span className="chats-composer-gap" />
          {/* The one word about the microphone, on a row that exists at this height whether it
              says anything or not. It was a paragraph under the box until 2026-09-20, and under
              the box is under a composer pinned to the foot of a conversation: every "nothing was
              heard" pushed the transcript up a line and the next press pulled it back down. It
              shrinks rather than grows, with the whole sentence on `title`. */}
          {dictation.trouble !== null && (
            <span className="chats-dictation-trouble" title={dictation.trouble}>
              {dictation.trouble}
            </span>
          )}
          {/* Last before send, because it is the answer most likely to be changed in the moment of
              sending — "actually, plan this one" — and the hand is already on that corner. */}
          <PermissionMenu chatId={chatId} />
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
