import type { ReactNode } from "react";

export interface ErrorNoteProps {
  children: ReactNode;
}

/**
 * Something went wrong, said next to the control that caused it.
 *
 * `role="alert"` — assertive, because this is the case where the app tried and
 * could not, and the person is waiting on an answer that is not coming.
 * {@link RefusalNote} is the deliberate contrast: a refusal is the daemon
 * answering, and answering is not failing.
 *
 * There are no toasts in this app. A message that appears in a corner, away
 * from the button that produced it, is a message about nothing in particular
 * — and it leaves before the person who was reading something else looks up.
 */
export function ErrorNote({ children }: ErrorNoteProps) {
  return (
    <p className="ui-note ui-note-error" role="alert">
      {children}
    </p>
  );
}
