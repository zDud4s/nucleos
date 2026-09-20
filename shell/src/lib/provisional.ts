/**
 * The part of a text box that belongs to the microphone and not to the person.
 *
 * Progressive dictation revises what it already said. The model hears "come", then hears "come view",
 * then hears enough of the sentence to know it was "câmbio" — and each of those is a REPLACEMENT of
 * the last, not a word to add after it. Appending them produces "come come view câmbio", which is
 * what this file exists to stop: `useDictationInto` was append-only until 2026-09-20, because until
 * then the whole transcript arrived once, at the end.
 *
 * So a dictation in progress owns a span of the box, and every revision replaces the span whole. The
 * span closes — becomes ordinary text nobody will touch again — when the gate decides the sentence
 * ended, and the next sentence opens a new one after it.
 *
 * **The separator belongs to the span.** A microphone that appended " " and then heard nothing would
 * leave a space behind it that the person did not type and cannot see. Carrying `lead` inside the
 * region is what lets a revision down to nothing take its own space back with it.
 *
 * Pure, and separate from `data/dictation.ts`, for the usual reason in this codebase: the arithmetic
 * of a shrinking replacement is where the bugs are, and it is testable without a microphone.
 */

/** A span of a text box that the microphone will replace whole on its next revision. */
export interface Provisional {
  /** Where the span begins — at the separator, when there is one. */
  at: number;
  /** How many characters from `at` belong to the microphone, `lead` included. */
  length: number;
  /** What sits in front of the words and is as much the microphone's as they are. */
  lead: string;
}

export interface Revised {
  text: string;
  /** The new span, or `null` when this revision left the microphone owning nothing. */
  region: Provisional | null;
  /** Where the caret goes: the end of what was just written. */
  caret: number;
}

/**
 * The box after the microphone's latest guess at the sentence in progress.
 *
 * `region` is `null` for the first revision of a sentence — there is nothing of the microphone's in
 * the box yet, so this is where the span opens, at the end of whatever was typed.
 */
export function revise(text: string, region: Provisional | null, said: string): Revised {
  if (region === null) {
    // Nothing of ours in the box and nothing to put there. Opening an empty span would fix a
    // separator in place that every later revision would have to keep.
    if (said === "") return { text, region: null, caret: text.length };
    const lead = text === "" || /\s$/.test(text) ? "" : " ";
    const body = lead + said;
    return {
      text: text + body,
      region: { at: text.length, length: body.length, lead },
      caret: text.length + body.length,
    };
  }

  const body = said === "" ? "" : region.lead + said;
  return {
    text: text.slice(0, region.at) + body + text.slice(region.at + region.length),
    region: said === "" ? null : { at: region.at, length: body.length, lead: region.lead },
    caret: region.at + body.length,
  };
}
