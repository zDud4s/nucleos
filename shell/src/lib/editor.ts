/**
 * Which of the editor's conversations are happening right now.
 *
 * The daemon offers every session the CLI has a transcript for, newest first, and until now the
 * window drew them all the same way — so a conversation somebody is typing into this second looked
 * exactly like one from last Tuesday. The list already carried the fact that tells them apart and
 * nothing read it.
 *
 * Kept apart from the page for the reason `mention.ts` is: what counts as "still going" is a
 * decision about *data*, and a module that cannot import a component is a module that cannot grow
 * one.
 */

/**
 * How long after its last line a session still counts as one somebody is in.
 *
 * Two minutes because the gap this has to survive is a person reading an answer and deciding what
 * to say next, and the file says nothing at all while they do. Shorter and the mark flickers off
 * every time somebody thinks — at exactly the moment the answer they are reading arrived.
 *
 * It is deliberately not tied to how fast the window polls. Polling decides how soon a change is
 * noticed; this decides what counts as a change.
 */
export const STILL_IN_IT_MS = 2 * 60 * 1000;

/**
 * Whether this session is one somebody is in right now.
 *
 * What is actually known is when the CLI last wrote to the file, so that is what this measures and
 * what the name should be read as. A session written to a moment ago is one somebody is in; the
 * inference is short and it is the one the fact supports.
 *
 * A timestamp slightly AHEAD of this clock counts as recent rather than as broken — two clocks that
 * disagree by a second are ordinary, and the last line of a file is still the most recent thing that
 * happened whichever way the skew runs.
 *
 * An unreadable timestamp claims nothing. It is not evidence of a session being live, and marking
 * one on the strength of it would be inventing the single fact this exists to report.
 */
export function stillGoing(lastActivity: string, now: number): boolean {
  const written = Date.parse(lastActivity);
  if (Number.isNaN(written)) return false;
  return written >= now - STILL_IN_IT_MS;
}
