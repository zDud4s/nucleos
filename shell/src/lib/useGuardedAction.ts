import { useCallback, useRef, useState } from "react";
import { newIntent, type Intent } from "./idempotency";

/**
 * Guard an async action against double clicks (#19).
 *
 *   const { run, pending } = useGuardedAction((intent) => post(body, { key: intent.key }));
 *   <Button disabled={pending} onClick={() => void run()}>Save</Button>
 *
 * While a run is in flight `pending` is true (disable the trigger with it) and further `run()`
 * calls return the SAME promise instead of starting another. Each fresh run mints one
 * {@link Intent}; pass `intent.key` to the request as its `Idempotency-Key`.
 */
export function useGuardedAction<A extends unknown[], R>(
  action: (intent: Intent, ...args: A) => Promise<R>,
) {
  const inflight = useRef<Promise<R> | null>(null);
  const actionRef = useRef(action);
  actionRef.current = action;
  const [pending, setPending] = useState(false);

  const run = useCallback((...args: A): Promise<R> => {
    if (inflight.current !== null) return inflight.current;
    setPending(true);
    const p = (async () => actionRef.current(newIntent(), ...args))().finally(() => {
      inflight.current = null;
      setPending(false);
    });
    inflight.current = p;
    return p;
  }, []);

  return { run, pending };
}
