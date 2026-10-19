import { useCallback, useEffect, useRef, useState, type ReactNode } from "react";
import { Button } from "./Button";

/** How long the undo window stays open, in milliseconds. */
export const UNDO_MS = 6000;

export interface UndoToastOptions {
  /** What happened, in the past tense and naming the subject. */
  message: string;
  /** Runs when the person presses Undo; the window closes without `onExpire`. */
  onUndo: () => void;
  /** Runs when the window closes unanswered: the point at which the action becomes final. */
  onExpire?: () => void;
  /** Overrides {@link UNDO_MS}. */
  ms?: number;
}

export interface UndoToast {
  /** Open the window. A second call settles the first (as expired) and replaces it. */
  show: (options: UndoToastOptions) => void;
  /** Render this once, anywhere on the page. `null` while no window is open. */
  toast: ReactNode;
}

/**
 * A short "Undo" window after an action, held in local state.
 *
 * There is no toast system in this app (`ErrorNote` says so on purpose), so this is the minimum:
 * the hook returns the element and the page places it, next to what it concerns. It is
 * `role="status"`, so the message is announced politely, and it holds the pending action: Undo
 * cancels it, and silence for `ms` lets it stand. Unmounting settles a pending window as expired.
 */
export function useUndoToast(): UndoToast {
  const [current, setCurrent] = useState<UndoToastOptions | null>(null);
  const timer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const pending = useRef<UndoToastOptions | null>(null);

  const clear = useCallback(() => {
    if (timer.current !== null) clearTimeout(timer.current);
    timer.current = null;
  }, []);

  const expire = useCallback(() => {
    clear();
    const p = pending.current;
    pending.current = null;
    setCurrent(null);
    p?.onExpire?.();
  }, [clear]);

  const show = useCallback(
    (options: UndoToastOptions) => {
      const previous = pending.current;
      clear();
      previous?.onExpire?.();
      pending.current = options;
      setCurrent(options);
      timer.current = setTimeout(expire, options.ms ?? UNDO_MS);
    },
    [clear, expire],
  );

  const undo = useCallback(() => {
    clear();
    const p = pending.current;
    pending.current = null;
    setCurrent(null);
    p?.onUndo();
  }, [clear]);

  useEffect(
    () => () => {
      clear();
      const p = pending.current;
      pending.current = null;
      p?.onExpire?.();
    },
    [clear],
  );

  const toast =
    current === null ? null : (
      <div className="ui-undo-toast" role="status">
        <span className="ui-undo-toast-message">{current.message}</span>
        <Button variant="link" onClick={undo}>
          Undo
        </Button>
      </div>
    );

  return { show, toast };
}
