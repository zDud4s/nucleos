/**
 * A line diff between two texts, computed here rather than read from git.
 *
 * Not `diff.ts`: that module only parses output git has already produced, and a council seat's
 * revision of its own answer never touches a repository. So this is the small engine `diff.ts`
 * deliberately is not -- a longest-common-subsequence table over lines, with no dependency, because
 * an answer is a few dozen lines and a quadratic table over that is nothing.
 *
 * It returns the same `DiffLine` shape `diff.ts` does, so whatever draws a git diff draws this too.
 */

import type { DiffLine } from "./diff";

/**
 * An empty string is NO lines, not one empty line. `"".split("\n")` is `[""]`, and taking that
 * literally would draw a phantom blank row on the empty side of a first revision or a withdrawn one.
 */
function linesOf(text: string): string[] {
  return text === "" ? [] : text.split("\n");
}

/**
 * The lines of `before` and `after`, each marked context, removed or added.
 *
 * The table is built over SUFFIXES (`lcs[i][j]` is the common-subsequence length of `a[i..]` and
 * `b[j..]`) so the walk that reads it can go forward and emit lines in reading order. On a tie the
 * walk removes before it adds, which is what keeps a replaced block as every old line followed by
 * every new one: interleaving them is what makes a revision unreadable at a glance.
 */
export function lineDiff(before: string, after: string): DiffLine[] {
  const a = linesOf(before);
  const b = linesOf(after);
  const n = a.length;
  const m = b.length;

  const lcs: number[][] = Array.from({ length: n + 1 }, () => new Array<number>(m + 1).fill(0));
  for (let i = n - 1; i >= 0; i--) {
    for (let j = m - 1; j >= 0; j--) {
      lcs[i][j] = a[i] === b[j] ? lcs[i + 1][j + 1] + 1 : Math.max(lcs[i + 1][j], lcs[i][j + 1]);
    }
  }

  const out: DiffLine[] = [];
  let i = 0;
  let j = 0;
  while (i < n && j < m) {
    if (a[i] === b[j]) {
      out.push({ kind: "context", text: a[i] });
      i++;
      j++;
    } else if (lcs[i + 1][j] >= lcs[i][j + 1]) {
      out.push({ kind: "removed", text: a[i] });
      i++;
    } else {
      out.push({ kind: "added", text: b[j] });
      j++;
    }
  }
  for (; i < n; i++) out.push({ kind: "removed", text: a[i] });
  for (; j < m; j++) out.push({ kind: "added", text: b[j] });
  return out;
}
