import { useEffect, useLayoutEffect, useRef, useState, type ReactNode } from "react";
import { keys } from "../../data/keys";
import type { Entry } from "../../data/files";

/* --------------------------------------------------------- context menu -- */

/** How far from the window edge the menu is held, so it never opens half off-screen. */
const MENU_MARGIN = 8;

/** One verb in the row menu, with the key that does the same from the listing, if one does. */
function MenuItem({
  primary = false,
  danger = false,
  keys,
  onClick,
  children,
}: {
  primary?: boolean;
  danger?: boolean;
  keys?: string;
  onClick: () => void;
  children: ReactNode;
}) {
  const classes = ["fi-context-item"];
  if (primary) classes.push("fi-context-item-default");
  if (danger) classes.push("fi-context-item-danger");
  return (
    <button
      type="button"
      role="menuitem"
      tabIndex={-1}
      className={classes.join(" ")}
      aria-keyshortcuts={keys === "Del" ? "Delete" : keys}
      onClick={onClick}
    >
      <span>{children}</span>
      {keys !== undefined && (
        <kbd className="fi-context-keys" aria-hidden="true">
          {keys}
        </kbd>
      )}
    </button>
  );
}

/**
 * The row's actions, reachable from the keyboard as well as the mouse.
 *
 * `role="menu"` carries a contract, and this keeps it: focus moves into the
 * menu when it opens, the arrows and Home/End walk it, Escape closes it and
 * hands focus back to the row it was opened from, and Tab leaves it rather
 * than wandering into the page behind. The Sidebar's switcher chose a
 * disclosure instead to avoid exactly this work; a context menu has no such
 * way out, since a list of verbs on a row is what a menu is.
 */
export function ContextMenu({
  x,
  y,
  entry,
  count,
  canRename,
  onClose,
  onOpen,
  onDownload,
  onRename,
  onMove,
  onCopyPath,
  onDelete,
}: {
  x: number;
  y: number;
  entry: Entry;
  /** How many entries Copy path, Move and Delete act on — more than one when the row is part of a selection. */
  count: number;
  /** False on a search hit: the rename field lives in the table's row. */
  canRename: boolean;
  onClose: (returnFocus: boolean) => void;
  onOpen: () => void;
  onDownload: () => void;
  onRename: () => void;
  onMove: () => void;
  onCopyPath: () => void;
  onDelete: () => void;
}) {
  const menuRef = useRef<HTMLDivElement>(null);
  const many = count > 1;
  const [position, setPosition] = useState({ left: x, top: y });

  // Measured before paint and pulled back inside the window: at 800x600 a
  // right-click near the bottom opened the menu below the visible edge.
  useLayoutEffect(() => {
    const menu = menuRef.current;
    if (menu === null) return;
    const { width, height } = menu.getBoundingClientRect();
    setPosition({
      left: Math.max(MENU_MARGIN, Math.min(x, window.innerWidth - width - MENU_MARGIN)),
      top: Math.max(MENU_MARGIN, Math.min(y, window.innerHeight - height - MENU_MARGIN)),
    });
  }, [x, y]);

  // Through a ref: the page hands a fresh `onClose` on every render, and the
  // listing polls, so an effect keyed on it re-ran — and re-focused the first
  // item — every few seconds while a person was choosing.
  const onCloseRef = useRef(onClose);
  useEffect(() => {
    onCloseRef.current = onClose;
  });
  useEffect(() => {
    menuRef.current?.querySelector<HTMLElement>('[role="menuitem"]')?.focus();
    function handlePointerDown() {
      onCloseRef.current(false);
    }
    // The menu is `position: fixed` at the spot it was opened: a scroll under
    // it, a resize, or the window losing focus would leave it floating over the
    // wrong row. None of them hands focus back to the opener, as Escape does.
    function handleDrift(event: Event) {
      if (event.target instanceof Node && menuRef.current?.contains(event.target)) return;
      onCloseRef.current(false);
    }
    document.addEventListener("mousedown", handlePointerDown);
    document.addEventListener("scroll", handleDrift, true);
    window.addEventListener("resize", handleDrift);
    window.addEventListener("blur", handleDrift);
    return () => {
      document.removeEventListener("mousedown", handlePointerDown);
      document.removeEventListener("scroll", handleDrift, true);
      window.removeEventListener("resize", handleDrift);
      window.removeEventListener("blur", handleDrift);
    };
  }, []);

  function handleKeyDown(event: React.KeyboardEvent<HTMLDivElement>) {
    // Kept inside the menu: the table under it listens for the same keys.
    event.stopPropagation();
    const items = Array.from(menuRef.current?.querySelectorAll<HTMLElement>('[role="menuitem"]') ?? []);
    const current = items.indexOf(document.activeElement as HTMLElement);
    const focusAt = (index: number) => items[(index + items.length) % items.length]?.focus();
    if (event.key === "ArrowDown") {
      event.preventDefault();
      focusAt(current + 1);
    } else if (event.key === "ArrowUp") {
      event.preventDefault();
      focusAt(current - 1);
    } else if (event.key === "Home") {
      event.preventDefault();
      focusAt(0);
    } else if (event.key === "End") {
      event.preventDefault();
      focusAt(items.length - 1);
    } else if (event.key === "Escape" || event.key === "Tab") {
      event.preventDefault();
      onClose(true);
    } else if (event.key.length === 1 && event.key !== " " && !event.ctrlKey && !event.metaKey && !event.altKey) {
      // Typeahead: a letter jumps to the next item whose text starts with it,
      // wrapping, as a Windows menu does.
      const letter = event.key.toLowerCase();
      for (let step = 1; step <= items.length; step += 1) {
        const item = items[(current + step + items.length) % items.length];
        if (item?.textContent?.trim().toLowerCase().startsWith(letter)) {
          event.preventDefault();
          item.focus();
          break;
        }
      }
    }
  }

  return (
    <div
      ref={menuRef}
      className="fi-context-menu"
      role="menu"
      aria-label={count > 1 ? `Actions for ${String(count)} selected items` : `Actions for ${entry.name}`}
      style={{ left: position.left, top: position.top }}
      onMouseDown={(event) => event.stopPropagation()}
      onKeyDown={handleKeyDown}
    >
      {/* One scope at a time, as Explorer's menu is. Inside a selection it
          offers only what acts on the whole of it; on a single row, the row's
          verbs and then the rest below a rule. Both scopes at once was six
          items under two captions, Rename two lines above a red "Delete 2". */}
      {many ? (
        <div role="group" aria-label={`${String(count)} selected`} className="fi-context-group">
          <span className="fi-context-caption" aria-hidden="true">
            {count} selected
          </span>
          <MenuItem onClick={onCopyPath}>Copy {count} full paths</MenuItem>
          <MenuItem onClick={onMove}>Move {count}…</MenuItem>
          <MenuItem danger keys="Del" onClick={onDelete}>
            Delete {count}
          </MenuItem>
        </div>
      ) : (
        <>
          <div role="group" className="fi-context-group">
            <MenuItem primary keys="Enter" onClick={onOpen}>
              {entry.is_dir ? "Open" : "Open in Windows"}
            </MenuItem>
            {!entry.is_dir && <MenuItem onClick={onDownload}>Download a copy</MenuItem>}
            {canRename && (
              <MenuItem keys="F2" onClick={onRename}>
                Rename
              </MenuItem>
            )}
          </div>
          <div role="separator" className="fi-context-sep" />
          <div role="group" className="fi-context-group">
            <MenuItem onClick={onCopyPath}>Copy full path</MenuItem>
            <MenuItem onClick={onMove}>Move…</MenuItem>
            <MenuItem danger keys="Del" onClick={onDelete}>
              Delete
            </MenuItem>
          </div>
        </>
      )}
    </div>
  );
}
