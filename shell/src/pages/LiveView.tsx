import { useEffect, useRef, useState, type ClipboardEvent, type CompositionEvent, type KeyboardEvent, type MouseEvent } from "react";
import { isApiRefusal, openStream } from "../data/client";
import { InputBatcher, keyEvent, modifiers, mouseButton, toPage } from "../data/liveInput";
import { backoffDelay, readRecords, type FrameMeta, type InputEvent, type OpenPrompt, type PromptAnswer } from "../data/liveRecords";
import { postAnswer, postInput } from "../data/seat";
import { LivePrompt } from "./LivePrompt";

type Status =
  | { kind: "connecting" }
  | { kind: "live" }
  | { kind: "reconnecting" }
  | { kind: "ended"; sentence: string };

const STALE_AFTER_MS = 5000;
const LOST_WHEEL = "The wheel is no longer yours — watching only.";
const NOT_HOLDER = "This view does not hold the seat — give it back or close it.";
const ANSWER_FAILED = "The answer could not be sent.";
const ANSWER_TOO_LARGE = "Those files are too large together.";

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
 * A live view of one browser session: the daemon's frame stream, shown as
 * it arrives. It streams only while mounted and visible, says in a sentence
 * why it stopped when it stops for good, and reconnects with backoff when the
 * connection merely dropped.
 *
 * Given a seat `nonce` it also drives the page: mapped mouse and wheel, the
 * keyboard while focused, pasted and composed text, and the page's prompts.
 * `driven` says the session is driven by this shell although the nonce is
 * gone (a reload), so the view can say why it cannot drive.
 */
export function LiveView({
  sessionId,
  nonce,
  driven = false,
}: {
  sessionId: number;
  nonce?: string | null;
  driven?: boolean;
}) {
  const visible = useVisible();
  const [status, setStatus] = useState<Status>({ kind: "connecting" });
  const [src, setSrc] = useState<string | null>(null);
  const [lastFrameAt, setLastFrameAt] = useState<number | null>(null);
  const [now, setNow] = useState(() => Date.now());
  const [dropped, setDropped] = useState(false);
  const [notice, setNotice] = useState<string | null>(null);
  const [answering, setAnswering] = useState(0);
  const [prompts, setPrompts] = useState<Map<string, OpenPrompt>>(() => new Map());
  const [wrapper, setWrapper] = useState<HTMLDivElement | null>(null);

  const drive = typeof nonce === "string" && !dropped;
  const driveRef = useRef(drive);
  driveRef.current = drive;
  const metaRef = useRef<FrameMeta | null>(null);
  const imgRef = useRef<HTMLImageElement | null>(null);
  const batcherRef = useRef<InputBatcher | null>(null);

  // A new session or a new seat starts from a clean slate.
  useEffect(() => {
    setDropped(false);
    setNotice(null);
    setAnswering(0);
  }, [sessionId, nonce]);

  // Leaving drive mode (or the session) closes every open prompt.
  useEffect(() => {
    if (!drive) setPrompts((current) => (current.size === 0 ? current : new Map()));
  }, [drive, sessionId]);

  const refused = (error: unknown) => {
    if (!isApiRefusal(error)) return;
    if (error.status === 409) {
      setDropped(true);
      setNotice(LOST_WHEEL);
    } else if (error.status === 403) {
      setDropped(true);
      setNotice(NOT_HOLDER);
    }
  };
  const refusedRef = useRef(refused);
  refusedRef.current = refused;

  useEffect(() => {
    if (!drive || typeof nonce !== "string") return;
    const batcher = new InputBatcher({
      send: (events) => postInput(sessionId, nonce, events),
      onError: (error) => refusedRef.current(error),
    });
    batcherRef.current = batcher;
    return () => {
      batcher.dispose();
      if (batcherRef.current === batcher) batcherRef.current = null;
    };
  }, [drive, sessionId, nonce]);

  const point = (clientX: number, clientY: number) => {
    const img = imgRef.current;
    if (!img) return null;
    return toPage(clientX, clientY, img.getBoundingClientRect(), metaRef.current);
  };
  const pointRef = useRef(point);
  pointRef.current = point;

  // React's wheel listener is passive, so the page would scroll under the
  // person: a native, non-passive one can cancel it.
  useEffect(() => {
    if (!drive || !wrapper) return;
    const onWheel = (e: WheelEvent) => {
      e.preventDefault();
      const at = pointRef.current(e.clientX, e.clientY);
      if (!at) return;
      const unit = e.deltaMode === 1 ? 40 : 1;
      batcherRef.current?.push({ kind: "wheel", x: at.x, y: at.y, dx: e.deltaX * unit, dy: e.deltaY * unit });
    };
    wrapper.addEventListener("wheel", onWheel, { passive: false });
    return () => wrapper.removeEventListener("wheel", onWheel);
  }, [drive, wrapper]);

  const mouse = (type: "mousePressed" | "mouseReleased" | "mouseMoved") => (e: MouseEvent) => {
    const at = point(e.clientX, e.clientY);
    if (!at) return;
    const event: InputEvent = {
      kind: "mouse",
      type,
      x: at.x,
      y: at.y,
      button: type === "mouseMoved" ? "none" : mouseButton(e.button),
      buttons: e.buttons,
      clickCount: type === "mouseMoved" ? 0 : Math.max(e.detail, 1),
      modifiers: modifiers(e),
    };
    batcherRef.current?.push(event);
  };

  const composing = (e: KeyboardEvent) => e.nativeEvent.isComposing || e.keyCode === 229;
  const onKey = (type: "keyDown" | "keyUp") => (e: KeyboardEvent) => {
    if (composing(e)) return;
    // Tab and Shift+Tab stay with the browser, so focus can always leave the view.
    if (e.key === "Tab") return;
    // The handler sits on the wrapper: only a key it holds focus for is the page's.
    if (document.activeElement !== e.currentTarget) return;
    // The paste shortcut arrives as a paste event, with the text in it.
    if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "v") return;
    e.preventDefault();
    batcherRef.current?.push(keyEvent(type, e));
  };
  const onPaste = (e: ClipboardEvent) => {
    e.preventDefault();
    const value = e.clipboardData.getData("text");
    if (value !== "") batcherRef.current?.push({ kind: "text", value });
  };
  const onCompositionEnd = (e: CompositionEvent) => {
    if (e.data !== "") batcherRef.current?.push({ kind: "text", value: e.data });
  };

  const answer = (prompt: OpenPrompt, result: PromptAnswer) => {
    if (typeof nonce !== "string") return;
    const remove = () =>
      setPrompts((current) => {
        const next = new Map(current);
        next.delete(prompt.id);
        return next;
      });
    setNotice((current) => (current === ANSWER_FAILED || current === ANSWER_TOO_LARGE ? null : current));
    setAnswering((n) => n + 1);
    postAnswer(sessionId, nonce, prompt.id, result)
      .then(remove, (error: unknown) => {
        if (isApiRefusal(error) && error.status === 404) {
          remove();
        } else if (isApiRefusal(error) && (error.status === 403 || error.status === 409)) {
          refusedRef.current(error);
        } else {
          setNotice(isApiRefusal(error) && error.status === 413 ? ANSWER_TOO_LARGE : ANSWER_FAILED);
        }
      })
      .finally(() => setAnswering((n) => Math.max(0, n - 1)));
  };

  useEffect(() => {
    if (!visible) return;
    const controller = new AbortController();
    const { signal } = controller;
    let current: string | null = null;

    const drop = () => {
      if (current !== null) URL.revokeObjectURL(current);
      current = null;
    };

    const end = (sentence: string) => {
      drop();
      setSrc(null);
      setStatus({ kind: "ended", sentence });
    };

    setStatus({ kind: "connecting" });
    metaRef.current = null;

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
            } else if (record.kind === "meta") {
              metaRef.current = record.meta;
            } else if (record.kind === "prompt") {
              if (!driveRef.current) continue;
              const one = record.prompt;
              setPrompts((existing) => {
                const next = new Map(existing);
                if ("resolved" in one) next.delete(one.id);
                else next.set(one.id, one);
                return next;
              });
            } else if (record.reason === "wheel") {
              end("The wheel was asked for, or a person has it.");
              return;
            } else if (record.reason === "closed") {
              end("Session closed.");
              return;
            } else {
              break;
            }
          }
        } catch (error) {
          if (signal.aborted) return;
          if (isApiRefusal(error)) {
            if (error.status === 409) {
              end(error.detail);
              return;
            }
            if (error.status === 404) {
              end("Session closed.");
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
  const shownNotice = notice ?? (driven && nonce === null ? NOT_HOLDER : null);
  const alt = `live view of session ${sessionId}`;

  return (
    <div className="browser-live">
      {src !== null && drive && (
        <div
          ref={setWrapper}
          className="browser-live-drive"
          tabIndex={0}
          aria-label={`drive session ${sessionId}`}
          onMouseDown={mouse("mousePressed")}
          onMouseUp={mouse("mouseReleased")}
          onMouseMove={mouse("mouseMoved")}
          onContextMenu={(e) => e.preventDefault()}
          onKeyDown={onKey("keyDown")}
          onKeyUp={onKey("keyUp")}
          onPaste={onPaste}
          onCompositionEnd={onCompositionEnd}
        >
          <img ref={imgRef} className="browser-live-frame" src={src} alt={alt} draggable={false} />
        </div>
      )}
      {src !== null && !drive && <img ref={imgRef} className="browser-live-frame" src={src} alt={alt} />}
      {drive &&
        [...prompts.values()].map((prompt) => (
          <LivePrompt key={prompt.id} prompt={prompt} onAnswer={(result) => answer(prompt, result)} pending={answering > 0} />
        ))}
      <div className="browser-live-status">
        {status.kind === "live" && <span className="browser-live-marker">live</span>}
        {stale && <span className="browser-live-quiet">{`unchanged for ${silentFor}s`}</span>}
        {status.kind === "connecting" && <span className="browser-live-quiet">connecting…</span>}
        {status.kind === "reconnecting" && <span className="browser-live-quiet">Connection lost — reconnecting</span>}
        {shownNotice !== null && <span className="browser-live-quiet">{shownNotice}</span>}
        {status.kind === "ended" && <span className="browser-live-end">{status.sentence}</span>}
      </div>
    </div>
  );
}
