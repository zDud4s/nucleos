import { useEffect, useId, useLayoutEffect, useRef, useState, type RefObject } from "react";
import { File as FileIcon, Folder as FolderIcon } from "lucide-react";
import { formatBytes, joinPath, middleEllipsis, useFolder, type Entry, type SortColumn, type SortDirection } from "../../data/files";
import { Button, Panel, Quiet, RelativeTime, Skeleton, StaleNote, Teach } from "../../ui";
import { LIST_SENTENCES, MOVE_SENTENCES, RefusalOrError, remPx } from "./shared";
import type { FocusAfter } from "./pageParts";

/* --------------------------------------------------------------- table -- */

interface FileTableProps {
  tableRef: RefObject<HTMLDivElement | null>;
  path: string;
  folder: ReturnType<typeof useFolder>;
  /** The listing in the order on screen — the page sorts it once and reads it too. */
  ordered: Entry[];
  sort: { column: SortColumn; direction: SortDirection };
  onSort: (column: SortColumn) => void;
  selected: Set<string>;
  onToggleSelect: (name: string) => void;
  onAddToSelection: (names: string[]) => void;
  onSelectAll: (names: string[]) => void;
  focusIndex: number | null;
  onFocusRow: (index: number) => void;
  focusAfter: FocusAfter | null;
  onFocusLanded: () => void;
  renaming: { path: string; name: string } | null;
  /** Why the last rename was refused — said under the field it came from, not above the table. */
  renameError: unknown;
  onStartRename: (entry: Entry) => void;
  onSubmitRename: (from: string, name: string) => void;
  onCancelRename: () => void;
  onOpen: (entry: Entry) => void;
  onContextMenu: (entry: Entry, x: number, y: number, opener: HTMLElement | null) => void;
  onDelete: (names: string[]) => void;
  onGoUp: () => void;
  /** The rows a Ctrl+X is holding, when they are in this folder. */
  cutNames: Set<string> | null;
  onCancelCut: () => void;
  onDragStart: (entry: Entry, event: React.PointerEvent) => void;
}

const COLUMN_LABEL: Record<SortColumn, string> = { name: "Name", size: "Size", modified: "Modified" };

/** The name button of row `index` — the element a row's keyboard focus actually lands on. */
function rowButton(container: HTMLElement | null, index: number): HTMLElement | null {
  return container?.querySelector<HTMLElement>(`[data-row-open="${String(index)}"]`) ?? null;
}

export function FileTable(props: FileTableProps) {
  const {
    tableRef,
    path,
    folder,
    ordered,
    sort,
    onSort,
    selected,
    onToggleSelect,
    onAddToSelection,
    onSelectAll,
    focusIndex,
    onFocusRow,
    focusAfter,
    onFocusLanded,
    renaming,
    renameError,
    onStartRename,
    onSubmitRename,
    onCancelRename,
    onOpen,
    onContextMenu,
    onDelete,
    onGoUp,
    cutNames,
    onCancelCut,
    onDragStart,
  } = props;

  const [anchor, setAnchor] = useState<number | null>(null);
  const selectAllRef = useRef<HTMLInputElement>(null);
  const keysId = useId();
  const [nameHead, setNameHead] = useState<HTMLTableCellElement | null>(null);
  const nameMax = useNameFit(nameHead);

  const stale = folder.isError && folder.data !== undefined;
  const entries = folder.data;
  // One Tab stop for the whole listing — the row the keyboard was last on, the
  // first before that. Every row's checkbox and name were both in the tab
  // order, so leaving a sixty-row folder took a hundred and twenty presses for
  // what the arrows already do.
  const tabStop = focusIndex !== null && focusIndex < ordered.length ? focusIndex : 0;
  const selectedHere = ordered.filter((entry) => selected.has(entry.name)).length;

  // "Some but not all" is a third state a checkbox can show and a screen
  // reader can say ("mixed"); without it a partial selection read as none.
  useEffect(() => {
    if (selectAllRef.current !== null) {
      selectAllRef.current.indeterminate = selectedHere > 0 && selectedHere < ordered.length;
    }
  }, [selectedHere, ordered.length]);

  /**
   * Moves the keyboard to a row for real. The row used to be only painted as
   * focused while DOM focus stayed on the wrapper, so a screen reader heard
   * nothing as the arrows went down the list; now the row's own name button
   * takes focus, and is read by its name.
   */
  function goTo(index: number) {
    onFocusRow(index);
    rowButton(tableRef.current, index)?.focus();
  }

  // After a delete, the neighbour takes focus once the listing no longer holds
  // what was deleted — before then its index is the old one, and focus would
  // land a row off once the refetch closed the gap.
  useEffect(() => {
    if (focusAfter === null) return;
    // Not read yet: a "Show in folder" arrives before the folder's listing does.
    if (entries === undefined) return;
    if (ordered.some((entry) => focusAfter.gone.includes(entry.name))) return;
    const index = ordered.findIndex((entry) => entry.name === focusAfter.name);
    if (index >= 0) goTo(index);
    else tableRef.current?.focus();
    onFocusLanded();
    // `goTo` and the callbacks are fresh every render; the listing is the trigger.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [focusAfter, ordered]);

  function selectRange(from: number, to: number) {
    const [low, high] = from <= to ? [from, to] : [to, from];
    onAddToSelection(ordered.slice(low, high + 1).map((entry) => entry.name));
  }

  function openMenuFor(index: number) {
    const entry = ordered[index];
    const button = rowButton(tableRef.current, index);
    if (entry === undefined) return;
    const rect = button?.getBoundingClientRect();
    onContextMenu(entry, rect?.left ?? 0, rect?.bottom ?? 0, button);
  }

  function handleKeyDown(event: React.KeyboardEvent<HTMLDivElement>) {
    const target = event.target as HTMLElement;
    // A key pressed ON a control inside the table is that control's first. Enter
    // on a name button already opens that row through its own click, and the
    // wrapper opening the focused row as well meant one keypress, two opens.
    const onControl = target !== event.currentTarget && (target.tagName === "BUTTON" || target.tagName === "INPUT");
    const onField = target.tagName === "INPUT" && (target as HTMLInputElement).type !== "checkbox";

    if (event.key === "Escape") {
      onCancelRename();
      onCancelCut();
      return;
    }
    if (renaming !== null || onField) return;

    if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === "a") {
      event.preventDefault();
      onSelectAll(ordered.map((entry) => entry.name));
      return;
    }
    if (event.key === "ArrowDown" || event.key === "ArrowUp" || event.key === "Home" || event.key === "End") {
      if (ordered.length === 0) return;
      event.preventDefault();
      const last = ordered.length - 1;
      const next =
        event.key === "Home"
          ? 0
          : event.key === "End"
            ? last
            : event.key === "ArrowDown"
              ? Math.min(last, (focusIndex ?? -1) + 1)
              : Math.max(0, (focusIndex ?? 0) - 1);
      if (event.shiftKey) selectRange(anchor ?? focusIndex ?? next, next);
      else setAnchor(next);
      goTo(next);
      return;
    }
    if (event.key === " " && target.tagName !== "INPUT") {
      // Space picks the row, the way it does in every file list — never
      // activates the name button under it.
      event.preventDefault();
      const entry = focusIndex === null ? undefined : ordered[focusIndex];
      if (entry !== undefined) {
        onToggleSelect(entry.name);
        setAnchor(focusIndex);
      }
      return;
    }
    if (event.key === "Enter") {
      if (onControl) return;
      const entry = focusIndex === null ? undefined : ordered[focusIndex];
      if (entry !== undefined) onOpen(entry);
      return;
    }
    if (event.key === "F2") {
      const entry = focusIndex === null ? undefined : ordered[focusIndex];
      if (entry !== undefined) onStartRename(entry);
      return;
    }
    if (event.key === "ContextMenu" || (event.shiftKey && event.key === "F10")) {
      event.preventDefault();
      if (focusIndex !== null) openMenuFor(focusIndex);
      return;
    }
    if (event.key === "Backspace") {
      if (path !== "") onGoUp();
      return;
    }
    if (event.key === "Delete") {
      // A held key auto-repeats, and each repeat would be one more item gone.
      if (event.repeat) return;
      // The selection if there is one, otherwise the row the keyboard is on.
      // One press: the entry goes to the trash with an undo beside it.
      const focused = focusIndex === null ? undefined : ordered[focusIndex];
      const names = selected.size > 0 ? [...selected] : focused !== undefined ? [focused.name] : [];
      if (names.length > 0) onDelete(names);
    }
  }

  return (
    // eslint-disable-next-line jsx-a11y/no-noninteractive-tabindex -- the
    // shortcuts below (arrows, Enter, F2, Delete) need somewhere to land, and
    // a `<table>` is not itself focusable. Out of the tab order once there are
    // rows: the row's own name is then the stop, and the wrapper was a second
    // one in front of it that did nothing a row does not.
    <div
      ref={tableRef}
      className="fi-table-wrap"
      tabIndex={ordered.length > 0 ? -1 : 0}
      // A group, so the name and the description count: on a bare `div` ARIA
      // allows neither, and both were dropped. The keys are the description,
      // not the label — twenty words of shortcuts read before every row, on
      // every visit. On screen they show while the keyboard is in the listing.
      role="group"
      aria-label="Folder contents"
      aria-describedby={ordered.length > 0 ? keysId : undefined}
      onKeyDown={handleKeyDown}
      onKeyUp={(event) => {
        // A button fires its click on Space's keyup. The keydown above already
        // turned Space into "select", so the keyup must not also open the row.
        if (event.key === " " && (event.target as HTMLElement).tagName === "BUTTON") event.preventDefault();
      }}
    >
      {/* No title: the path bar above already names what this is, and "Contents"
          was one more line between the toolbar and the first row. Flat: the
          card round the rows was padding on every side of a table that is the
          page's whole subject, and a second box beside the tree's. */}
      <Panel variant="flat">
        {stale && <StaleNote dataUpdatedAt={folder.dataUpdatedAt} />}
        {folder.isError && entries === undefined && (
          <RefusalOrError error={folder.error} sentences={LIST_SENTENCES} what="nothing is known about this folder" />
        )}
        {!folder.isError && entries === undefined && <Skeleton label="Reading the folder…" lines={4} />}
        {/* `Teach` is for a place nothing has ever been — the root, empty. A
            subfolder that is empty is an ordinary answer, and says so in one line. */}
        {entries !== undefined && entries.length === 0 && path === "" && (
          <Teach title="Nothing has been filed yet">
            <p>
              Upload a file, make a folder, or drag something in from Windows — a drop lands under the folder you
              are looking at.
            </p>
          </Teach>
        )}
        {entries !== undefined && entries.length === 0 && path !== "" && (
          <Quiet says="Nothing in this folder yet — upload, drop or make a folder here." />
        )}

        {/* `fi-table-picking` once anything is picked: until then the row boxes
            stay hidden except under the pointer or the keyboard. A click on the
            row already picks it, and seven empty squares down the edge were the
            loudest thing on a page at rest. */}
        {ordered.length > 0 && (
          <table className={selectedHere > 0 ? "fi-table fi-table-picking" : "fi-table"}>
            <thead>
              <tr>
                <th scope="col" className="fi-col-select">
                  <input
                    ref={selectAllRef}
                    type="checkbox"
                    aria-label="Select all"
                    checked={ordered.length > 0 && selectedHere === ordered.length}
                    onChange={(event) => onSelectAll(event.target.checked ? ordered.map((entry) => entry.name) : [])}
                  />
                </th>
                <SortHeader column="name" sort={sort} onSort={onSort} cellRef={setNameHead} />
                <SortHeader column="size" sort={sort} onSort={onSort} />
                <SortHeader column="modified" sort={sort} onSort={onSort} />
              </tr>
            </thead>
            <tbody>
              {ordered.map((entry, index) => (
                <FileRow
                  key={entry.name}
                  entry={entry}
                  index={index}
                  tabStop={tabStop === index}
                  focused={focusIndex === index}
                  selected={selected.has(entry.name)}
                  anySelected={selected.size > 0}
                  cut={cutNames?.has(entry.name) === true}
                  dropPath={entry.is_dir ? joinPath(path, entry.name) : undefined}
                  onPointerDown={(event) => onDragStart(entry, event)}
                  renaming={renaming?.path === joinPath(path, entry.name) ? renaming : null}
                  renameError={renameError}
                  nameMax={nameMax}
                  onToggleSelect={(extend) => {
                    onFocusRow(index);
                    if (extend && anchor !== null) {
                      selectRange(anchor, index);
                    } else {
                      onToggleSelect(entry.name);
                      setAnchor(index);
                    }
                  }}
                  onRowClick={({ toggle, extend }) => {
                    goTo(index);
                    // Explorer's three: a click picks this row alone, Ctrl adds or
                    // drops it, Shift takes the run from the last row picked —
                    // instead of the selection, or on top of it with Ctrl.
                    if (extend && anchor !== null) {
                      const [low, high] = anchor <= index ? [anchor, index] : [index, anchor];
                      const run = ordered.slice(low, high + 1).map((item) => item.name);
                      if (toggle) onAddToSelection(run);
                      else onSelectAll(run);
                      return;
                    }
                    if (toggle) onToggleSelect(entry.name);
                    else onSelectAll([entry.name]);
                    setAnchor(index);
                  }}
                  onFocus={() => onFocusRow(index)}
                  onOpen={() => {
                    onFocusRow(index);
                    onOpen(entry);
                  }}
                  onContextMenu={(x, y, opener) => {
                    onFocusRow(index);
                    onContextMenu(entry, x, y, opener);
                  }}
                  onSubmitRename={(name) => onSubmitRename(joinPath(path, entry.name), name)}
                  onCancelRename={onCancelRename}
                />
              ))}
            </tbody>
          </table>
        )}
        {ordered.length > 0 && (
          <p id={keysId} className="fi-keys">
            <kbd>↑</kbd> <kbd>↓</kbd> move · <kbd>Shift</kbd>+<kbd>↑</kbd> <kbd>↓</kbd> extend · <kbd>Space</kbd>{" "}
            select · <kbd>Ctrl</kbd>+<kbd>A</kbd> all · <kbd>Enter</kbd> open in Windows · <kbd>F2</kbd> rename ·{" "}
            <kbd>Delete</kbd> to the trash · <kbd>Ctrl</kbd>+<kbd>X</kbd>, then <kbd>Ctrl</kbd>+<kbd>V</kbd> in another
            folder, move · <kbd>Ctrl</kbd>+<kbd>Z</kbd> undo · <kbd>Backspace</kbd> up ·{" "}
            <kbd>Shift</kbd>+<kbd>F10</kbd> more
          </p>
        )}
      </Panel>
    </div>
  );
}

function SortHeader({
  column,
  sort,
  onSort,
  cellRef,
}: {
  column: SortColumn;
  sort: { column: SortColumn; direction: SortDirection };
  onSort: (column: SortColumn) => void;
  cellRef?: (cell: HTMLTableCellElement | null) => void;
}) {
  const active = sort.column === column;
  const ariaSort: "ascending" | "descending" | "none" = !active ? "none" : sort.direction === "asc" ? "ascending" : "descending";
  // Every head carries a mark, so the ones not sorting still read as
  // controls; the idle one is faint until the pointer is on it.
  const mark = (
    <span aria-hidden="true" className={active ? "fi-sort-mark" : "fi-sort-mark fi-sort-idle"}>
      {active ? (sort.direction === "asc" ? "▲" : "▼") : "↕"}
    </span>
  );
  return (
    <th ref={cellRef} scope="col" aria-sort={ariaSort} className={column === "name" ? "fi-col-name" : "fi-col-num"}>
      <button type="button" className="fi-sort-button" onClick={() => onSort(column)}>
        {/* A number's head keeps its mark on the left, so the label's right
            edge is the column's: with the mark after it, SIZE and MODIFIED
            ended a mark's width short of the figures under them. */}
        {column !== "name" && mark}
        {COLUMN_LABEL[column]}
        {column === "name" && mark}
      </button>
    </th>
  );
}

/** How many characters of a name the row shows before cutting it in the middle. */
const NAME_MAX = 56;

/** Fewer than this and the middle cut leaves too little of either end to recognise. */
const NAME_MIN = 12;

/** The name cell's padding, the kind glyph and the gap before the name, in rem. */
const NAME_CHROME_REM = 2.5;

let glyphCanvas: HTMLCanvasElement | null = null;

/**
 * How many characters of a name fit the name column as it is now drawn. The
 * cut happens in the middle and keeps the extension, but only up to a fixed
 * 56: in a narrower column the CSS end-ellipsis took over, and "roster-ex…"
 * lost the one part that says what kind of file it is. The names are mono, so
 * one glyph's advance measures them all. No `ResizeObserver` (jsdom) keeps 56.
 */
function useNameFit(cell: HTMLElement | null): number {
  const [fit, setFit] = useState(NAME_MAX);
  useLayoutEffect(() => {
    if (cell === null || typeof ResizeObserver === "undefined") return;
    const measure = () => {
      const name = cell.closest("table")?.querySelector<HTMLElement>(".fi-entry-name");
      if (name === null || name === undefined) return;
      glyphCanvas ??= document.createElement("canvas");
      const context = glyphCanvas.getContext("2d");
      if (context === null) return;
      const style = getComputedStyle(name);
      context.font = `${style.fontWeight} ${style.fontSize} ${style.fontFamily}`;
      const glyph = context.measureText("0").width;
      if (glyph <= 0) return;
      const room = cell.clientWidth - NAME_CHROME_REM * remPx() - glyph;
      setFit(Math.max(NAME_MIN, Math.min(NAME_MAX, Math.floor(room / glyph))));
    };
    const observer = new ResizeObserver(measure);
    observer.observe(cell);
    return () => observer.disconnect();
  }, [cell]);
  return fit;
}

/** The tooltip on a name: the whole of it when the row shows it cut, and how a file opens. */
function fileTitle(entry: { name: string; is_dir: boolean }, shown: string): string | undefined {
  const full = shown === entry.name ? null : entry.name;
  if (entry.is_dir) return full ?? undefined;
  return full === null ? "Double-click to open in Windows" : `${full} — double-click to open in Windows`;
}

function FileRow({
  entry,
  index,
  tabStop,
  focused,
  selected,
  anySelected,
  cut,
  dropPath,
  onPointerDown,
  renaming,
  renameError,
  nameMax,
  onToggleSelect,
  onRowClick,
  onFocus,
  onOpen,
  onContextMenu,
  onSubmitRename,
  onCancelRename,
}: {
  entry: Entry;
  index: number;
  /** Whether this row's name is the listing's one Tab stop. */
  tabStop: boolean;
  focused: boolean;
  selected: boolean;
  anySelected: boolean;
  /** Held by a Ctrl+X: drawn faint until the paste lands. */
  cut: boolean;
  /** A folder row's path, which a dragged row can be let go on. */
  dropPath: string | undefined;
  onPointerDown: (event: React.PointerEvent) => void;
  renaming: { path: string; name: string } | null;
  renameError: unknown;
  /** How many characters of a name the column has room for. */
  nameMax: number;
  onToggleSelect: (extend: boolean) => void;
  /** A mouse click on the row outside its controls: `toggle` is Ctrl, `extend` is Shift. */
  onRowClick: (keys: { toggle: boolean; extend: boolean }) => void;
  onFocus: () => void;
  onOpen: () => void;
  onContextMenu: (x: number, y: number, opener: HTMLElement | null) => void;
  onSubmitRename: (name: string) => void;
  onCancelRename: () => void;
}) {
  // A folder's trailing slash takes one of the characters the column has.
  const shown = middleEllipsis(entry.name, nameMax - (entry.is_dir ? 1 : 0));
  const classes = ["fi-row"];
  if (focused) classes.push("fi-row-focus");
  if (selected) classes.push("fi-row-selected");
  if (cut) classes.push("fi-row-cut");
  // The name button is what focus lands on, so it is what says the row's
  // state: its whole name even when the screen shows it cut, whether it is a
  // folder (the eye gets that from the icon and the slash), and whether it is
  // picked, which a checkbox one cell away never told anyone arrowing down.
  // "not selected" only once something is: before that it is noise on every row.
  const spoken = [
    entry.name,
    entry.is_dir ? "folder" : null,
    cut ? "cut" : null,
    selected ? "selected" : anySelected ? "not selected" : null,
  ]
    .filter((part): part is string => part !== null)
    .join(", ");
  return (
    // A row picks itself on a click, as a row in Explorer does: the checkbox was
    // the only way in, a small target a cell away from the name a person aims at.
    // The keyboard has Space for the same thing, so the row needs no role of its own.
    // eslint-disable-next-line jsx-a11y/click-events-have-key-events, jsx-a11y/no-noninteractive-element-interactions
    <tr
      className={classes.join(" ")}
      data-drop-path={dropPath}
      onPointerDown={onPointerDown}
      onClick={(event) => {
        const target = event.target as HTMLElement;
        // Only a plain single click: the second of a double-click is the open,
        // and a click the keyboard made (`detail` 0) belongs to the control it
        // landed on. The checkbox and the rename field answer for themselves,
        // and a folder's name opens the folder.
        if (event.detail !== 1) return;
        if (target.closest("input, form") !== null) return;
        const toggle = event.ctrlKey || event.metaKey;
        if (entry.is_dir && !toggle && !event.shiftKey && target.closest("[data-row-open]") !== null) return;
        onRowClick({ toggle, extend: event.shiftKey });
      }}
      onContextMenu={(event) => {
        event.preventDefault();
        const opener = event.currentTarget.querySelector<HTMLElement>("[data-row-open]");
        onContextMenu(event.clientX, event.clientY, opener);
      }}
    >
      <td className="fi-col-select">
        {/* Out of the tab order: Space on the row picks it, and a second stop
            per row is what made the listing a wall to Tab through. */}
        <input
          type="checkbox"
          tabIndex={-1}
          aria-label={`Select ${entry.name}`}
          checked={selected}
          // Shift on the click extends from the last row picked, as a file list does.
          onChange={(event) => onToggleSelect((event.nativeEvent as MouseEvent).shiftKey)}
        />
      </td>
      <td className="fi-col-name">
        {renaming !== null ? (
          <RenameForm initialName={renaming.name} error={renameError} onSubmit={onSubmitRename} onCancel={onCancelRename} />
        ) : (
          <span className="fi-name-cell">
            {entry.is_dir ? (
              <FolderIcon className="fi-kind fi-kind-dir" strokeWidth={1.5} aria-hidden="true" />
            ) : (
              <FileIcon className="fi-kind" strokeWidth={1.5} aria-hidden="true" />
            )}
            {/* A folder opens on one click — it stays in this window. A file
                opens on a double-click or Enter, as in Explorer: it hands focus
                to another app, and one click meant to pick a row beside an IDE
                threw Acrobat in front of both; that click picks the row instead.
                `detail` is 0 on a click the keyboard made, so Enter still opens
                through here. */}
            <Button
              variant="link"
              data-row-open={index}
              tabIndex={tabStop ? 0 : -1}
              aria-label={spoken}
              title={fileTitle(entry, shown)}
              onFocus={onFocus}
              onClick={(event) => {
                // Ctrl or Shift on a folder's name adds it to the selection, as
                // everywhere else on the row: it went into the folder, and the
                // way in threw away every row picked so far.
                if (event.ctrlKey || event.metaKey || event.shiftKey) return;
                if (entry.is_dir || event.detail === 0) onOpen();
              }}
              onDoubleClick={() => {
                if (!entry.is_dir) onOpen();
              }}
            >
              <span className={entry.is_dir ? "fi-entry-name fi-entry-dir" : "fi-entry-name"}>
                {shown}
                {entry.is_dir ? "/" : ""}
              </span>
            </Button>
          </span>
        )}
      </td>
      {/* A dash, not a blank: a folder's size is left out on purpose, and an
          empty cell reads as a figure that failed to load. */}
      <td className="fi-col-num">{entry.is_dir ? <span title="not measured for folders">—</span> : formatBytes(entry.size_bytes)}</td>
      <td className="fi-col-num">
        {entry.modified === null ? <span title="the disk did not say when">—</span> : <RelativeTime at={entry.modified} />}
      </td>
    </tr>
  );
}

/**
 * The name field, in the row it renames. A refusal is said under it and tied
 * to it: it used to land above the table, a screen away from the field a
 * person was still typing in, and focus never went to find it.
 */
function RenameForm({
  initialName,
  error,
  onSubmit,
  onCancel,
}: {
  initialName: string;
  error: unknown;
  onSubmit: (name: string) => void;
  onCancel: () => void;
}) {
  const [value, setValue] = useState(initialName);
  const errorId = useId();
  const refused = error !== null && error !== undefined;
  return (
    <div className="fi-rename-wrap">
      <form
        className="fi-rename"
        onSubmit={(event) => {
          event.preventDefault();
          const trimmed = value.trim();
          if (trimmed !== "") onSubmit(trimmed);
        }}
      >
        <input
          // eslint-disable-next-line jsx-a11y/no-autofocus -- F2 and the context
          // menu's Rename both open this form to be typed into immediately.
          autoFocus
          value={value}
          aria-label="New name"
          aria-invalid={refused || undefined}
          aria-describedby={refused ? errorId : undefined}
          onChange={(event) => setValue(event.target.value)}
        />
        <Button type="submit">Rename</Button>
        <Button variant="ghost" onClick={onCancel}>
          Cancel
        </Button>
      </form>
      {refused && (
        <div id={errorId}>
          <RefusalOrError error={error} sentences={MOVE_SENTENCES} what="that rename did not go through" />
        </div>
      )}
    </div>
  );
}
