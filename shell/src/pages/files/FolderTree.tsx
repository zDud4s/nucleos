import { useEffect, useState, type CSSProperties } from "react";
import { DAEMON_NAME } from "../../copy";
import { joinPath, pathSegments, useFolder } from "../../data/files";
import { Button, Inset } from "../../ui";

/* ---------------------------------------------------------------- tree -- */

export function FolderTree({
  currentPath,
  onSelect,
  label = "Folders",
  marked = true,
  herePath,
  dropTargets = false,
}: {
  currentPath: string;
  onSelect: (path: string, byKey: boolean) => void;
  /** Whether a dragged row can be let go on its folders — the side tree's, not the move form's copy. */
  dropTargets?: boolean;
  /** The landmark's name — the side column's tree, or the one the move form picks from. */
  label?: string;
  /** Whether `currentPath` is drawn as chosen. The move form's tree opens on it without it being an answer yet. */
  marked?: boolean;
  /** A folder to say "(here)" beside — where the move form's items already are. */
  herePath?: string;
}) {
  return (
    // The `nav` wraps the `Inset` rather than wearing it: `Inset` renders a
    // `div` or an `li` and is deliberately not widened, and the landmark with
    // its label is what tells a screen reader this column is the folder tree.
    // `--fi-deepest` is how deep the open path runs, so the indent can share
    // the column between that many steps and still leave the names room.
    <nav aria-label={label} style={{ "--fi-deepest": Math.max(3, pathSegments(currentPath).length) } as CSSProperties}>
      <Inset className="fi-tree">
        {/* A tree, walked with the arrows the way Explorer's is, and one Tab
            stop — the folder on screen. It was a column of buttons, two stops
            per folder (the arrow and the name), and twice that again with the
            move form's copy open beside it. */}
        <div role="tree" aria-label={label} onKeyDown={handleTreeKey}>
          <TreeNode
            path=""
            label="Files"
            currentPath={currentPath}
            marked={marked}
            herePath={herePath}
            onSelect={onSelect}
            dropTargets={dropTargets}
            depth={0}
          />
        </div>
      </Inset>
    </nav>
  );
}

/**
 * The tree's keys, read off the DOM: the items on screen are the visible ones,
 * since a folded branch renders no children. Up and Down walk them, Right opens
 * a folded folder or steps into an open one, Left folds or steps out, Enter and
 * Space go to the folder. Expanding and choosing go through the same buttons
 * the pointer uses, so there is one path for each.
 */
// True only while handleTreeKey forwards Enter/Space to a folder button, so
// the select handler can tell a keyboard pick from a pointer click (the click
// runs synchronously inside `.click()`).
let keyboardPick = false;

function handleTreeKey(event: React.KeyboardEvent<HTMLDivElement>) {
  const item = (event.target as HTMLElement).closest<HTMLElement>('[role="treeitem"]');
  if (item === null) return;
  const items = Array.from(event.currentTarget.querySelectorAll<HTMLElement>('[role="treeitem"]'));
  const at = items.indexOf(item);
  const expanded = item.getAttribute("aria-expanded");
  const toggle = item.querySelector<HTMLElement>(":scope > .fi-tree-row > .fi-tree-toggle");
  let next: HTMLElement | null | undefined;
  switch (event.key) {
    case "ArrowDown":
      next = items[at + 1];
      break;
    case "ArrowUp":
      next = items[at - 1];
      break;
    case "Home":
      next = items[0];
      break;
    case "End":
      next = items[items.length - 1];
      break;
    case "ArrowRight":
      if (expanded === "false") toggle?.click();
      else if (expanded === "true") next = item.querySelector<HTMLElement>(':scope > .fi-tree-children > [role="treeitem"]');
      break;
    case "ArrowLeft":
      if (expanded === "true" && toggle !== null) toggle.click();
      else next = item.parentElement?.closest<HTMLElement>('[role="treeitem"]');
      break;
    case "Enter":
    case " ":
      keyboardPick = true;
      try {
        item.querySelector<HTMLElement>(":scope > .fi-tree-row [data-tree-select]")?.click();
      } finally {
        keyboardPick = false;
      }
      break;
    default:
      return;
  }
  event.preventDefault();
  event.stopPropagation();
  next?.focus();
}

/** Whether `path` is `currentPath` itself or a folder above it — the branch the tree must show open. */
function isOnTheWay(path: string, currentPath: string): boolean {
  return path === "" || currentPath === path || currentPath.startsWith(`${path}/`);
}

/**
 * One branch of the tree, lazy: a node's own children are asked for only once
 * it is open. The root is open from the start so the first screen is not
 * empty; every folder under it starts collapsed — except the ones on the way
 * to the folder on screen, which open by themselves, so arriving from the
 * table or the breadcrumbs still leaves the tree saying where you are.
 */
function TreeNode({
  path,
  label,
  currentPath,
  marked,
  herePath,
  onSelect,
  dropTargets,
  depth,
}: {
  path: string;
  label: string;
  currentPath: string;
  marked: boolean;
  herePath: string | undefined;
  onSelect: (path: string, byKey: boolean) => void;
  dropTargets: boolean;
  depth: number;
}) {
  const [open, setOpen] = useState(depth === 0 || (isOnTheWay(path, currentPath) && currentPath !== path));
  const listing = useFolder(path, open);
  const dirs = (listing.data ?? []).filter((entry) => entry.is_dir);
  const active = marked && path === currentPath;
  const here = herePath === path;
  // Known to hold no folders: then it has nothing to expand, and says so by
  // carrying no `aria-expanded` at all.
  const leaf = open && listing.data !== undefined && dirs.length === 0;
  // A listing that failed must not draw as a folder with no subfolders: the
  // tree is on screen the whole time, and an unread branch that looks empty is
  // this page claiming a currency it does not have.
  const fault = !listing.isError ? null : listing.data === undefined ? "unread" : "stale";

  // Opens, never closes: a branch a person folded by hand stays folded unless
  // the folder on screen moves somewhere beneath it.
  useEffect(() => {
    if (isOnTheWay(path, currentPath) && currentPath !== path) setOpen(true);
  }, [path, currentPath]);

  // The one Tab stop of the tree. Normally the folder on screen; but when that
  // node is not rendered (an ancestor folded by hand, the folder deleted, its
  // parent's listing failed) the deepest visible node on the way to it takes
  // over, so the tree is never left without a stop.
  const childOnTheWay = open && dirs.some((dir) => isOnTheWay(joinPath(path, dir.name), currentPath));
  const tabStop = path === currentPath || (isOnTheWay(path, currentPath) && !childOnTheWay);

  const spoken = [label, here ? "here" : null, fault].filter((part): part is string => part !== null).join(", ");

  return (
    <div
      className="fi-tree-node"
      role="treeitem"
      aria-label={spoken}
      aria-level={depth + 1}
      aria-expanded={leaf ? undefined : open}
      aria-selected={active}
      tabIndex={tabStop ? 0 : -1}
    >
      {/* Depth as a custom property and the step as a token, rather than a
          pixel count multiplied inline. */}
      <div
        className={active ? "fi-tree-row ui-current" : "fi-tree-row"}
        style={{ "--fi-depth": depth } as CSSProperties}
        data-drop-path={dropTargets ? path : undefined}
      >
        {/* The root has no toggle and no spacer where one would be: it is always
            open, and the blank 1.5rem left the current-folder rule standing apart
            from its name like a stray bar. */}
        {depth > 0 && (
          <button
            type="button"
            tabIndex={-1}
            className="fi-tree-toggle"
            aria-label={open ? `Collapse ${label}` : `Expand ${label}`}
            aria-expanded={open}
            onClick={() => setOpen((current) => !current)}
          >
            {open ? "▾" : "▸"}
          </button>
        )}
        <Button
          variant="link"
          tabIndex={-1}
          data-tree-select
          aria-current={active ? "location" : undefined}
          onClick={() => onSelect(path, keyboardPick)}
        >
          <span className="fi-tree-label" title={label}>
            {label}
          </span>
        </Button>
        {here && <span className="fi-tree-here">(here)</span>}
        {fault !== null && (
          <span
            className={fault === "unread" ? "fi-tree-fault fi-tree-fault-unread" : "fi-tree-fault"}
            title={
              fault === "unread"
                ? `the ${DAEMON_NAME} could not list this folder: open it to see why`
                : "this branch is the last listing that loaded; the latest one failed"
            }
          >
            {fault}
          </span>
        )}
      </div>
      {open && dirs.length > 0 && (
        <div className="fi-tree-children" role="group">
          {dirs.map((dir) => (
            <TreeNode
              key={dir.name}
              path={joinPath(path, dir.name)}
              label={dir.name}
              currentPath={currentPath}
              marked={marked}
              herePath={herePath}
              onSelect={onSelect}
              dropTargets={dropTargets}
              depth={depth + 1}
            />
          ))}
        </div>
      )}
    </div>
  );
}
