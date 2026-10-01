import type { ApiRefusal } from "../data/client";

/**
 * Sentences for the refusals every page can meet.
 *
 * Two kinds of key live here. The first five are names the núcleo chose for
 * itself (`core/src/http.rs`, `fn refusal`) — a route that knows why it said no
 * says so, and the shell's job is to turn the name into a sentence a person can
 * act on. The rest are the status-derived floor that `client.ts` falls back to
 * when a route refused without naming itself; they are vaguer because the
 * information is vaguer, and pretending otherwise would be inventing a reason.
 *
 * Pages override this with copy of their own — a 409 on `POST /jobs` is "that
 * project is full", a 409 on a merge decision is "somebody already decided the
 * opposite way", and neither is a generic conflict. The floor exists so that a
 * page which has not written its copy yet still says something true.
 */
const SENTENCES: Record<string, string> = {
  // Named by the núcleo.
  kill_switch: "the kill switch is engaged — nothing autonomous starts until it is released",
  turn_in_progress: "this conversation already has a turn in flight; it clears on its own",
  no_local_model: "no local model is available, and this asked for one",
  internal: "the núcleo hit an error of its own handling this",

  // Derived from the status, for routes that refused without a name.
  bad_request: "the núcleo would not accept that request",
  unauthorized: "this window is not authorised — the daemon token may have rotated",
  forbidden: "the núcleo refused this outright",
  not_found: "there is nothing there to act on",
  conflict: "something about this has already changed",
  unprocessable: "the núcleo understood the request and would not carry it out",
  locked: "this is locked right now",
  too_many_requests: "a ceiling is holding this back",
  unavailable: "the part of the núcleo this needs is not available",
};

export interface RefusalNoteProps {
  refusal: ApiRefusal;
  /**
   * Page-specific copy, merged over the shared floor.
   *
   * This is where a page says what its own 409 means. Same codes, sharper
   * sentences, because only the route knows which ceiling it hit.
   */
  sentences?: Record<string, string>;
}

/**
 * The daemon answered, and the answer was no.
 *
 * A refusal is a *value*, not an error, and this is the component that makes
 * the difference visible. `role="status"` rather than `alert`: nothing broke,
 * so nothing should interrupt what a screen reader is in the middle of saying.
 * The styling is cool and quiet where {@link ErrorNote} is red — a rule fired,
 * and the correct response is to change something, not to retry.
 *
 * The refusal's own code is shown alongside the sentence. It is the string that
 * survives rewording, and it is what a person quotes when the sentence does not
 * explain enough.
 *
 * The fallback order matters: page copy, then shared copy, then whatever prose
 * the daemon sent, and only then a sentence built from the code. There is no
 * step at which this renders "request failed".
 */
export function RefusalNote({ refusal, sentences }: RefusalNoteProps) {
  const named = sentences?.[refusal.code] ?? SENTENCES[refusal.code];
  const detail = refusal.detail.trim();
  // The daemon repeats the code as the detail when the code is all it sent;
  // showing it twice reads as a stutter rather than as information.
  const prose = detail !== "" && detail !== refusal.code ? detail : "";
  const sentence = named ?? (prose !== "" ? prose : `the núcleo refused this: ${refusal.code}`);

  return (
    <p className="ui-note ui-note-refusal" role="status">
      <span className="ui-note-code">{refusal.code}</span>
      <span className="ui-note-text">{sentence}</span>
    </p>
  );
}
