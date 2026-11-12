/**
 * Fuzzy matching for the command menu (#3).
 *
 * `fuzzyScore` returns `null` for no match, else a score (higher is better): a subsequence match,
 * rewarding consecutive runs, word starts and an early first hit. Case-insensitive. A plain
 * substring always matches, so this is a superset of the old substring filter.
 */
export function fuzzyScore(text: string, query: string): number | null {
  const q = query.trim().toLowerCase();
  if (q === "") return 0;
  const t = text.toLowerCase();
  const sub = t.indexOf(q);
  if (sub !== -1) return 1000 - sub + (sub === 0 ? 50 : 0) + (q.length === t.length ? 100 : 0);
  let score = 0;
  let ti = 0;
  let streak = 0;
  for (const ch of q) {
    const at = t.indexOf(ch, ti);
    if (at === -1) return null;
    const boundary = at === 0 || /[\s\-_/.]/.test(t[at - 1] ?? "");
    streak = at === ti && ti > 0 ? streak + 1 : 0;
    score += 1 + streak * 3 + (boundary ? 5 : 0) - Math.min(at - ti, 5) * 0.2;
    ti = at + 1;
  }
  return score;
}

/** Filter and rank `items` by `fuzzyScore` of `text(item)`; stable among ties. Empty query → same array. */
export function fuzzyFilter<T>(items: T[], query: string, text: (item: T) => string): T[] {
  if (query.trim() === "") return items;
  return items
    .map((item, i) => ({ item, i, s: fuzzyScore(text(item), query) }))
    .filter((r): r is { item: T; i: number; s: number } => r.s !== null)
    .sort((a, b) => b.s - a.s || a.i - b.i)
    .map((r) => r.item);
}
