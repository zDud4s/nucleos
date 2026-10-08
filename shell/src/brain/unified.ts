import type { Known, KnownLayer } from "../data/knowledge";
import type { OwnerNote } from "../data/owner-notes";
import { knownBucket, noteBucket, passesState } from "./item-state";

/** One entry of the unified Brain list. Discriminated on `kind`; a third kind is one more arm. */
export type BrainItem =
  | { kind: "note"; note: OwnerNote }
  | { kind: "knowledge"; known: Known };

export interface ItemFilters {
  type: "all" | "note" | "knowledge";
  state: "in_force" | "out" | "all";
  /** Knowledge only. Notes pass when this is `"all"` and are excluded otherwise. */
  layer: KnownLayer | "all";
  /** Knowledge only: `"machine"`, a project id, or `"all"`. Same rule for notes as `layer`. */
  scope: string;
  /** Matches knowledge title/body only; notes are narrowed by the server's search instead. */
  q: string;
}

export function toItems(notes: readonly OwnerNote[], known: readonly Known[]): BrainItem[] {
  return [
    ...notes.map((note): BrainItem => ({ kind: "note", note })),
    ...known.map((row): BrainItem => ({ kind: "knowledge", known: row })),
  ];
}

/** When the item entered the list: a note's creation, a knowledge row's activation (else creation). */
export function itemDate(item: BrainItem): string {
  return item.kind === "note" ? item.note.created_at : (item.known.activated_at ?? item.known.created_at);
}

export function itemId(item: BrainItem): number {
  return item.kind === "note" ? item.note.id : item.known.id;
}

/** The scope key a knowledge row answers to in the Scope filter (same keys as the Learned page). */
export function scopeKey(row: Known): string {
  return row.scope_kind === "machine" ? "machine" : (row.scope_id ?? "machine");
}

function time(item: BrainItem): number {
  const parsed = Date.parse(itemDate(item));
  return Number.isNaN(parsed) ? 0 : parsed;
}

/** Filtered and sorted newest first; ties fall to kind then id so the order is stable. */
export function filterItems(items: readonly BrainItem[], filters: ItemFilters): BrainItem[] {
  const needle = filters.q.trim().toLowerCase();
  return items
    .filter((item) => {
      if (filters.type !== "all" && item.kind !== filters.type) return false;
      if (item.kind === "note") {
        if (!passesState(noteBucket(item.note.state), filters.state, "list")) return false;
        return filters.layer === "all" && filters.scope === "all";
      }
      const row = item.known;
      if (!passesState(knownBucket(row.status), filters.state, "list")) return false;
      if (filters.layer !== "all" && row.layer !== filters.layer) return false;
      if (filters.scope !== "all" && scopeKey(row) !== filters.scope) return false;
      if (needle === "") return true;
      return row.title.toLowerCase().includes(needle) || row.body.toLowerCase().includes(needle);
    })
    .sort((a, b) => {
      const diff = time(b) - time(a);
      if (diff !== 0) return diff;
      if (a.kind !== b.kind) return a.kind === "note" ? -1 : 1;
      return itemId(b) - itemId(a);
    });
}
