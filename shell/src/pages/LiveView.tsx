import { useEffect, useState } from "react";
import { isApiRefusal, openStream } from "../data/client";
import { backoffDelay, readRecords } from "../data/liveRecords";

type Status =
  | { kind: "connecting" }
  | { kind: "live" }
  | { kind: "reconnecting" }
  | { kind: "ended"; sentence: string };

const STALE_AFTER_MS = 5000;

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

function useVisible(): boolean {
  const [visible, setVisible] = useState(() => document.visibilityState === "visible");
  useEffect(() => {
    const onChange = () => setVisible(document.visibilityState === "visible");
    document.addEventListener("visibilitychange", onChange);
    return () => document.removeEventListener("visibilitychange", onChange);
  }, []);
  return visible;
}

/**
 * A live view of one agent-mode session: the daemon's frame stream, shown as
 * it arrives. It streams only while mounted and visible, says in a sentence
 * why it stopped when it stops for good, and reconnects with backoff when the
 * connection merely dropped.
 */
export function LiveView({ sessionId }: { sessionId: number }) {
  const visible = useVisible();
  const [status, setStatus] = useState<Status>({ kind: "connecting" });
  const [src, setSrc] = useState<string | null>(null);
  const [lastFrameAt, setLastFrameAt] = useState<number | null>(null);
  const [now, setNow] = useState(() => Date.now());

  useEffect(() => {
    if (!visible) return;
    const controller = new AbortController();
    const { signal } = controller;
    let current: string | null = null;

    const drop = () => {
      if (current !== null) URL.revokeObjectURL(current);
      current = null;
    };

    setStatus({ kind: "connecting" });

    void (async () => {
      let attempt = 0;
      while (!signal.aborted) {
        try {
          const stream = await openStream(`/browser/sessions/${sessionId}/live`, signal);
          for await (const record of readRecords(stream)) {
            if (signal.aborted) return;
            if (record.kind === "frame") {
              attempt = 0;
              const next = URL.createObjectURL(new Blob([record.jpeg as BlobPart], { type: "image/jpeg" }));
              const previous = current;
              current = next;
              if (previous !== null) URL.revokeObjectURL(previous);
              setSrc(next);
              setLastFrameAt(Date.now());
              setNow(Date.now());
              setStatus({ kind: "live" });
            } else if (record.reason === "wheel") {
              setStatus({ kind: "ended", sentence: "The wheel was asked for, or a person has it." });
              return;
            } else if (record.reason === "closed") {
              setStatus({ kind: "ended", sentence: "Session closed." });
              return;
            } else {
              break;
            }
          }
        } catch (error) {
          if (signal.aborted) return;
          if (isApiRefusal(error)) {
            if (error.status === 409) {
              setStatus({ kind: "ended", sentence: error.detail });
              return;
            }
            if (error.status === 404) {
              setStatus({ kind: "ended", sentence: "Session closed." });
              return;
            }
          }
        }
        if (signal.aborted) return;
        setStatus({ kind: "reconnecting" });
        await sleep(backoffDelay(attempt), signal);
        attempt += 1;
      }
    })();

    return () => {
      controller.abort();
      drop();
      setSrc(null);
      setLastFrameAt(null);
    };
  }, [sessionId, visible]);

  const flowing = status.kind === "live" && lastFrameAt !== null;
  useEffect(() => {
    if (!flowing) return;
    const timer = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(timer);
  }, [flowing, lastFrameAt]);

  const silentFor = lastFrameAt === null ? 0 : Math.floor((now - lastFrameAt) / 1000);
  const stale = flowing && now - (lastFrameAt ?? now) > STALE_AFTER_MS;

  return (
    <div className="browser-live">
      {src !== null && <img className="browser-live-frame" src={src} alt={`live view of session ${sessionId}`} />}
      <div className="browser-live-status">
        {status.kind === "live" && <span className="browser-live-marker">live</span>}
        {stale && <span className="browser-live-quiet">{`unchanged for ${silentFor}s`}</span>}
        {status.kind === "connecting" && <span className="browser-live-quiet">connecting…</span>}
        {status.kind === "reconnecting" && <span className="browser-live-quiet">Connection lost — reconnecting</span>}
        {status.kind === "ended" && <span className="browser-live-end">{status.sentence}</span>}
      </div>
    </div>
  );
}
