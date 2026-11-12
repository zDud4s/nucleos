import type { ReactNode } from "react";
import { textOf, useToastOnce } from "./toast";

export interface ErrorNoteProps {
  children: ReactNode;
  /** When given, a Retry button is drawn beside the message and calls this. */
  onRetry?: () => void;
  /** Label of the retry button. Defaults to "Retry". */
  retryLabel?: string;
}

/**
 * Something went wrong. Said as a toast, not as a box in the page.
 *
 * An inline banner pushed the page around when it appeared; this component keeps its old
 * call sites and API but renders nothing, and raises an error toast (role="alert") when it
 * mounts or its message changes. The same message is not toasted twice in a row, so a poll
 * that keeps failing speaks once. `onRetry` becomes the toast's action button.
 * {@link RefusalNote} is the deliberate contrast: a refusal is the daemon answering.
 */
export function ErrorNote({ children, onRetry, retryLabel = "Retry" }: ErrorNoteProps) {
  useToastOnce({
    type: "error",
    title: textOf(children),
    action: onRetry ? { label: retryLabel, onClick: onRetry } : undefined,
  });
  return null;
}
