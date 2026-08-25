/**
 * Silero VAD: the answer to "is this speech", which is the question `vad.ts` actually wants asked.
 *
 * `energyOf` answers "is this loud", and the gap between the two is measured rather than asserted.
 * Run against this repository's own Piper output and three kinds of noise at the same amplitude:
 *
 * | | Silero, mean | `energyOf` |
 * |---|---|---|
 * | speech | **0.961** | above the threshold |
 * | silence | 0.006 | below |
 * | steady hum, amp 0.2 | **0.001** | **above the threshold** |
 * | white noise, amp 0.2 | **0.011** | **above the threshold** |
 *
 * The last two rows are the whole reason this file exists: a fan, a fridge and a hard drive all clear
 * an energy threshold, and a gate driven by one opens turns on an empty room.
 *
 * **The split this file preserves.** It supplies a probability and nothing else. When speech starts
 * and stops is still `vad.ts`'s decision — the hysteresis, the minimum, the hangover — because those
 * are where the conversational failures live and they are testable with an array of numbers. Swapping
 * the source was designed to be a one-function change, and this is that function.
 */

/**
 * Samples the model is fed per call: the 512-sample hop plus 64 of carried context.
 *
 * **512 alone is accepted and silently wrong.** Measured against the same speech that scores 0.961
 * with context, a bare 512-sample window peaks at 0.234 and averages 0.001 — no error, no warning,
 * just a model that never fires. That is the worst shape a bug can have here: it looks exactly like a
 * microphone that is not working, and it would be chased in the audio graph for a day.
 *
 * (For completeness, since the numbers say something a comment otherwise could not: 1024 and 1536
 * both fail LOUDLY, with an ONNX shape error. Only the near-miss is silent.)
 */
export const CONTEXT_SAMPLES = 64;
export const WINDOW_SAMPLES = 512 + CONTEXT_SAMPLES;

/** Shape of the recurrent state the model threads between calls: `[2, 1, 128]`. */
export const STATE_SIZE = 2 * 1 * 128;

/**
 * The one thing this needs from an inference runtime.
 *
 * An interface rather than onnxruntime's own types, for the reason `core/src/transcribe.rs` gives
 * about `Transcriber`: it is what lets the rules above be tested without standing up the engine, and
 * what keeps a change of runtime from reaching anything but the factory below.
 */
export interface VadSession {
  infer(
    window: Float32Array,
    state: Float32Array,
  ): Promise<{ probability: number; state: Float32Array }>;
}

/**
 * Turns a stream of 512-sample frames into a stream of speech probabilities.
 *
 * Holds the two things the model cannot: the recurrent state, and the last 64 samples of the previous
 * frame. Both are per-conversation and must be reset when the microphone closes — a state carried
 * across a gap of minutes describes audio that is no longer related to what is arriving.
 */
export class SpeechProbe {
  private state = new Float32Array(STATE_SIZE);
  private context = new Float32Array(CONTEXT_SAMPLES);

  constructor(private readonly session: VadSession) {}

  /**
   * One frame's probability.
   *
   * The window is built as `[context, frame]` — previous first — because the context IS the
   * immediately preceding audio, and a model handed it in the other order is handed a recording with
   * 4 ms of the future spliced onto its front.
   */
  async probability(frame: Float32Array): Promise<number> {
    const window = new Float32Array(WINDOW_SAMPLES);
    window.set(this.context, 0);
    window.set(frame.subarray(0, WINDOW_SAMPLES - CONTEXT_SAMPLES), CONTEXT_SAMPLES);

    const { probability, state } = await this.session.infer(window, this.state);
    this.state = state;
    // Taken from the FRAME and not from the window, so a short final frame cannot fold the old
    // context forward as though it were new audio.
    this.context = frame.slice(Math.max(0, frame.length - CONTEXT_SAMPLES));
    if (this.context.length < CONTEXT_SAMPLES) {
      const padded = new Float32Array(CONTEXT_SAMPLES);
      padded.set(this.context, CONTEXT_SAMPLES - this.context.length);
      this.context = padded;
    }
    return Number.isFinite(probability) ? probability : 0;
  }

  /**
   * Forgets everything about the audio so far.
   *
   * Called when the microphone closes. Silero's state is a recurrent memory of what it just heard;
   * carrying it across a closed microphone would start the next conversation mid-thought about a
   * sentence from the last one.
   */
  reset(): void {
    this.state = new Float32Array(STATE_SIZE);
    this.context = new Float32Array(CONTEXT_SAMPLES);
  }
}

/** Where the model is served from. `shell/public/` is copied verbatim into the build. */
export const MODEL_URL = "/models/silero_vad.onnx";

/**
 * Builds a session against onnxruntime-web, or answers `null` if it cannot.
 *
 * `null` rather than a throw, and the caller falls back to `energyOf`. That is the same call
 * `voice.rs`'s `clean_up` makes about its own model — degraded output beats no output — and here it
 * covers a specific unknown: whether WebView2 loads this runtime's wasm under Tauri's custom
 * protocol. That has not been verified on a running app, so the failure has to be survivable.
 *
 * Single-threaded on purpose. The threaded build needs `SharedArrayBuffer`, which needs
 * cross-origin isolation headers that a custom protocol does not send — so asking for threads is
 * asking for the one configuration most likely not to exist.
 */
export async function loadSileroSession(): Promise<VadSession | null> {
  try {
    // `onnxruntime-web/wasm` and NOT the package root, and the difference is 13 MB of shipped app.
    // The root entry pulls the `jsep` build, which carries WebGPU and WebNN backends this never asks
    // for: measured on this repo's own `npm run build`, the root emits a 26.8 MB wasm plus 401 KB of
    // glue, and this entry emits 13.5 MB plus 72 KB. Nothing here wants a GPU — Silero on 576
    // samples is about a millisecond on one CPU core.
    const ort = await import("onnxruntime-web/wasm");
    ort.env.wasm.numThreads = 1;
    const session = await ort.InferenceSession.create(MODEL_URL);

    return {
      async infer(window: Float32Array, state: Float32Array) {
        const feeds = {
          input: new ort.Tensor("float32", window, [1, window.length]),
          state: new ort.Tensor("float32", state, [2, 1, 128]),
          // int64 as BigInt64Array: the model declares `sr` as `tensor(int64)`, and a Number here is
          // a type error at run time rather than a coercion.
          sr: new ort.Tensor("int64", BigInt64Array.from([16000n]), []),
        };
        const out = await session.run(feeds);
        return {
          probability: Number((out.output.data as Float32Array)[0]),
          state: out.stateN.data as Float32Array,
        };
      },
    };
  } catch {
    return null;
  }
}
