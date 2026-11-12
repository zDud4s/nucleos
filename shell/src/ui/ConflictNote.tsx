import type { ReactNode } from "react";

export interface ConflictNoteProps {
  children: ReactNode;
}

/**
 * What is about to happen will not be allowed, said before it is attempted.
 *
 * The rung the ladder was missing. {@link ErrorNote} reports a fault, and
 * {@link RefusalNote} reports a "no" that has already been said; neither covers
 * "press this and it will be refused", which is a different sentence because it
 * is addressed to somebody who can still choose otherwise.
 *
 * No `role="alert"`. That is the deliberate contrast with `ErrorNote`: an alert
 * interrupts whatever is being read, and interrupting is right when the app
 * tried and could not. Here nothing has been tried yet — the note sits beside a
 * control the reader has not pressed, and it will be read when they look at it.
 * Announcing it assertively would make every rule in the system shout the moment
 * a form rendered.
 *
 * Two pages had this rule byte for byte under their own names
 * (`waiting-conflict`, `contacts-conflict`) before it existed here.
 */
export function ConflictNote({ children }: ConflictNoteProps) {
  return <p className="ui-note ui-note-conflict">{children}</p>;
}
