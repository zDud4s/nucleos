import { useState, type CSSProperties, type ReactNode } from "react";
import { Link } from "@tanstack/react-router";
import {
  Archive,
  ArchiveRestore,
  ListFilter,
  MoreHorizontal,
  Plus,
  Search,
  SquareCode,
} from "lucide-react";
import {
  DropdownMenu,
  DropdownMenuCheckboxItem,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuLabel,
  DropdownMenuRadioGroup,
  DropdownMenuRadioItem,
  DropdownMenuSeparator,
  DropdownMenuSub,
  DropdownMenuSubContent,
  DropdownMenuSubTrigger,
  DropdownMenuTrigger,
} from "../ui/vendor/dropdown-menu";
import {
  useArchiveChat,
  useArchivedChats,
  useChatGroups,
  useCreateChatGroup,
  useDeleteChatGroup,
  useIdeSessions,
  useRenameChatGroup,
  useRestoreChat,
  useSetChatGroup,
  type ChatGroup,
  type ChatSummary,
  type IdeSession,
} from "../data/chats";
import { ErrorNote, RelativeTime, Teach, relativeText } from "../ui";
import { stillGoing } from "../lib/editor";
import { BAND_TITLE, inBands } from "../lib/when";
import {
  applyFilters,
  byGroup,
  chatHue,
  countSessions,
  dotFor,
  type Dot,
  type SessionStatus,
  type TabState,
} from "./sessions";

/**
 * One row of the column, from either source.
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
export function mergeRows(chats: ChatSummary[], sessions: IdeSession[]): ListRow[] {
  const pickedUp = new Set(
    chats.map((chat) => chat.ide_session_id).filter((id): id is string => id !== null),
  );
  const rows: ListRow[] = [
    ...chats.map((chat) => ({ kind: "chat" as const, at: chat.last_activity, chat })),
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

/** What each dot is called out loud. */
export const DOT_NAME: Record<Dot, string> = {
  needs_input: "Needs input",
  working: "Working",
  unread: "Unread",
  seen: "Seen",
};

/** The title a row goes by. */
export function sessionTitle(row: ChatSummary): string {
  return row.title ?? row.first_message ?? "nothing said yet";
}

/**
 * What this row is called out loud.
 *
 * Spelled out rather than left to the name computation over the children, for the same reason the
 * sidebar's own nav items are: adjacent inline text — title, brain, the unread count — concatenates
 * with no separator, and a screen reader would announce "hello therecloud3" instead of a sentence.
 */
function rowLabel(row: ChatSummary, live: boolean): string {
  const parts = [sessionTitle(row), row.brain];
  if (live) parts.push("thinking");
  if (row.last_activity !== null)
    parts.push(relativeText(Date.parse(row.last_activity), Date.now()));
  if (row.waiting > 0) parts.push(`${row.waiting} unread`);
  const relayed = row.relayed_waiting ?? 0;
  if (relayed > 0) parts.push(`${relayed} from another conversation`);
  const said = row.notices_waiting ?? 0;
  if (said > 0) parts.push(`${said} from a team`);
  return parts.join(", ");
}

/**
 * The conversation's mark. Given `chatId`, it is filled with that chat's own colour (`chatHue`),
 * the same on its row and on its tab, so the two can be matched at a glance; the state, when there
 * is one, is then a ring around it. Never the accent: see `reserved-cyan.test.ts`.
 */
export function StateDot({ dot, chatId }: { dot: Dot | null; chatId?: string }) {
  const own = chatId !== undefined;
  const className = `chats-dot chats-dot-${dot ?? "none"}${own ? " chats-dot-chat" : ""}`;
  const style = own ? ({ "--chat-hue": chatHue(chatId) } as CSSProperties) : undefined;
  if (dot === null) return <span className={className} style={style} aria-hidden="true" />;
  return <span className={className} style={style} role="img" aria-label={DOT_NAME[dot]} />;
}

export interface SessionColumnProps {
  rows: ChatSummary[];
  answered: boolean;
  selected: string | null;
  /** The open conversation's turn is running, which the list may not have heard of yet. */
  selectedLive: boolean;
  pickingUp: string | null;
  onPickUp: (sessionId: string) => void;
  /** Told before a `<Link>` navigates, so the editor preview does not outlive the press. */
  onOpenChat: () => void;
  onNew: () => void;
  openTabs: string[];
}

export function SessionColumn({
  rows,
  answered,
  selected,
  selectedLive,
  pickingUp,
  onPickUp,
  onOpenChat,
  onNew,
  openTabs,
}: SessionColumnProps) {
  // Watched, because one of these may be being typed into in the editor while it is on screen here.
  const sessions = useIdeSessions(true, true);
  const groupsQuery = useChatGroups();
  const groups: ChatGroup[] = Array.isArray(groupsQuery.data) ? groupsQuery.data : [];
  const createGroup = useCreateChatGroup();

  const [statuses, setStatuses] = useState<Set<SessionStatus>>(new Set());
  const [tabStates, setTabStates] = useState<Set<TabState>>(new Set());
  const [query, setQuery] = useState("");
  const [naming, setNaming] = useState(false);
  const [newName, setNewName] = useState("");

  // The open conversation is running before the list has been told; trust the transcript.
  const live = (row: ChatSummary) =>
    row.working === true || (row.chat_id === selected && selectedLive);
  const lit = rows.map((row) =>
    row.chat_id === selected && selectedLive && row.working !== true
      ? { ...row, working: true, activity: "working" as const }
      : row,
  );

  const counts = countSessions(lit, openTabs);
  const filtering = statuses.size > 0 || tabStates.size > 0 || query.trim() !== "";
  const shown = applyFilters(lit, { statuses, tabs: tabStates, query }, openTabs);
  const split = byGroup(shown, groups);

  const needle = query.trim().toLowerCase();
  const editors: IdeSession[] =
    statuses.size > 0 || tabStates.size > 0
      ? []
      : (sessions.data ?? []).filter(
          (session) =>
            needle === "" ||
            (session.title ?? session.session_id).toLowerCase().includes(needle),
        );
  const ungrouped = mergeRows(split.ungrouped, editors);
  const ungroupedCount = ungrouped.length;

  const activeOnly =
    statuses.size === 2 && statuses.has("needs_input") && statuses.has("working");

  function toggle<T>(set: Set<T>, value: T, put: (next: Set<T>) => void) {
    const next = new Set(set);
    if (next.has(value)) next.delete(value);
    else next.add(value);
    put(next);
  }

  function commitGroup() {
    const name = newName.trim();
    if (name === "") return;
    createGroup.mutate(name, {
      onSuccess: () => {
        setNaming(false);
        setNewName("");
      },
    });
  }

  const renderRow = (entry: ListRow) =>
    entry.kind === "chat" ? (
      <SessionRow
        key={entry.chat.chat_id}
        row={entry.chat}
        active={entry.chat.chat_id === selected}
        live={live(entry.chat)}
        tabOpen={openTabs.includes(entry.chat.chat_id)}
        groups={groups}
        onOpen={onOpenChat}
      />
    ) : (
      <EditorRow
        key={entry.session.session_id}
        session={entry.session}
        active={entry.session.session_id === pickingUp}
        onOpen={() => onPickUp(entry.session.session_id)}
      />
    );

  return (
    <>
      <div className="chats-rail-actions">
        {/* Opens an empty chat, and asks nothing: the model is a control in the box, answerable
            while you type the first sentence and changeable after it. */}
        <button type="button" className="chats-rail-new" onClick={onNew}>
          <Plus className="chats-rail-icon" aria-hidden="true" />
          New session
        </button>
        <button
          type="button"
          className="chats-rail-new chats-rail-new-group"
          aria-expanded={naming}
          onClick={() => setNaming(true)}
        >
          <Plus className="chats-rail-icon" aria-hidden="true" />
          New group
        </button>
      </div>

      <div className="chats-session-tools">
        <DropdownMenu>
          <DropdownMenuTrigger
            className="chats-session-filter"
            aria-label="Filter sessions"
            data-on={statuses.size + tabStates.size > 0 ? "" : undefined}
          >
            <ListFilter className="chats-rail-icon" aria-hidden="true" />
          </DropdownMenuTrigger>
          <DropdownMenuContent align="start" className="chats-session-menu">
            <DropdownMenuLabel>Status</DropdownMenuLabel>
            {(
              [
                ["needs_input", "Needs input", counts.needs_input],
                ["working", "Working", counts.working],
                ["completed", "Completed", counts.completed],
              ] as const
            ).map(([key, label, n]) => (
              <DropdownMenuCheckboxItem
                key={key}
                checked={statuses.has(key)}
                onCheckedChange={() => toggle(statuses, key, setStatuses)}
                onSelect={(event) => event.preventDefault()}
              >
                {`${label} (${n})`}
              </DropdownMenuCheckboxItem>
            ))}
            <DropdownMenuSeparator />
            <DropdownMenuLabel>Tabs</DropdownMenuLabel>
            {(
              [
                ["open", "Open", counts.open],
                ["closed", "Closed", counts.closed],
              ] as const
            ).map(([key, label, n]) => (
              <DropdownMenuCheckboxItem
                key={key}
                checked={tabStates.has(key)}
                onCheckedChange={() => toggle(tabStates, key, setTabStates)}
                onSelect={(event) => event.preventDefault()}
              >
                {`${label} (${n})`}
              </DropdownMenuCheckboxItem>
            ))}
          </DropdownMenuContent>
        </DropdownMenu>

        <button
          type="button"
          className="chats-session-chip"
          aria-pressed={activeOnly}
          onClick={() =>
            setStatuses(activeOnly ? new Set() : new Set<SessionStatus>(["needs_input", "working"]))
          }
        >
          {`Active · ${counts.active}`}
        </button>

        <label className="chats-session-search">
          <Search className="chats-rail-icon" aria-hidden="true" />
          <input
            type="text"
            aria-label="Search sessions"
            placeholder="Search"
            value={query}
            onChange={(event) => setQuery(event.target.value)}
          />
        </label>
      </div>

      {/* Only while a group is being named; the button that opens it sits beside New session. */}
      {naming && (
        <div className="chats-session-groupbar">
          <input
            className="chats-group-name"
            aria-label="Group name"
            placeholder="Group name"
            value={newName}
            autoFocus
            onChange={(event) => setNewName(event.target.value)}
            onKeyDown={(event) => {
              if (event.key === "Enter") commitGroup();
              if (event.key === "Escape") {
                setNaming(false);
                setNewName("");
              }
            }}
          />
        </div>
      )}

      <div className="chats-rail-scroll">
        {!answered && <p className="chats-loading">reading your conversations…</p>}
        {answered && rows.length === 0 && editors.length === 0 && groups.length === 0 && (
          <Teach title="No conversations yet">
            <p>
              Telegram&apos;s own conversations do not show up here — nothing has opened a row for
              them, because the only door into this list is the button above. Start one to see it
              appear.
            </p>
          </Teach>
        )}
        {sessions.isError && <ErrorNote>your editor sessions could not be read</ErrorNote>}

        {split.groups.map(({ group, rows: inGroup }) =>
          filtering && inGroup.length === 0 ? null : (
            <GroupSection key={group.id} group={group} count={inGroup.length}>
              <ul className="chats-list" aria-label={group.name}>
                {inGroup.map((chat) =>
                  renderRow({ kind: "chat", at: chat.last_activity, chat }),
                )}
              </ul>
            </GroupSection>
          ),
        )}

        <details className="chats-group" open>
          <summary className="chats-group-head">
            <span className="chats-group-name-text">{`Ungrouped (${ungroupedCount})`}</span>
          </summary>
          {ungrouped.length > 0 && (
            /* One list, both kinds, cut into days. `inBands` never reorders; it only says where
               the list changes day. */
            <ul className="chats-list" aria-label="Conversations">
              {inBands(ungrouped, (entry) => entry.at, Date.now()).map((cut) => (
                <li key={cut.band} className="chats-band">
                  <p className="chats-band-title">{BAND_TITLE[cut.band]}</p>
                  <ul className="chats-list" aria-label={BAND_TITLE[cut.band]}>
                    {cut.rows.map(renderRow)}
                  </ul>
                </li>
              ))}
            </ul>
          )}
        </details>

        <ArchivedSection />
      </div>
    </>
  );
}

/** A user group: a collapsible section with its count, and a menu to rename or delete it. */
function GroupSection({
  group,
  count,
  children,
}: {
  group: ChatGroup;
  count: number;
  children: ReactNode;
}) {
  const rename = useRenameChatGroup();
  const remove = useDeleteChatGroup();
  const [renaming, setRenaming] = useState(false);
  const [name, setName] = useState(group.name);

  return (
    <details className="chats-group" open>
      <summary className="chats-group-head">
        {renaming ? (
          <input
            className="chats-group-name"
            aria-label="Rename group"
            value={name}
            autoFocus
            onClick={(event) => event.preventDefault()}
            onChange={(event) => setName(event.target.value)}
            onKeyDown={(event) => {
              event.stopPropagation();
              if (event.key === "Enter" && name.trim() !== "") {
                rename.mutate({ id: group.id, name: name.trim() });
                setRenaming(false);
              }
              if (event.key === "Escape") {
                setName(group.name);
                setRenaming(false);
              }
            }}
          />
        ) : (
          <span className="chats-group-name-text">{`${group.name} (${count})`}</span>
        )}
        <DropdownMenu>
          <DropdownMenuTrigger
            className="chats-meta-more chats-group-more"
            aria-label={`Group actions for ${group.name}`}
            onClick={(event) => event.stopPropagation()}
          >
            <MoreHorizontal className="chats-meta-more-icon" aria-hidden="true" />
          </DropdownMenuTrigger>
          <DropdownMenuContent align="end">
            <DropdownMenuItem
              onSelect={() => {
                setName(group.name);
                setRenaming(true);
              }}
            >
              Rename
            </DropdownMenuItem>
            {/* Ungroups its sessions, never archives them — see `useDeleteChatGroup`. */}
            <DropdownMenuItem onSelect={() => remove.mutate(group.id)}>Delete</DropdownMenuItem>
          </DropdownMenuContent>
        </DropdownMenu>
      </summary>
      {children}
    </details>
  );
}

/** Archived sessions, read only while the section is open, each with a way back. */
function ArchivedSection() {
  const [open, setOpen] = useState(false);
  const archived = useArchivedChats(open);
  const restore = useRestoreChat();
  const list = Array.isArray(archived.data) ? archived.data : [];

  return (
    <details className="chats-group" open={open}>
      <summary
        className="chats-group-head"
        onClick={(event) => {
          // Held in state rather than left to the element, so the query's `enabled` and the
          // disclosure cannot disagree about whether the section is open.
          event.preventDefault();
          setOpen((was) => !was);
        }}
      >
        <span className="chats-group-name-text">
          Archived sessions{archived.data !== undefined && open ? ` (${list.length})` : ""}
        </span>
      </summary>
      {open && archived.isError && <ErrorNote>archived sessions could not be read</ErrorNote>}
      {open && archived.data !== undefined && list.length === 0 && (
        <p className="chats-group-empty">Nothing archived.</p>
      )}
      {open && list.length > 0 && (
        <ul className="chats-list" aria-label="Archived sessions">
          {list.map((chat) => (
            <li key={chat.chat_id} className="chats-row chats-session-archived">
              <StateDot dot={null} chatId={chat.chat_id} />
              <span className="chats-row-title">{sessionTitle(chat)}</span>
              <button
                type="button"
                className="chats-session-restore"
                aria-label={`Restore ${sessionTitle(chat)}`}
                disabled={restore.isPending}
                onClick={() => restore.mutate(chat.chat_id)}
              >
                <ArchiveRestore className="chats-rail-icon" aria-hidden="true" />
                Restore
              </button>
            </li>
          ))}
        </ul>
      )}
    </details>
  );
}

function SessionRow({
  row,
  active,
  live,
  tabOpen,
  groups,
  onOpen,
}: {
  row: ChatSummary;
  active: boolean;
  live: boolean;
  tabOpen: boolean;
  groups: ChatGroup[];
  /** See `openingAChat`. Pressing a row is one of the three ways a conversation gets opened. */
  onOpen: () => void;
}) {
  const archive = useArchiveChat();
  const setGroup = useSetChatGroup();
  // A conversation with no name AND nothing said in it has no name to show; saying what is true of
  // it makes the empty ones one visibly different kind of row you can skim past.
  const said = row.title !== null || row.first_message !== null;
  const dot = dotFor(row, tabOpen);
  const name = sessionTitle(row);

  return (
    <li className={active ? "chats-row chats-row-active" : "chats-row"}>
      <Link
        className="chats-row-link"
        to={`/chats/${row.chat_id}`}
        aria-label={rowLabel(row, live)}
        aria-current={active ? "page" : undefined}
        /* Beside the navigation and not instead of it: the `<Link>` still does the routing, this
           only takes the editor preview down on the way. Pressing the row you are ALREADY on has
           to work too — that is the case an effect watching the id would sleep through. */
        onClick={onOpen}
      >
        <StateDot dot={dot} chatId={row.chat_id} />
        <span className={said ?"chats-row-title" : "chats-row-title chats-row-unsaid"}>
          {name}
        </span>
        {row.last_activity !== null && (
          <span className="chats-row-when" aria-hidden="true">
            <RelativeTime at={row.last_activity} />
          </span>
        )}
        {(row.notices_waiting ?? 0) > 0 && (
          <span
            className="chats-row-said"
            aria-hidden="true"
            title={`${row.notices_waiting} said by a team you set going`}
          >
            {row.notices_waiting}
          </span>
        )}
        {row.waiting > 0 && (
          <span
            className={
              (row.relayed_waiting ?? 0) > 0
                ? "chats-row-unread chats-row-unread-relayed"
                : "chats-row-unread"
            }
            aria-hidden="true"
            title={
              (row.relayed_waiting ?? 0) > 0
                ? `${row.relayed_waiting} handed over by another conversation`
                : undefined
            }
          >
            {row.waiting}
          </span>
        )}
      </Link>
      <DropdownMenu>
        <DropdownMenuTrigger
          className="chats-meta-more chats-session-more"
          aria-label={`Session actions for ${name}`}
        >
          <MoreHorizontal className="chats-meta-more-icon" aria-hidden="true" />
        </DropdownMenuTrigger>
        <DropdownMenuContent align="end">
          <DropdownMenuSub>
            <DropdownMenuSubTrigger>Move to group</DropdownMenuSubTrigger>
            <DropdownMenuSubContent>
              <DropdownMenuRadioGroup
                value={row.group_id == null ? "none" : String(row.group_id)}
                onValueChange={(value) =>
                  setGroup.mutate({
                    chatId: row.chat_id,
                    groupId: value === "none" ? null : Number(value),
                  })
                }
              >
                <DropdownMenuRadioItem value="none">Ungrouped</DropdownMenuRadioItem>
                {groups.map((group) => (
                  <DropdownMenuRadioItem key={group.id} value={String(group.id)}>
                    {group.name}
                  </DropdownMenuRadioItem>
                ))}
              </DropdownMenuRadioGroup>
            </DropdownMenuSubContent>
          </DropdownMenuSub>
          <DropdownMenuItem onSelect={() => archive.mutate(row.chat_id)}>
            <Archive aria-hidden="true" />
            Archive
          </DropdownMenuItem>
        </DropdownMenuContent>
      </DropdownMenu>
    </li>
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
  const going = stillGoing(session.last_activity, Date.now());
  return (
    <li className={active ? "chats-row chats-row-active" : "chats-row"}>
      <button
        type="button"
        className="chats-row-link chats-row-editor"
        aria-pressed={active}
        /* Spelled out: without it the mark, the name and "happening now" concatenate into one
           run-on word, which is the same trap the nav items and the chat rows already document. */
        aria-label={`In the editor: ${session.title ?? session.session_id}${
          going ? ", happening now" : ""
        }`}
        onClick={onOpen}
      >
        <SquareCode className="chats-row-mark" aria-hidden="true" />
        <span className="chats-row-title">{session.title ?? session.session_id}</span>
        {going && (
          <span className="chats-row-live" aria-hidden="true">
            now
          </span>
        )}
        {!going && (
          <span className="chats-row-when" aria-hidden="true">
            <RelativeTime at={session.last_activity} />
          </span>
        )}
      </button>
    </li>
  );
}
