/**
 * Reading and writing an `@name` at the caret, purely — no React, no daemon, no DOM.
 *
 * Kept apart from the page for the reason `turns.ts` is: where a mention starts, what ends it, and
 * what replacing one does to the text around it are decisions about *strings*, and a module that
 * cannot import a component is a module that cannot grow one. `mention.test.ts` asserts every rule
 * here without mounting anything.
 */

/** A half-typed `@name` and where it sits, so it can be replaced in place. */
export interface Mentioning {
  /** What has been typed after the `@`. Empty for a bare `@`, which is where the list opens. */
  query: string;
  /** Index of the `@` itself. */
  from: number;
  /** Index just past the caret — the end of what will be replaced. */
  to: number;
}

/** What the box holds after a name is picked, and where the caret goes. */
export interface Written {
  text: string;
  caret: number;
}

/**
 * The mention being typed at `caret`, or nothing.
 *
 * Scans back from the caret, not forward from the start: the caret is where the person is, and the
 * mention that matters is the nearest `@` behind it. Whitespace ends the scan, so a finished word
 * never re-opens a list somebody has moved past.
 *
 * The `@` must begin a word. That single rule is what keeps `duarte@gmail.com` from opening a file
 * picker over an email address, which everybody types eventually.
 */
export function mentionAt(text: string, caret: number): Mentioning | null {
  for (let at = caret - 1; at >= 0; at -= 1) {
    const character = text[at];
    // A path is a normal thing to be part way through typing, so a slash does NOT end a mention —
    // only whitespace does.
    if (/\s/.test(character)) return null;
    if (character !== "@") continue;
    // Beginning a word: the start of the box, or preceded by whitespace. Anything else is an @
    // inside something that is not a mention.
    const before = at === 0 ? null : text[at - 1];
    if (before !== null && !/\s/.test(before)) return null;
    return { query: text.slice(at + 1, caret), from: at, to: caret };
  }
  return null;
}

/**
 * The box with `path` written where the half-typed name was.
 *
 * A file gets a trailing space, because without one the caret sits against the path and the next
 * thing typed becomes part of it — which is how a picked file turns straight back into a typo. A
 * folder gets a trailing slash and no space: it is a place to keep typing, not a name somebody has
 * finished giving.
 */
export function withMention(
  text: string,
  at: Mentioning,
  path: string,
  isDir = false,
): Written {
  const written = `@${path}${isDir ? "/" : " "}`;
  return {
    text: text.slice(0, at.from) + written + text.slice(at.to),
    caret: at.from + written.length,
  };
}
