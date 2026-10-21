import { RING_ROWS } from "./limits";
import type { TermLine, TranscriptEntry } from "./types";

/** A transcript entry and a live line are the same event: same kind, same text or a `…`-cut prefix. */
export function sameLine(entry: TranscriptEntry, line: TermLine): boolean {
  if (entry.kind !== line.kind) return false;
  if (entry.text === line.text) return true;
  return line.text.endsWith("…") && entry.text.startsWith(line.text.slice(0, -1));
}

/** The buffered live lines that the first page did not already contain (compared with its tail). */
export function freshLines(page: readonly TranscriptEntry[], buffered: readonly TermLine[]): TermLine[] {
  const tail = page.slice(-RING_ROWS);
  return buffered.filter((line) => !tail.some((entry) => sameLine(entry, line)));
}
