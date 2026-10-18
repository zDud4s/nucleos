import { formatItem, parseItem, parseView, type BrainView } from "./item-ref";

// Kept apart from the page so the router can validate `/brain` without loading the page (and d3).
export interface BrainSearch {
  capture?: number;
  view: BrainView;
  /** `note:<id>` | `knowledge:<id>`, already checked by `parseItem`. */
  item?: string;
}

/**
 * The Brain's search params.
 *
 * `capture` is the stamp a shortcut puts in the address to say "take a note now": a number or
 * nothing, as `validateVoiceSearch` does for `talk` — anything else is the same as not asking.
 * `view` is `list` unless it says `graph`. `item` is kept only when `parseItem` accepts it, so a
 * malformed one (`knowledge:abc`) is dropped rather than opening a panel for nothing.
 */
export function validateBrainSearch(search: Record<string, unknown>): BrainSearch {
  const out: BrainSearch = { view: parseView(search.view) };
  const capture = typeof search.capture === "number" ? search.capture : Number(search.capture);
  if (Number.isFinite(capture) && search.capture !== undefined && search.capture !== "") {
    out.capture = capture;
  }
  const item = parseItem(search.item);
  if (item !== null) out.item = formatItem(item);
  return out;
}

