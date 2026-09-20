/**
 * An open microphone that reports, frame by frame, when somebody started and stopped talking.
 *
 * **Not `lib/capture.ts`, and the difference is the whole reason this exists.** That one opens the
 * device, keeps everything, and hands it over when it is closed — which is all a press-to-dictate
 * needs and is what its own header says it is for. This one is read WHILE it records: a gate decides
 * mid-recording that a sentence ended, and something else decides what to do about that. Same three
 * nodes, genuinely different job, as `lib/capture.ts:15-17` said before this file existed.
 *
 * **Why it is its own module rather than part of either caller.** It was part of
 * `data/conversation.ts` until 2026-09-20, when progressive dictation needed the same ears without
 * the mouth: `useVoiceConversation` hears a sentence, sends it as a turn and plays the answer, and
 * the only thing joining those two halves is the edge from a closed turn to a sent one. Copying the
 * listening half into `data/dictation.ts` would have made a second copy of four things that are
 * invisible from the outside and wrong in silence — the zero-gain sink that keeps `onaudioprocess`
 * firing without playing the microphone out loud, the pre-roll that saves the first syllable, the
 * single-file chain that keeps Silero's sequential state honest, and the rule that drops frames
 * rather than queueing them when the runtime falls behind. Each has its reasoning beside it here,
 * once.
 *
 * What it does NOT own: what a sentence is worth, what to do with one, and whether to record at all.
 * The caller starts and stops the recording and reads it whenever it likes.
 */

import { useCallback, useEffect, useRef } from "react";

import { durationMs } from "../lib/audio";
import { loadSileroSession, SpeechProbe } from "../lib/silero";
import {
  energyOf,
  FRAME_SAMPLES,
  GateState,
  IDLE_GATE,
  onFrame,
  PREROLL_FRAMES,
} from "../lib/vad";

/**
 * Samples per `onaudioprocess` callback.
 *
 * A multiple of `FRAME_SAMPLES` so a callback splits into whole frames with nothing left over. A
 * remainder would have to be carried between callbacks, and a carry that is ever dropped shifts every
 * later frame — which reads as the gate becoming erratic rather than as an arithmetic mistake.
 */
const BUFFER_SAMPLES = FRAME_SAMPLES * 8;

/** What the microphone has kept, at the device's own rate. */
export interface Recorded {
  samples: Float32Array;
  rate: number;
  ms: number;
}

export interface ListeningHandlers {
  /**
   * Every frame, synchronously, before the pre-roll and the gate see it.
   *
   * `recording` says whether this frame went into the buffer. Returning `false` drops the frame
   * entirely — `useVoiceConversation` uses that to abandon a turn that has been idle too long, where
   * letting the frame through would let the gate open a segment on the way out.
   */
  onFrame?: (recording: boolean) => boolean | void;
  /** Somebody started or stopped talking. */
  onSignal: (signal: "speechStarted" | "speechEnded") => void;
  /** The microphone could not be opened, in words fit to show somebody. */
  onTrouble: (sentence: string) => void;
  /**
   * Which detector this session got, and the runtime's own words for why not the good one.
   *
   * Reported rather than kept private because the two behave visibly differently: `"energy"` opens a
   * sentence on a fan or a fridge, and somebody watching that happen deserves to know it is the
   * fallback talking rather than the feature being broken. Called once, on the first open.
   */
  onDetector?: (using: "silero" | "energy", why: string | null) => void;
}

/** An open microphone, and the handles for what a caller does with it. */
export interface Listening {
  /**
   * Opens the device and starts deciding. `false` when it could not be opened — `onTrouble` has
   * already said why, and the caller is the one that knows what its mode should do about it.
   *
   * `constraints` is the caller's, not a default with an override: a dictation wants the device raw,
   * and anything recording while audio plays back needs `echoCancellation` or it records the answer.
   */
  open: (constraints?: MediaTrackConstraints | boolean) => Promise<boolean>;
  close: () => void;
  /** Start keeping frames, seeded with the pre-roll — see `PREROLL_FRAMES` for why that matters. */
  record: () => void;
  /** Everything kept so far, without stopping. `null` when nothing is being kept. */
  peek: () => Recorded | null;
  /** Everything kept so far, and stop keeping. `null` when nothing was being kept. */
  take: () => Recorded | null;
  isRecording: () => boolean;
}

interface Live {
  stream: MediaStream;
  context: AudioContext;
  source: MediaStreamAudioSourceNode;
  processor: ScriptProcessorNode;
  sink: GainNode;
}

export function useListening(handlers: ListeningHandlers): Listening {
  /* The handlers are rebuilt on every render — they close over the state their caller is showing —
     while the audio graph outlives many of them. Captured, `onaudioprocess` would hold the first
     render's closure for the life of the microphone. */
  const handlersRef = useRef(handlers);
  handlersRef.current = handlers;

  const liveRef = useRef<Live | null>(null);
  const gateRef = useRef<GateState>(IDLE_GATE);
  /** The last `PREROLL_FRAMES` frames, kept always — see `PREROLL_FRAMES` for why. */
  const prerollRef = useRef<Float32Array[]>([]);
  const recordingRef = useRef<Float32Array[] | null>(null);
  /** `null` until the model has been loaded, and permanently so if it could not be. */
  const probeRef = useRef<SpeechProbe | null>(null);
  /**
   * Inference is async and Silero's state is sequential, so frames are run one at a time in order.
   * Two in flight would interleave their state updates and the model's memory would describe audio
   * that never happened.
   */
  const chainRef = useRef<Promise<void>>(Promise.resolve());
  /** How many frames are waiting on the chain, so a slow runtime cannot grow it without bound. */
  const pendingRef = useRef(0);
  /**
   * Which detector was settled on, so the load is attempted once and not once per open.
   *
   * A ref and not state because it is read inside `open`, which is a stable callback: reading state
   * there would read whatever it was when the callback was built, which is `null` forever.
   */
  const usingRef = useRef<"silero" | "energy" | null>(null);

  const close = useCallback(() => {
    const live = liveRef.current;
    liveRef.current = null;
    recordingRef.current = null;
    prerollRef.current = [];
    gateRef.current = IDLE_GATE;
    // Silero's state is a memory of what it just heard. Carried across a closed microphone, the next
    // session would start mid-thought about a sentence from the last one.
    probeRef.current?.reset();
    pendingRef.current = 0;
    if (live === null) return;
    live.processor.disconnect();
    live.source.disconnect();
    live.sink.disconnect();
    for (const track of live.stream.getTracks()) track.stop();
    void live.context.close();
  }, []);

  /* An open microphone must not survive the thing it belongs to. Leaving the graph up on unmount
     leaves the device light on with nothing listening — and on a chat's front door, unmounting is
     what happens the moment the first message opens a conversation. */
  useEffect(() => close, [close]);

  /**
   * One frame, through the gate, into whatever the caller makes of it.
   *
   * Assigned once when the graph is built, so everything it reads is a ref.
   */
  const onAudioFrame = useCallback((frame: Float32Array) => {
    // Synchronous and first, because these two are what the recording IS. Deferring them behind the
    // probe below would put the audio's order at the mercy of how fast inference happens to be.
    const recording = recordingRef.current;
    if (handlersRef.current.onFrame?.(recording !== null) === false) return;
    if (recording !== null) recording.push(frame);

    prerollRef.current.push(frame);
    if (prerollRef.current.length > PREROLL_FRAMES) prerollRef.current.shift();

    const decide = (probability: number) => {
      const { state, signal } = onFrame(gateRef.current, probability);
      gateRef.current = state;
      if (signal !== null) handlersRef.current.onSignal(signal);
    };

    const probe = probeRef.current;
    if (probe === null) {
      decide(energyOf(frame));
      return;
    }

    // Dropped rather than queued once the chain is behind. A frame decided three frames late is a
    // barge-in noticed a tenth of a second late, and every one kept makes the next one later still —
    // the lag would grow for as long as the runtime stayed slow, and never recover.
    if (pendingRef.current > 3) return;

    pendingRef.current += 1;
    chainRef.current = chainRef.current
      .then(() => probe.probability(frame))
      .then(decide)
      // One frame that failed is one frame of silence, not a broken mode. A runtime that fails every
      // frame presents as a gate that never opens, which is what the fallback above is for.
      .catch(() => undefined)
      .finally(() => {
        pendingRef.current -= 1;
      });
  }, []);

  const open = useCallback(
    async (constraints: MediaTrackConstraints | boolean = true): Promise<boolean> => {
      try {
        // Loaded once per process and kept. 13 MB of WebAssembly and a 2.3 MB model, so paying for
        // it on every open would put a second of dead air at the front of every session.
        if (probeRef.current === null && usingRef.current === null) {
          const { session, why } = await loadSileroSession();
          probeRef.current = session === null ? null : new SpeechProbe(session);
          usingRef.current = session === null ? "energy" : "silero";
          handlersRef.current.onDetector?.(usingRef.current, why);
        }
        const stream = await navigator.mediaDevices.getUserMedia({ audio: constraints });
        const AudioContextCtor =
          window.AudioContext ??
          (window as unknown as { webkitAudioContext: typeof AudioContext }).webkitAudioContext;
        const context = new AudioContextCtor();
        const source = context.createMediaStreamSource(stream);
        const processor = context.createScriptProcessor(BUFFER_SAMPLES, 1, 1);
        // Zero gain, for the reason `lib/capture.ts` records at length: the graph has to reach
        // `destination` for `onaudioprocess` to fire at all, and a live path there plays the
        // microphone out loud while it records. Invisible in jsdom, which has neither API.
        const sink = context.createGain();
        sink.gain.value = 0;

        processor.onaudioprocess = (event) => {
          const buffer = event.inputBuffer.getChannelData(0);
          for (let at = 0; at + FRAME_SAMPLES <= buffer.length; at += FRAME_SAMPLES) {
            onAudioFrame(new Float32Array(buffer.subarray(at, at + FRAME_SAMPLES)));
          }
        };
        source.connect(processor);
        processor.connect(sink);
        sink.connect(context.destination);
        liveRef.current = { stream, context, source, processor, sink };
        return true;
      } catch {
        handlersRef.current.onTrouble("the microphone could not be opened");
        return false;
      }
    },
    [onAudioFrame],
  );

  const record = useCallback(() => {
    // Seeded with the pre-roll, so the recording contains the syllable the gate needed in order to
    // decide there was one.
    recordingRef.current = [...prerollRef.current];
  }, []);

  const gather = useCallback((frames: Float32Array[]): Recorded => {
    const samples = concat(frames);
    const rate = liveRef.current?.context.sampleRate ?? 16000;
    return { samples, rate, ms: durationMs(samples.length, rate) };
  }, []);

  const peek = useCallback(() => {
    const frames = recordingRef.current;
    return frames === null ? null : gather(frames);
  }, [gather]);

  const take = useCallback(() => {
    const frames = recordingRef.current;
    recordingRef.current = null;
    return frames === null ? null : gather(frames);
  }, [gather]);

  const isRecording = useCallback(() => recordingRef.current !== null, []);

  return { open, close, record, peek, take, isRecording };
}

function concat(frames: Float32Array[]): Float32Array {
  const total = frames.reduce((sum, frame) => sum + frame.length, 0);
  const out = new Float32Array(total);
  let at = 0;
  for (const frame of frames) {
    out.set(frame, at);
    at += frame.length;
  }
  return out;
}
