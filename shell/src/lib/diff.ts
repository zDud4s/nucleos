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

/** What happened to one file, as git's headers say it. */
export type FileChange = "added" | "deleted" | "renamed" | "modified";

/** One file's share of a `git diff`: its name, what happened to it, and how many lines each way. */
export interface DiffFile {
  path: string;
  change: FileChange;
  /** Lines added inside its hunks. A binary file has none to count, and says 0. */
  added: number;
  /** Lines removed inside its hunks. */
  removed: number;
  /** Its own slice of the diff, headers included, exactly as git printed it. */
  text: string;
}

const HUNK_HEAD = /^@@ -\d+(?:,(\d+))? \+\d+(?:,(\d+))? @@/;

/**
 * A `git diff`, cut at each `diff --git` into the files it touches.
 *
 * What a person scans first is WHICH files and how much — the lines come after. Counted inside the
 * hunks by each hunk's own `@@` lengths rather than by first character, for the reason `HEADERS`
 * gives: a removed line whose text is `-- x` arrives as `--- x` and is a removal, not a header.
 *
 * Empty in, empty out, as `diffLines`. A fragment with no `diff --git` line is one unnamed file,
 * which is better than dropping lines somebody is looking for.
 */
export function diffFiles(diff: string): DiffFile[] {
  if (diff.trim() === "") return [];
  const lines = diff.split("\n");
  if (lines[lines.length - 1] === "") lines.pop();

  const files: DiffFile[] = [];
  let oldLeft = 0;
  let newLeft = 0;
  const begin = (path: string): DiffFile => {
    const started: DiffFile = { path, change: "modified", added: 0, removed: 0, text: "" };
    files.push(started);
    oldLeft = 0;
    newLeft = 0;
    return started;
  };

  let file: DiffFile | null = null;
  for (const line of lines) {
    if (line.startsWith("diff --git ")) {
      const named = / b\/(.+)$/.exec(line);
      file = begin(named === null ? line.slice("diff --git ".length) : named[1]);
    } else if (file === null) {
      file = begin("");
    }
    file.text += `${line}\n`;

    if (oldLeft > 0 || newLeft > 0) {
      if (line.startsWith("\\")) continue;
      if (line.startsWith("+")) {
        file.added += 1;
        newLeft -= 1;
      } else if (line.startsWith("-")) {
        file.removed += 1;
        oldLeft -= 1;
      } else {
        oldLeft -= 1;
        newLeft -= 1;
      }
      continue;
    }
    const hunk = HUNK_HEAD.exec(line);
    if (hunk !== null) {
      oldLeft = hunk[1] === undefined ? 1 : Number(hunk[1]);
      newLeft = hunk[2] === undefined ? 1 : Number(hunk[2]);
      continue;
    }
    if (line.startsWith("new file mode")) file.change = "added";
    else if (line.startsWith("deleted file mode")) file.change = "deleted";
    else if (line.startsWith("rename to ")) {
      file.change = "renamed";
      file.path = line.slice("rename to ".length);
    } else if (line.startsWith("+++ ") && line !== "+++ /dev/null") {
      file.path = line.slice(4).replace(/^b\//, "");
    } else if (file.path === "" && line.startsWith("--- ") && line !== "--- /dev/null") {
      file.path = line.slice(4).replace(/^a\//, "");
    }
  }
  return files;
}
