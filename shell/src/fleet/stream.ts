import { openStream } from "../data/client";
import { LIVE_PATH, SILENCE_MS, backoffFor } from "./limits";
import { NdjsonSplitter, parseEvent } from "./ndjson";
import type { ConnectionState, FleetEvent } from "./types";

export interface StreamHandlers {
  onEvent(event: FleetEvent): void;
  onState(state: ConnectionState): void;
}

type Open = (path: string, signal: AbortSignal) => Promise<ReadableStream<Uint8Array>>;

function sleep(ms: number, signal: AbortSignal): Promise<void> {
  return new Promise((resolve) => {
    if (signal.aborted) return resolve();
    const timer = setTimeout(done, ms);
    function done() {
      clearTimeout(timer);
      signal.removeEventListener("abort", done);
      resolve();
    }
    signal.addEventListener("abort", done);
  });
}

/**
 * Follows `GET /fleet/live` until the returned function is called.
 *
 * Each connection owns an AbortController of its own so the silence watchdog can kill just that
 * connection; `stop` aborts the outer one, which aborts whichever connection is open. There is no
 * replay: after a reconnect the daemon's first event is a `snapshot`, and the store replaces its
 * whole state with it. The backoff resets on that snapshot, not on connect, so a daemon that
 * accepts and immediately drops does not get hammered at 1 s.
 */
export function startFleetStream(handlers: StreamHandlers, open: Open = openStream): () => void {
  const outer = new AbortController();

  void (async () => {
    let attempt = 0;
    handlers.onState("connecting");
    while (!outer.signal.aborted) {
      const link = new AbortController();
      const onOuterAbort = () => link.abort();
      outer.signal.addEventListener("abort", onOuterAbort, { once: true });
      let silence: ReturnType<typeof setTimeout> | undefined;
      const arm = () => {
        clearTimeout(silence);
        silence = setTimeout(() => link.abort(), SILENCE_MS);
      };
      try {
        arm();
        const body = await open(LIVE_PATH, link.signal);
        const reader = body.getReader();
        // A body that is not tied to the signal (a test pipe) still has to stop reading.
        link.signal.addEventListener("abort", () => void reader.cancel().catch(() => undefined), { once: true });
        if (link.signal.aborted) void reader.cancel().catch(() => undefined);
        const splitter = new NdjsonSplitter();
        for (;;) {
          const { done, value } = await reader.read();
          if (done) break;
          arm();
          for (const text of splitter.push(value)) {
            const event = parseEvent(text);
            if (event === null) continue;
            // A handler that throws is a bug in the shell, not a dropped connection: it must not
            // reach the catch below (which would reconnect and hide the "can't reach" note logic).
            try {
              handlers.onEvent(event);
            } catch (error) {
              console.error("fleet: could not apply an event", error);
              continue;
            }
            if (event.t === "snapshot") {
              attempt = 0;
              handlers.onState("live");
            }
          }
        }
      } catch {
        // Refused, dropped or aborted: all the same to the loop below.
      } finally {
        clearTimeout(silence);
        outer.signal.removeEventListener("abort", onOuterAbort);
      }
      if (outer.signal.aborted) return;
      handlers.onState("reconnecting");
      await sleep(backoffFor(attempt), outer.signal);
      attempt += 1;
    }
  })();

  return () => outer.abort();
}
