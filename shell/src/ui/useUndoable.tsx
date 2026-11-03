import { useCallback, type ReactNode } from "react";
import { useUndoToast } from "./useUndoToast";

export interface UndoableOptions {
  /** What is about to happen, in the past tense, naming the subject ("Deleted 'Notes'"). */
  message: string;
  /**
   * The real, irreversible action. It is DELAYED: it runs when the undo window closes
   * unanswered (or when the page unmounts / another undoable replaces this one), never on
   * the click itself. Pages hide the item optimistically in the meantime.
   */
  commit: () => void | Promise<void>;
  /** Runs on Undo — restore the optimistic hide. `commit` is not called. */
  onUndo?: () => void;
  /** Overrides the default window. */
  ms?: number;
}

export interface Undoable {
  /** Start the window. The action commits after it unless the person presses Undo. */
  run: (options: UndoableOptions) => void;
  /** Render once, next to what it concerns. */
  toast: ReactNode;
}

/**
 * Delayed-commit undo, the replacement for a confirm dialog on reversible actions.
 *
 * ```tsx
 * const undoable = useUndoable();
 * onDelete = () => { hide(id); undoable.run({ message: "Deleted note", commit: () => api.remove(id), onUndo: () => show(id) }); };
 * return <>{list}{undoable.toast}</>;
 * ```
 *
 * Thin layer over {@link useUndoToast}: the toast's "expire" is the commit, so there is no
 * state where the person saw "Deleted" and the server never heard of it (unmount commits).
 */
export function useUndoable(): Undoable {
  const { show, toast } = useUndoToast();
  const run = useCallback(
    ({ message, commit, onUndo, ms }: UndoableOptions) => {
      show({
        message,
        ms,
        onUndo: () => onUndo?.(),
        onExpire: () => {
          void commit();
        },
      });
    },
    [show],
  );
  return { run, toast };
}
