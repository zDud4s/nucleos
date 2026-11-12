import type { MouseEvent, ReactNode } from "react";
import { UI_LOCALE } from "../lib/locale";

/**
 * A `git diff`, drawn so that what changed is the first thing the eye finds.
 *
 * One renderer and not three. The inspector (`pages/Projects.tsx`), a conversation's "what is
 * different" (`pages/Chats.tsx`) and the Code mode's review (`project/ModeCode.tsx`) each drew a
 * diff of their own, and the one where a person decides whether to trust an agent's work was the
 * one with no colour at all. The shared unit is the component (the decision of 2026-09-06), so the
 * parsing, the tints and the line numbers are decided here once.
 *
 * **The meaning never lives in the colour alone.** Every line keeps the `+`, `-` or space git gave
 * it; an added line is an `<ins>` and a removed one a `<del>`, so the distinction survives a
 * screen reader, a forced-colours theme and a reader who cannot separate the hues. The tint only
 * reinforces what the characters and the elements already say.
 *
 * **Two gutters, old and new**, counted from each hunk's `@@ -a,b +c,d @@`. A diff without numbers
 * makes a person count lines to answer "where is this", and the new-side number is also the one
 * an editor opens at — which is what {@link DiffProps.lineLink} is for.
 *
 * Past {@link DIFF_ROW_CAP} rows it is one block of plain text and says so. A `git diff` has no
 * upper bound — a regenerated lock file alone is tens of thousands of lines — and a row here is
 * several page elements.
 */

/** What one line of a diff is. `note` is git's `\ No newline at end of file`. */
export type DiffRowKind = "add" | "remove" | "hunk" | "file" | "context" | "note";

export interface DiffRow {
  kind: DiffRowKind;
  text: string;
  /** The line's number before the change, when it has one — context and removals. */
  oldLine: number | null;
  /** The line's number after the change, when it has one — context and additions. */
  newLine: number | null;
  /**
   * The file this line belongs to, as the `+++` header named it (the `---` one for a deletion).
   * `null` before any header, or for a fragment that never had one.
   */
  file: string | null;
}

/**
 * How many rows are worth one element each.
 *
 * The same number the inspector used (`DIFF_LINE_CAP` in `data/projects.ts`), for the same reason;
 * it lives here now because this is the component that pays for the rows.
 */
export const DIFF_ROW_CAP = 2000;

const HUNK = /^@@ -(\d+)(?:,(\d+))? \+(\d+)(?:,(\d+))? @@/;

/**
 * The header lines that say WHERE rather than WHAT.
 *
 * `+++` and `---` are the trap: they begin with the characters of an addition and a removal and
 * are file names, so a rule that looked at the first character alone would paint a path green at
 * the top of every file. They are only headers OUTSIDE a hunk, though — inside one, a removed line
 * whose own text is `-- x` arrives as `--- x`, and that is a removal. Which is why the reader below
 * counts each hunk's lines down rather than trusting prefixes.
 */
const HEADERS = [
  "diff --git ",
  "index ",
  "new file mode",
  "deleted file mode",
  "old mode",
  "new mode",
  "similarity index",
  "dissimilarity index",
  "rename from",
  "rename to",
  "copy from",
  "copy to",
  "Binary files ",
];

/** `a/core/src/http.rs` → `core/src/http.rs`; a `--no-prefix` diff has no prefix to take off. */
function unprefixed(path: string): string {
  return path.replace(/^[ab]\//, "");
}

/**
 * One `git diff`, as rows a page can draw.
 *
 * Empty in, empty out: a diff with nothing in it is a real answer — nothing changed — and it is
 * not a row. Callers say that in words rather than drawing an empty box.
 */
export function readDiff(diff: string): DiffRow[] {
  if (diff.trim() === "") return [];
  const lines = diff.split("\n");
  // The newline that ends the last line is not a line of its own.
  if (lines[lines.length - 1] === "") lines.pop();

  const rows: DiffRow[] = [];
  let file: string | null = null;
  let removedFile: string | null = null;
  let oldAt = 0;
  let newAt = 0;
  let oldLeft = 0;
  let newLeft = 0;

  for (const text of lines) {
    const row = (kind: DiffRowKind, oldLine: number | null = null, newLine: number | null = null) =>
      rows.push({ kind, text, oldLine, newLine, file });

    // Git's remark about the line above it. It belongs to no side and consumes no count.
    if (text.startsWith("\\")) {
      row("note");
      continue;
    }

    // Inside a hunk the counts decide, not the look of the line.
    if (oldLeft > 0 || newLeft > 0) {
      if (text.startsWith("+")) {
        row("add", null, newAt);
        newAt += 1;
        newLeft -= 1;
      } else if (text.startsWith("-")) {
        row("remove", oldAt, null);
        oldAt += 1;
        oldLeft -= 1;
      } else {
        row("context", oldAt, newAt);
        oldAt += 1;
        newAt += 1;
        oldLeft -= 1;
        newLeft -= 1;
      }
      continue;
    }

    const hunk = HUNK.exec(text);
    if (hunk !== null) {
      oldAt = Number(hunk[1]);
      oldLeft = hunk[2] === undefined ? 1 : Number(hunk[2]);
      newAt = Number(hunk[3]);
      newLeft = hunk[4] === undefined ? 1 : Number(hunk[4]);
      row("hunk");
      continue;
    }

    if (text.startsWith("diff --git ")) {
      const named = / b\/(.+)$/.exec(text);
      file = named === null ? null : named[1];
      removedFile = null;
      row("file");
      continue;
    }
    if (text.startsWith("--- ")) {
      removedFile = text.slice(4) === "/dev/null" ? null : unprefixed(text.slice(4));
      row("file");
      continue;
    }
    if (text.startsWith("+++ ")) {
      const added = text.slice(4);
      // A deleted file's new side is `/dev/null`; its lines still belong to the old name.
      file = added === "/dev/null" ? removedFile : unprefixed(added);
      row("file");
      continue;
    }
    if (HEADERS.some((header) => text.startsWith(header))) {
      row("file");
      continue;
    }

    // A fragment with no hunk header — the first character is all there is to go on.
    if (text.startsWith("+")) row("add");
    else if (text.startsWith("-")) row("remove");
    else row("context");
  }
  return rows;
}

/** A place to open a line at, when the caller has one. */
export interface LineLink {
  /** The URL the click opens, so the destination shows on hover and can be copied. */
  href: string;
  /** What a click does instead of following `href` — the editor's door goes through a plugin. */
  open: () => void;
  /** What the link is called to somebody who cannot see the gutter, e.g. "Open line 12 in VS Code". */
  label: string;
}

export interface DiffProps {
  /** The diff, exactly as git printed it. */
  text: string;
  /** The region's name for a screen reader — which diff this is. */
  label: string;
  /**
   * Makes a new-side line number a door, when the caller can say where it leads.
   *
   * `file` is the row's file as the header named it, repository-relative. Return `null` for a row
   * with nowhere to go; the number is then drawn as plain text.
   */
  lineLink?: (file: string | null, line: number) => LineLink | null;
}

/*
  The two sides are a fill with full-strength text on it, not a tone's foreground. Acting Green on
  a word means work happening now (`badge-authorship.test.ts` refuses that foreground as a text class
  outside the map), and an added line is not that — the fill marks the band, the `+` and the
  `<ins>` carry the fact, and the text stays at the contrast of every other line.
*/
const ROW_TONE: Record<DiffRowKind, string> = {
  add: "bg-tone-active-bg text-text",
  remove: "bg-tone-danger-bg text-text",
  // Where in the file, which is navigation rather than change.
  hunk: "text-tone-info-fg",
  // Which file — including the `---`/`+++` pair, a header and not one removal and one addition.
  file: "font-medium text-text",
  context: "text-text-muted",
  note: "text-text-faint",
};

/*
  The box every rendering shares: the sunken rung, the data face at the data size (13px/1.35),
  a height it stops at, and a scroll of its own so a long diff scrolls inside itself instead of
  stretching the page it sits on. Focusable, because a region that scrolls and cannot take focus
  cannot be scrolled from a keyboard.
*/
const BOX =
  "max-h-[32rem] overflow-auto overscroll-none rounded-sm border border-border bg-surface-sunken py-2 font-mono text-sm leading-snug [tab-size:4]";

export function Diff({ text, label, lineLink }: DiffProps) {
  const rows = readDiff(text);
  if (rows.length === 0) return null;

  if (rows.length > DIFF_ROW_CAP) {
    return (
      <div className="flex flex-col gap-2">
        <p className="text-xs text-text-muted">
          {rows.length.toLocaleString(UI_LOCALE)} lines — past {DIFF_ROW_CAP.toLocaleString(UI_LOCALE)}{" "}
          this is drawn as plain text, because a row per line is a page element per line.
        </p>
        <pre className={`${BOX} m-0 whitespace-pre px-3 text-text-muted`} aria-label={label} tabIndex={0}>
          <code>{text}</code>
        </pre>
      </div>
    );
  }

  // Wide enough for the longest number on either side, so the two gutters line up down the page.
  const digits = Math.max(
    2,
    ...rows.map((row) => String(Math.max(row.oldLine ?? 0, row.newLine ?? 0)).length),
  );
  const columns = { gridTemplateColumns: `${digits + 1}ch ${digits + 1}ch minmax(0, 1fr)` };

  return (
    <div role="region" aria-label={label} tabIndex={0} className={BOX}>
      {/* `w-max min-w-full`: every row as wide as the widest, so a tint runs the full width even
          when the box has scrolled sideways, and a run of removals reads as one thing. */}
      <code className="block w-max min-w-full">
        {rows.map((row, at) => (
          // Keyed by position: a diff is read whole and redrawn whole, and nothing reorders in it.
          <span key={at} className={`grid min-h-[1.35em] pr-3 ${ROW_TONE[row.kind]}`} style={columns}>
            <Gutter tinted={row.kind === "add" || row.kind === "remove"}>{row.oldLine}</Gutter>
            <Gutter tinted={row.kind === "add" || row.kind === "remove"}>
              {row.newLine === null ? null : (
                <LineNumber line={row.newLine} link={lineLink?.(row.file, row.newLine) ?? null} />
              )}
            </Gutter>
            <span className="whitespace-pre pl-2">
              {row.kind === "add" ? (
                <ins className="no-underline">{row.text}</ins>
              ) : row.kind === "remove" ? (
                <del className="no-underline">{row.text}</del>
              ) : (
                row.text
              )}
            </span>
          </span>
        ))}
      </code>
    </div>
  );
}

/**
 * One number column. Unselectable, so a copied block carries the code and not `12 13` down its
 * left edge; on a tinted row it takes the row's own colour, because Faint on a tone fill is the
 * one pairing nobody measured.
 */
function Gutter({ tinted, children }: { tinted: boolean; children: ReactNode }) {
  return (
    <span
      className={`select-none pr-1 text-right tabular-nums ${tinted ? "" : "text-text-faint"}`}
    >
      {children}
    </span>
  );
}

function LineNumber({ line, link }: { line: number; link: LineLink | null }) {
  if (link === null) return <>{line}</>;
  return (
    <a
      href={link.href}
      aria-label={link.label}
      title={link.label}
      onClick={(event: MouseEvent<HTMLAnchorElement>) => {
        event.preventDefault();
        link.open();
      }}
      className="text-inherit hover:text-text hover:underline"
    >
      {line}
    </a>
  );
}
