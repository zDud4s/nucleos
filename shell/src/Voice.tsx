import { useCallback, useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import {
  deleteVoiceMemo, getVoiceConfig, listVoiceMemos, postVoiceCapture,
  type ConnectionState, type VoiceCapture, type VoiceConfigView, type VoiceKind,
} from "./api";
import { durationMs, encodeCapture } from "./audio";
import {
  concatSamples, relativeTime, spokenDuration, voiceCleanupLabel, voiceCleanupTone,
} from "./derive";
import { Badge, Button, ConfirmButton, ErrorNote, Panel } from "./ui";

/** What the shell's Rust side reports after being asked to type a transcript. */
interface Delivery {
  pasted: boolean;
  held: string | null;
}

type Phase = "idle" | "recording" | "transcribing";

/**
 * The live microphone graph. Held in a ref rather than in state because nothing about it is drawn and
 * every field has to be torn down by identity — a re-render that replaced any of them would leak a
 * stream and leave the microphone light on.
 */
interface Recorder {
  stream: MediaStream;
  context: AudioContext;
  node: ScriptProcessorNode;
  chunks: Float32Array[];
  frames: number;
  capFrames: number;
  capped: boolean;
}

/** Frames per callback. About 85 ms at 48 kHz — small enough to stop promptly, large enough that the
 *  main thread is not woken constantly during a twenty-minute memo. */
const CHUNK_FRAMES = 4096;

/**
 * What is in force, read from the daemon and owned by it.
 *
 * There is deliberately nothing editable here. `.ai/voice.yaml` names a program the daemon will spawn,
 * which is why `classifier.rs` lists it as self-governing: an edit to that file is an edit to what this
 * machine will execute, and it belongs behind the same approval as any other such change rather than
 * behind a text box in a window that is already authenticated.
 */
function Configuration({ config, refused }: { config: VoiceConfigView; refused: string[] }) {
  return (
    <Panel
      title="Configuration"
      aside={<Badge tone={config.armed ? "active" : "off"}>{config.armed ? "armed" : "off"}</Badge>}
    >
      {!config.armed && (
        <p className="a-note">
          Voice is off. It arms when <code>.ai/voice.yaml</code> sets <code>enabled: true</code> AND
          names an <code>stt_command</code> — enabling it without a transcriber stays off on purpose,
          because a hotkey that records into nothing is worse than one that never fires.
        </p>
      )}
      <dl className="v-kv">
        <dt>Dictation hotkey</dt>
        <dd>{config.hotkey === "" ? "unset" : <code>{config.hotkey}</code>}</dd>
        <dt>Memo hotkey</dt>
        <dd>{config.memo_hotkey === "" ? "unset" : <code>{config.memo_hotkey}</code>}</dd>
        <dt>Cleanup model</dt>
        <dd>{config.cleanup_model ?? "none — transcripts are kept as spoken"}</dd>
        <dt>Dictation retention</dt>
        <dd>{config.retain_dictations_days} days</dd>
        <dt>Longest capture</dt>
        <dd>{spokenDuration(config.max_capture_seconds * 1000)}</dd>
        <dt>Misheard-word hints</dt>
        <dd>{config.hints.length === 0 ? "none" : config.hints.join(", ")}</dd>
      </dl>
      {refused.length > 0 && (
        <ErrorNote>
          Another application already owns {refused.join(" and ")}, so that hotkey is not listening.
          Change it in <code>.ai/voice.yaml</code>, or use the buttons above — dictation still works
          without a hotkey.
        </ErrorNote>
      )}
      <p className="a-note">
        Memos are kept until you delete them; dictations expire on the retention above. Audio is never
        stored at all — the recording is deleted as soon as it has been transcribed, including when the
        request that carried it is abandoned.
      </p>
      <details>
        <summary>Cleanup prompt</summary>
        <pre className="v-prompt">{config.cleanup_prompt}</pre>
      </details>
    </Panel>
  );
}

/**
 * The memos, newest first.
 *
 * `raw_text` is shown whenever there is no cleaned text rather than showing nothing: a refused or
 * unattempted cleanup still leaves a complete transcript, and hiding it would lose the one thing the
 * capture was for.
 */
function Memos({
  memos, busy, onDelete,
}: {
  memos: VoiceCapture[];
  busy: number | null;
  onDelete: (id: number) => void;
}) {
  if (memos.length === 0) {
    return (
      <Panel title="Memos">
        <p className="a-note">
          No memos yet. A memo is a long dictation kept as a document — it is saved rather than typed,
          and it survives the client disconnecting mid-transcription, unlike a quick dictation, which is
          discarded with its request.
        </p>
      </Panel>
    );
  }

  return (
    <Panel title="Memos" aside={<span className="v-count">{memos.length}</span>}>
      <ul className="v-memos">
        {memos.map((memo) => (
          <li key={memo.id}>
            <header>
              <Badge tone={voiceCleanupTone(memo.cleanup_state)}>
                {voiceCleanupLabel(memo.cleanup_state)}
              </Badge>
              <span className="v-when">{relativeTime(memo.created_at)}</span>
              <span className="v-len">{spokenDuration(memo.duration_ms)}</span>
              {memo.model !== null && <span className="v-model">{memo.model}</span>}
              <ConfirmButton
                variant="danger"
                size="sm"
                confirmLabel="Delete for good?"
                disabled={busy === memo.id}
                onConfirm={() => onDelete(memo.id)}
              >
                Delete
              </ConfirmButton>
            </header>
            <p className="v-text">{memo.clean_text ?? memo.raw_text}</p>
          </li>
        ))}
      </ul>
    </Panel>
  );
}

/**
 * The voice pillar's window: dictate, and read what has been said.
 *
 * The microphone is opened here rather than in the shell's Rust side because `cpal` cannot currently
 * be built alongside Tauri, and the samples stay here because Tauri's IPC would serialise them as
 * JSON. So this page captures, encodes and posts; the Rust side owns the hotkey and the paste, which
 * are the parts that reach into another application.
 */
export default function Voice({
  token, connection,
}: {
  token: string | null;
  connection: ConnectionState;
}) {
  const [config, setConfig] = useState<VoiceConfigView | null>(null);
  const [memos, setMemos] = useState<VoiceCapture[] | null>(null);
  const [loading, setLoading] = useState(true);
  const [busy, setBusy] = useState<number | null>(null);
  const [phase, setPhase] = useState<Phase>("idle");
  const [note, setNote] = useState<string | null>(null);
  const [lastText, setLastText] = useState<string | null>(null);
  const [refused, setRefused] = useState<string[]>([]);

  const recorder = useRef<Recorder | null>(null);
  /** The latest config, for the event handlers — they are registered once and must not capture a
   *  stale cap or a stale token. */
  const live = useRef<{ config: VoiceConfigView | null; token: string | null }>({
    config: null,
    token: null,
  });
  live.current = { config, token };

  const load = useCallback(async () => {
    if (token === null) return;
    const [nextConfig, nextMemos] = await Promise.all([
      getVoiceConfig(token),
      listVoiceMemos(token),
    ]);
    setConfig(nextConfig);
    setMemos(nextMemos);
    setLoading(false);
  }, [token]);

  useEffect(() => {
    if (token === null || connection !== "connected") return;
    let cancelled = false;
    const tick = () => {
      void load().then(() => {
        if (cancelled) return;
      });
    };
    tick();
    // Captures can land from outside this window — anything holding a token may POST one — so the list
    // is polled rather than only refreshed by our own deletes.
    const id = setInterval(tick, 5000);
    return () => {
      cancelled = true;
      clearInterval(id);
    };
  }, [connection, load, token]);

  /**
   * Hands the daemon's configured chords to the Rust side.
   *
   * Registered from here because this is the only place they exist: the daemon read them from
   * `.ai/voice.yaml`, and the shell may not read that file itself.
   */
  useEffect(() => {
    if (config === null || !config.armed) return;
    void invoke<string[]>("voice_register_hotkeys", {
      dictation: config.hotkey,
      memo: config.memo_hotkey,
    })
      .then(setRefused)
      .catch(() => setRefused([]));
  }, [config]);

  /** Tears the microphone down and returns what it captured. Safe to call twice. */
  const closeRecorder = useCallback((): { samples: Float32Array; rate: number } | null => {
    const active = recorder.current;
    recorder.current = null;
    if (active === null) return null;
    // Order matters: silence the callback before disconnecting, or a chunk can arrive after teardown
    // has begun and be appended to a list nobody will read.
    active.node.onaudioprocess = null;
    active.node.disconnect();
    active.stream.getTracks().forEach((track) => track.stop());
    const rate = active.context.sampleRate;
    void active.context.close();
    return { samples: concatSamples(active.chunks), rate };
  }, []);

  const startRecording = useCallback(async () => {
    // UNVERIFIED ON A REAL WINDOW: WebView2 raises its own `PermissionRequested` event for microphone
    // access, and a Tauri host that does not handle it can leave `getUserMedia` rejecting rather than
    // prompting. Nothing here can test that without a desktop, so the rejection is surfaced verbatim to
    // the person instead of being swallowed — if this is what happens, the note will say so, and the
    // fix is a permission handler in the Rust host rather than anything on this side.
    const capSeconds = live.current.config?.max_capture_seconds ?? 1200;
    // Mono at the source where the device allows it, which makes the downmix a no-op and halves what
    // crosses into JS. `channelCount` is a hint, so the conversion still handles stereo.
    const stream = await navigator.mediaDevices.getUserMedia({
      audio: { channelCount: 1, echoCancellation: true, noiseSuppression: true },
    });
    const context = new AudioContext();
    const source = context.createMediaStreamSource(stream);
    // ScriptProcessorNode, though deprecated, rather than an AudioWorklet: a worklet's code has to be
    // fetched as a separate module at runtime, which this app's CSP does not allow and which a bundled
    // desktop app should not need. Speech at this chunk size does not stress the main thread.
    const node = context.createScriptProcessor(CHUNK_FRAMES, 1, 1);
    const active: Recorder = {
      stream,
      context,
      node,
      chunks: [],
      frames: 0,
      capFrames: capSeconds * context.sampleRate,
      capped: false,
    };
    node.onaudioprocess = (event) => {
      if (active.capped) return;
      if (active.frames >= active.capFrames) {
        // Stop accumulating at the cap rather than trying to submit from inside an audio callback.
        // The daemon refuses anything longer, so going past it would only guarantee a rejection.
        active.capped = true;
        return;
      }
      // Copied, because the callback reuses its buffer for the next chunk.
      const chunk = new Float32Array(event.inputBuffer.getChannelData(0));
      active.chunks.push(chunk);
      active.frames += chunk.length;
    };
    // A gain of zero on the way to the destination: the processor only runs while connected to one,
    // and connecting it directly would play the microphone back through the speakers.
    const mute = context.createGain();
    mute.gain.value = 0;
    source.connect(node);
    node.connect(mute);
    mute.connect(context.destination);
    recorder.current = active;
  }, []);

  const stopAndSend = useCallback(
    async (kind: VoiceKind) => {
      const captured = closeRecorder();
      const authToken = live.current.token;
      if (captured === null || authToken === null) {
        await invoke("voice_abandon").catch(() => undefined);
        setPhase("idle");
        return;
      }
      if (captured.samples.length === 0) {
        await invoke("voice_abandon").catch(() => undefined);
        setPhase("idle");
        setNote("Nothing was recorded — the microphone produced no audio.");
        return;
      }

      setPhase("transcribing");
      setNote(null);
      const wav = encodeCapture(captured.samples, 1, captured.rate);
      const millis = durationMs(captured.samples.length, captured.rate);
      const result = await postVoiceCapture(authToken, kind, millis, wav);

      if (result === "silent") {
        await invoke("voice_abandon").catch(() => undefined);
        setNote("The transcriber heard nothing in that recording.");
      } else if (result === "failed") {
        await invoke("voice_abandon").catch(() => undefined);
        setNote("The daemon refused the recording. Check that voice is armed and the daemon is up.");
      } else {
        setLastText(result.text);
        if (kind === "memo") {
          // §5 gives a memo no delivery step: the daemon has persisted a document, and this tab is
          // where it is read. Typing it into whatever is in front would be the wrong thing.
          await invoke("voice_abandon").catch(() => undefined);
          setNote("Memo saved.");
        } else {
          const delivery = await invoke<Delivery>("voice_paste", { text: result.text }).catch(
            (error: unknown) => ({ pasted: false, held: String(error) }) as Delivery,
          );
          setNote(
            delivery.pasted
              ? null
              : `Not typed — ${delivery.held ?? "unknown reason"}. The text is below.`,
          );
        }
      }
      setPhase("idle");
      void load();
    },
    [closeRecorder, load],
  );

  /**
   * The hotkey fires in the Rust side, which owns the state machine; this only opens and closes the
   * device. Registered once, reading the current token and config through `live` so it never acts on a
   * stale copy.
   */
  useEffect(() => {
    let kind: VoiceKind = "dictation";
    const started = listen<VoiceKind>("voice://start", (event) => {
      kind = event.payload;
      setNote(null);
      setPhase("recording");
      void startRecording().catch(async (error: unknown) => {
        await invoke("voice_abandon").catch(() => undefined);
        setPhase("idle");
        setNote(`The microphone could not be opened — ${String(error)}`);
      });
    });
    const stopped = listen("voice://stop", () => {
      void stopAndSend(kind);
    });
    return () => {
      void started.then((un) => un());
      void stopped.then((un) => un());
      closeRecorder();
    };
  }, [closeRecorder, startRecording, stopAndSend]);

  const press = useCallback((memo: boolean) => {
    void invoke("voice_hotkey", { memo }).catch((error: unknown) => setNote(String(error)));
  }, []);

  const remove = useCallback(
    (id: number) => {
      if (token === null) return;
      setBusy(id);
      void (async () => {
        await deleteVoiceMemo(token, id);
        // Reload rather than splice: a failed delete and a successful one must not look different, and
        // what the daemon has is the only answer worth drawing.
        await load();
        setBusy(null);
      })();
    },
    [load, token],
  );

  if (token === null) {
    return (
      <section className="page-voice">
        <ErrorNote>The daemon token has not been read yet, so voice cannot be queried.</ErrorNote>
      </section>
    );
  }

  const armed = config?.armed === true;

  return (
    <section className="page-voice">
      {loading && config === null && <p className="a-note">Reading the voice configuration…</p>}
      {!loading && config === null && (
        <ErrorNote>
          Could not read the voice configuration. The daemon may predate the voice pillar.
        </ErrorNote>
      )}

      {armed && (
        <Panel
          title="Dictate"
          aside={<Badge tone={phase === "idle" ? "off" : "active"}>{phase}</Badge>}
        >
          <div className="v-actions">
            <Button
              variant="approve"
              disabled={phase === "transcribing"}
              onClick={() => press(false)}
            >
              {phase === "recording" ? "Stop and type" : "Dictate"}
            </Button>
            <Button disabled={phase !== "idle"} onClick={() => press(true)}>
              Record a memo
            </Button>
          </div>
          <p className="a-note">
            A dictation is typed into whatever window was in front when you started — and only into
            that one. If you switch windows while it transcribes, or hold a modifier key when it
            finishes, the text is kept here instead of being typed somewhere you did not choose.
          </p>
          {note !== null && <ErrorNote>{note}</ErrorNote>}
          {lastText !== null && <p className="v-text">{lastText}</p>}
        </Panel>
      )}

      {config !== null && <Configuration config={config} refused={refused} />}
      {memos === null
        ? !loading && <ErrorNote>Could not list the memos.</ErrorNote>
        : <Memos memos={memos} busy={busy} onDelete={remove} />}
    </section>
  );
}
