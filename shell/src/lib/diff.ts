/**
 * Reading `git diff` well enough to draw it.
 *
 * Not a diff engine: git has already done the work and this only decides what each line IS, so the
 * page can colour it. Kept apart from the page for the reason `mention.ts` and `editor.ts` are —
 * what counts as an added line is a decision about *data*, and a module that cannot import a
 * component is a module that cannot grow one.
 */

/** What one line of a diff is. */
export type DiffKind = "added" | "removed" | "meta" | "context";

export interface DiffLine {
  kind: DiffKind;
  text: string;
}

/**
 * The lines that say WHERE rather than WHAT.
 *
 * `+++` and `---` are the trap. They begin with the same characters as content and are file names,
 * so a rule that looks at the first character alone paints a path green as though somebody had
 * written it — and does it at the top of every file in the diff, which is where the eye lands first.
 * Tested before the single-character rules below for exactly that reason.
 */
const HEADERS = ["+++ ", "--- ", "diff --git ", "index ", "@@ ", "new file mode", "deleted file mode", "similarity index", "rename from", "rename to"];

/**
 * One `git diff`, as lines a page can colour.
 *
 * Empty in, empty out: a diff with nothing in it is a real answer — nothing has changed — and it is
 * not a line. The page says that in words rather than drawing an empty box.
 */
export function diffLines(diff: string): DiffLine[] {
  if (diff.trim() === "") return [];
  return diff
    .split("\n")
    .filter((line, at, all) => !(line === "" && at === all.length - 1))
    .map((text) => ({ kind: kindOf(text), text }));
}

function kindOf(line: string): DiffKind {
  if (HEADERS.some((header) => line.startsWith(header))) return "meta";
  if (line.startsWith("+")) return "added";
  if (line.startsWith("-")) return "removed";
  return "context";
}
