import { RING_ROWS } from "./limits";
import type { TermLine } from "./types";

/**
 * The terminal: 12 fixed rows, pinned to the bottom, oldest clipped at the top, no scroll.
 *
 * Plain text only: a line is a React text child, never markup, because these are model and tool
 * output from sessions the owner did not write. The per-kind glyphs (`› ▸ ✗ ✓`) and the busy
 * cursor are CSS `::before`/`::after` content, so they are not part of the text anyone copies or a
 * screen reader hears. `aria-live="off"`: narrating a terminal continuously is noise.
 */
export function TermBody({ lines, label, card }: { lines: readonly TermLine[]; label: string; card: boolean }) {
  const shown = card ? lines.slice(-1) : lines.slice(-RING_ROWS);
  return (
    <div
      className={card ? "fleet-term fleet-term-card" : "fleet-term"}
      role="group"
      aria-label={`Terminal of ${label}`}
      aria-live="off"
    >
      {shown.map((l, i) => (
        <div key={i} className={`fleet-ln fleet-ln-${l.kind}`}>
          {l.text}
        </div>
      ))}
    </div>
  );
}
