import {
  createContext,
  useContext,
  useEffect,
  useRef,
  useState,
  type ReactNode,
} from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import {
  finishCapture,
  startCapture,
  type ActiveCapture,
} from "../lib/capture";
import {
  phaseAfter,
  postCapture,
  type CaptureResult,
  type VoicePhase,
} from "../data/voice";

/**
 * The dictation and memo capture, held by the shell rather than by the Voice page.
 *
 * The two chords are global at the operating system: the host answers them from any window, in any
 * application, with `voice://start` and `voice://stop`. Until 2026-09-21 the only listener was the
 * Voice page, so pressed anywhere else the host moved to `recording` with no microphone open, and the
 * second press left it in `transcribing` for good — only `voice_paste` or `voice_abandon` bring it
 * back, and nothing was there to send either. A dictation is pasted into whatever application has
 * focus, so the answer is not the conversation chord's (`ConversationChord.tsx` takes the person to
 * the Voice page): moving this window to another page is no part of dictating into a text editor. So
 * the microphone, the POST and the paste live here, mounted on every page, and the Voice page draws
 * this state and presses the same controls.
 *
 * **The split held throughout**: the WEBVIEW owns the microphone and the POST — using
 * `lib/capture.ts` — and the HOST owns the tray icon and the paste, which is why a finished
 * dictation's text crosses back out through `invoke("voice_paste", …)` rather than being typed by the
 * webview itself. Audio bytes cannot cross that boundary the other direction either: Tauri IPC
 * serialises arguments as JSON, so recording happens here and only the already-encoded bytes ever
 * leave via `fetch`.
 *
 * `invoke("voice_hotkey", { memo })` is the page's capture buttons standing in for that same chord
 * while the window IS focused; its resolved phase decides whether to start recording or to stop and
 * post, via `phaseAfter`, the one place that decision is made.
 */

/** The open microphone, plus the one thing worth remembering about it: which list it is for. */
type ActiveRecording = ActiveCapture & { kind: "dictation" | "memo" };

/** What a finished attempt at a capture came back as — read once, shown once, replaced by the next attempt. */
export type CaptureOutcome =
  | { kind: "silent" }
  | { kind: "done"; result: CaptureResult }
  | { kind: "refused"; error: unknown }
  | { kind: "mic-error" };

export type Delivery = { pasted: boolean; held: string | null };

export interface Dictation {
  phase: VoicePhase;
  activeKind: "dictation" | "memo" | null;
  /** A button press is waiting on the host. */
  pending: boolean;
  outcome: CaptureOutcome | null;
  delivery: Delivery | null;
  /** The page's own buttons: the same toggle the chord is, for the window that has focus. */
  press: (memo: boolean) => Promise<void>;
  abandon: () => Promise<void>;
}

const DictationContext = createContext<Dictation | null>(null);

export function useDictation(): Dictation {
  const dictation = useContext(DictationContext);
  if (dictation === null)
    throw new Error("useDictation needs a DictationProvider above it");
  return dictation;
}

export function DictationProvider({ children }: { children: ReactNode }) {
  const [phase, setPhase] = useState<VoicePhase>("idle");
  const [activeKind, setActiveKind] = useState<"dictation" | "memo" | null>(
    null,
  );
  const [pending, setPending] = useState(false);
  const [outcome, setOutcome] = useState<CaptureOutcome | null>(null);
  const [delivery, setDelivery] = useState<Delivery | null>(null);

  const captureRef = useRef<ActiveRecording | null>(null);

  /** The phase the host already holds, in case it is mid-capture from before this window loaded. */
  useEffect(() => {
    let cancelled = false;
    // Started from a resolved promise so that a host answering with anything but a promise is caught
    // below too: this runs on every page, and a throw here would take the whole window with it.
    Promise.resolve()
      .then(() => invoke<VoicePhase>("voice_phase"))
      .then((initial) => {
        if (!cancelled && initial !== undefined) setPhase(initial);
      })
      .catch(() => {
        // No reading is not a reason to invent one — idle is the safe assumption.
      });
    return () => {
      cancelled = true;
    };
  }, []);

  async function beginRecording(kind: "dictation" | "memo") {
    setOutcome(null);
    setDelivery(null);
    try {
      captureRef.current = { ...(await startCapture()), kind };
      setActiveKind(kind);
      setPhase(phaseAfter({ type: "start", kind }));
    } catch {
      setOutcome({ kind: "mic-error" });
    }
  }

  async function endRecordingAndCapture() {
    const active = captureRef.current;
    if (active === null) return;
    captureRef.current = null;
    setPhase(phaseAfter({ type: "stop" }));

    const { bytes, ms } = await finishCapture(active);

    try {
      const result = await postCapture(bytes, active.kind, ms);
      if (result === undefined) {
        // The one route in this shell where a success-family status is a
        // negative answer — see `data/voice.ts`'s header.
        setOutcome({ kind: "silent" });
      } else {
        setOutcome({ kind: "done", result });
        if (active.kind === "dictation") {
          await deliverPaste(result.text);
        }
      }
      setPhase(phaseAfter({ type: "capture-done" }));
    } catch (error) {
      setOutcome({ kind: "refused", error });
      setPhase(phaseAfter({ type: "capture-error" }));
    } finally {
      setActiveKind(null);
    }
  }

  /** The host owns the paste — this only hands the finished text across and reads back whether it landed. */
  async function deliverPaste(text: string) {
    try {
      const result = await invoke<Delivery>("voice_paste", { text });
      setDelivery(result);
    } catch {
      // The host did not answer at all — distinct from `held`, which is the
      // host answering with a named reason. Neither is one of the sentences
      // the host sends verbatim, so this is not shown as one.
      setDelivery(null);
    }
  }

  async function abandon() {
    const active = captureRef.current;
    captureRef.current = null;
    if (active !== null) {
      active.processor.disconnect();
      active.source.disconnect();
      active.sink.disconnect();
      for (const track of active.stream.getTracks()) track.stop();
      await active.context.close();
    }
    try {
      await invoke("voice_abandon");
    } catch {
      // Best-effort — the host may already have nothing to abandon either.
    }
    setOutcome(null);
    setDelivery(null);
    setActiveKind(null);
    setPhase(phaseAfter({ type: "abandon" }));
  }

  async function press(memo: boolean) {
    if (pending) return;
    setPending(true);
    try {
      const result = await invoke<"recording" | "transcribing" | "busy">(
        "voice_hotkey",
        { memo },
      );
      if (result === "recording") {
        await beginRecording(memo ? "memo" : "dictation");
      } else if (result === "transcribing") {
        await endRecordingAndCapture();
      } else {
        setPhase(phaseAfter({ type: "hotkey", phase: "busy" }));
      }
    } catch (error) {
      setOutcome({ kind: "refused", error });
    } finally {
      setPending(false);
    }
  }

  /** The real chords — fired by the host whether or not this window is focused, on whatever page. */
  useEffect(() => {
    let unlistenStart: (() => void) | undefined;
    let unlistenStop: (() => void) | undefined;
    let gone = false;

    Promise.resolve()
      .then(() =>
        listen<"dictation" | "memo">("voice://start", (event) => {
          setActiveKind(event.payload);
          void beginRecording(event.payload);
        }),
      )
      .then((fn) => {
        if (gone) fn?.();
        else unlistenStart = fn;
      })
      .catch(() => undefined);

    Promise.resolve()
      .then(() =>
        listen("voice://stop", () => {
          void endRecordingAndCapture();
        }),
      )
      .then((fn) => {
        if (gone) fn?.();
        else unlistenStop = fn;
      })
      .catch(() => undefined);

    return () => {
      gone = true;
      unlistenStart?.();
      unlistenStop?.();
    };
    // Registered once: both handlers close only over refs and setState
    // setters, which are stable across renders.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  return (
    <DictationContext.Provider
      value={{ phase, activeKind, pending, outcome, delivery, press, abandon }}
    >
      {children}
    </DictationContext.Provider>
  );
}
