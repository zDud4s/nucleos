import type { ReactNode } from "react";
import { Button } from "./Button";

export interface ErrorNoteProps {
  children: ReactNode;
  /** When given, a Retry button is drawn beside the message and calls this. */
  onRetry?: () => void;
  /** Label of the retry button. Defaults to "Retry". */
  retryLabel?: string;
}

/**
 * Something went wrong, said next to the control that caused it.
 *
 * `role="alert"` — assertive, because this is the case where the app tried and
 * could not, and the person is waiting on an answer that is not coming.
 * {@link RefusalNote} is the deliberate contrast: a refusal is the daemon
 * answering, and answering is not failing.
 *
 * `onRetry` is optional and additive: a failed read can be tried again, a
 * failed one-off action usually should not offer to repeat itself.
 *
 * There are no toasts in this app. A message that appears in a corner, away
 * from the button that produced it, is a message about nothing in particular
 * — and it leaves before the person who was reading something else looks up.
 */
export function ErrorNote({ children, onRetry, retryLabel = "Retry" }: ErrorNoteProps) {
  return (
    <p className="ui-note ui-note-error" role="alert">
      <span>{children}</span>
      {onRetry ? (
        <Button type="button" onClick={onRetry}>
          {retryLabel}
        </Button>
      ) : null}
    </p>
  );
}
