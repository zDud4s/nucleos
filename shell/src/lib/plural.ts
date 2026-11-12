/**
 * The noun for a count: "file" for one, "files" for anything else, nought included ("0 files").
 *
 * `many` is for the nouns an English `-s` gets wrong ("person" / "people"). Pure, so the rule is
 * asserted without mounting anything; `ui/Count` draws the same rule inside a heading.
 */
export function pluralWord(n: number, one: string, many?: string): string {
  return n === 1 ? one : (many ?? `${one}s`);
}

/** The count and its noun together: `plural(3, "file")` is "3 files", `plural(1, "file")` "1 file". */
export function plural(n: number, one: string, many?: string): string {
  return `${n} ${pluralWord(n, one, many)}`;
}
