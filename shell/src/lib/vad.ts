/**
 * Deciding when somebody started talking and when they stopped.
 *
 * Split into two halves that fail in completely different ways, because conflating them is why
 * hands-free modes feel broken:
 *
 * - **The gate** (`onFrame`) turns a stream of per-frame speech probabilities into two events. It is
 *   pure, it is where every real bug lives, and it is tested exhaustively below.
 * - **The source** of those probabilities. Today that is `energyOf`, which is a placeholder and says
 *   so. The intended source is Silero VAD through ONNX Runtime Web, which is what everything else in
 *   this file was shaped for: a probability per frame, in `[0, 1]`, at a fixed frame size.
 *
 * The split is what makes swapping them a one-line change rather than a rewrite — the same move
 * `core/src/transcribe.rs` made with its `Transcriber` trait, and for the same reason: the engine is
 * an implementation and the rules around it are the architecture.
 */

/**
 * Samples per frame at 16 kHz — 32 ms, and Silero's own frame size at this rate.
 *
 * Fixed rather than configurable because every duration below is counted in frames, and a frame that
 * meant a different amount of time in different places would make `HANGOVER_FRAMES` mean nothing.
 */
export const FRAME_SAMPLES = 512;

/** What one frame is worth in milliseconds, for turning the counts below into durations. */
export const FRAME_MS = (FRAME_SAMPLES / 16000) * 1000;

export interface GateConfig {
  /** Probability at or above which a frame counts as speech. */
  enter: number;
  /** Probability below which a frame counts as silence. Lower than `enter` — see `DEFAULT_GATE`. */
  exit: number;
  /** Frames of speech required before a turn is declared started. */
  minSpeechFrames: number;
  /** Frames of silence tolerated inside a turn before it is declared finished. */
  hangoverFrames: number;
}

/**
 * Three guards, each against a failure that a single threshold produces on its own.
 *
 * **Hysteresis (`enter` above `exit`).** With one threshold, a probability hovering at it chatters:
 * start, stop, start, three times inside one word, each producing a recording of a syllable. The gap
 * is what makes the decision stick once it is made.
 *
 * **`minSpeechFrames`.** A cough, a door, a key press crosses any threshold for a frame or two.
 * Without a minimum, each one opens a turn and sends the núcleo a recording of nothing — which costs
 * a transcription, a chat turn, and an answer to a question nobody asked. Three frames is under a
 * tenth of a second: shorter than the shortest word, longer than a click.
 *
 * **`hangoverFrames`.** This is the one that ruins a conversation if it is wrong. The pauses INSIDE a
 * sentence — "o que… está a correr?" — fall below any silence threshold, and a gate that ends the
 * turn there sends half a question and answers it. Twenty frames is roughly 640 ms: longer than a
 * pause for breath, short enough that the end of a turn does not feel like waiting.
 */
export const DEFAULT_GATE: GateConfig = {
  enter: 0.6,
  exit: 0.35,
  minSpeechFrames: 3,
  hangoverFrames: 20,
};

/**
 * Frames of audio kept from BEFORE the gate opened, and prepended to the recording.
 *
 * Not a refinement — without it the start of every question is missing. The gate cannot declare
 * speech until `minSpeechFrames` of it have gone by, so by the time it fires, that much of the first
 * word is already in the past. In Portuguese that is most of a syllable, and "está" arriving as "tá"
 * is the kind of error that survives into the transcript looking like the transcriber's fault.
 *
 * Eight frames — a little over a quarter of a second — is comfortably more than `minSpeechFrames` and
 * short enough that the leading silence costs nothing: whisper is untroubled by it, and the deadline
 * scales with duration anyway.
 */
export const PREROLL_FRAMES = 8;

export interface GateState {
  /** Whether a turn is currently open. */
  speaking: boolean;
  /** Consecutive speech frames seen while closed — counts towards `minSpeechFrames`. */
  above: number;
  /** Consecutive silent frames seen while open — counts towards `hangoverFrames`. */
  below: number;
}

export const IDLE_GATE: GateState = { speaking: false, above: 0, below: 0 };

export type GateSignal = "speechStarted" | "speechEnded" | null;

/**
 * One frame of audio, as a decision.
 *
 * Returns the next state and whatever the frame decided, which is usually nothing. Pure, so the whole
 * of the behaviour above can be tested with an array of numbers rather than a microphone.
 *
 * A probability that is not a number is treated as silence rather than ignored. An ONNX session that
 * fails mid-stream yields `NaN`, and `NaN >= enter` is false while `NaN < exit` is also false — so an
 * un-guarded version would freeze the gate in whatever state it was in, which for an open turn means
 * recording until the mode is switched off by hand.
 */
export function onFrame(
  state: GateState,
  probability: number,
  config: GateConfig = DEFAULT_GATE,
): { state: GateState; signal: GateSignal } {
  const p = Number.isFinite(probability) ? probability : 0;

  if (!state.speaking) {
    if (p >= config.enter) {
      const above = state.above + 1;
      if (above >= config.minSpeechFrames) {
        return {
          state: { speaking: true, above: 0, below: 0 },
          signal: "speechStarted",
        };
      }
      return { state: { ...state, above }, signal: null };
    }
    // Reset rather than decay: `minSpeechFrames` counts CONSECUTIVE frames, and a run broken by
    // silence is two coughs rather than the start of a word.
    return { state: { ...state, above: 0 }, signal: null };
  }

  if (p < config.exit) {
    const below = state.below + 1;
    if (below >= config.hangoverFrames) {
      return {
        state: { speaking: false, above: 0, below: 0 },
        signal: "speechEnded",
      };
    }
    return { state: { ...state, below }, signal: null };
  }
  // Speech again before the hangover ran out: the pause was inside the sentence, so the count starts
  // over. Without this reset, a long enough sentence with enough pauses in it would accumulate its
  // way to an ending while somebody was still talking.
  return { state: { ...state, below: 0 }, signal: null };
}

/**
 * Root-mean-square level of a frame, mapped into `[0, 1]`. **A placeholder, not a voice detector.**
 *
 * It answers "is this loud" and the gate wants "is this speech", and the difference is exactly the
 * failure Silero exists to fix: a laptop fan, a fridge, music, and the person typing all clear an
 * energy threshold, and speech at a conversational distance sometimes does not. On a quiet desk with
 * a close microphone this is usable enough to run the loop end to end, which is what it is for.
 *
 * Kept deliberately small and behind the same shape Silero has — a frame in, a probability out — so
 * replacing it touches this function and nothing else.
 */
export function energyOf(frame: Float32Array): number {
  if (frame.length === 0) return 0;
  let sum = 0;
  for (let i = 0; i < frame.length; i += 1) sum += frame[i] * frame[i];
  const rms = Math.sqrt(sum / frame.length);
  // -50 dBFS to -20 dBFS spread across the range: below is a quiet room, above is somebody talking
  // into a desk microphone. Clamped at both ends so a loud frame cannot report more than certainty.
  const db = 20 * Math.log10(Math.max(rms, 1e-8));
  return Math.min(1, Math.max(0, (db + 50) / 30));
}
