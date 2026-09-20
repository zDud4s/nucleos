/**
 * Which part of a finished recording is somebody talking — and whether any of it is.
 *
 * **Why this is not the gate in `vad.ts`.** That one decides mid-recording, frame by frame, when a
 * turn starts and stops; a hands-free conversation cannot wait for the end because the end is what it
 * is trying to detect. This one is handed a recording that has already stopped, and answers a
 * different question: where the speech is inside it, and whether to send it at all. It uses `onFrame`
 * and `DEFAULT_GATE` rather than thresholds of its own, because two opinions about what counts as
 * speech in one application is how a recording that opens a turn becomes one that is discarded.
 *
 * **The measurement it exists for.** On 2026-09-18 the dictation on the front door recorded 40 s with
 * one sentence in it and sent all 40 s to whisper. Whisper does not answer "nothing" to audio without
 * speech in it — it invents:
 *
 * | what was said | duration | what came back |
 * |---|---|---|
 * | nothing | 40.7 s | `Allah, iosha' mumu.` |
 * | nothing | 40.5 s | `[IMHA METALL [ ice / ice / ice / ice …` |
 *
 * Whisper's own switches do not fix it, and one of them makes it worse. Measured on 40 s of a quiet
 * room with `ggml-base` on 2026-09-19: plain gives `[música] [música]`, `--suppress-nst` gives
 * `O que você está fazendo? O que você está fazendo?` — a discardable marker turned into a plausible
 * sentence, which is the dangerous direction, because that one gets pasted into whatever box was
 * waiting for it. `--no-speech-thold 0.9` changed nothing. Whisper's `--vad` does suppress it, at
 * double the time (12.8 s → 32.5 s on the same file) and with worse text on the speech that IS there.
 *
 * So the audio is cut before it is sent, here, where it costs a pass over an array that is already in
 * memory — and the recording that turns out to hold no speech is never sent at all.
 */

import type { SpeechProbe } from "./silero";
import { DEFAULT_GATE, FRAME_SAMPLES, IDLE_GATE, onFrame, PREROLL_FRAMES, type GateConfig } from "./vad";

/** A half-open range of frames: `fromFrame` is included, `toFrame` is not. */
export interface SpeechSpan {
  fromFrame: number;
  toFrame: number;
}

/**
 * Where the speech is, in frames, or `null` if the gate never opened.
 *
 * One span from the first word to the last rather than one per sentence, and that is deliberate: a
 * dictation is one thing somebody said, pauses included, and handing whisper the pieces separately
 * takes away the context it decodes with — which is what `--vad` did in the measurement above.
 */
export function speechSpan(
  probabilities: ArrayLike<number>,
  config: GateConfig = DEFAULT_GATE,
  margin = PREROLL_FRAMES,
): SpeechSpan | null {
  let state = IDLE_GATE;
  let first: number | null = null;
  let last: number | null = null;

  for (let frame = 0; frame < probabilities.length; frame += 1) {
    const probability = probabilities[frame];
    const step = onFrame(state, probability, config);
    state = step.state;
    if (step.signal === "speechStarted") {
      // `onFrame` fires on the LAST frame of the run it needed, so speech began `minSpeechFrames - 1`
      // frames ago. Trimming from the signal instead would cut the first syllable off every
      // dictation — in Portuguese, "está" arriving as "tá", which reads as the transcriber's fault.
      const began = frame - (config.minSpeechFrames - 1);
      first ??= began;
      last = frame;
    } else if (state.speaking && probability >= config.exit) {
      last = frame;
    }
  }

  if (first === null || last === null) return null;
  return {
    fromFrame: Math.max(0, first - margin),
    toFrame: Math.min(probabilities.length, last + 1 + margin),
  };
}

/**
 * The recording, cut down to the speech in it — or `null` when there is none.
 *
 * `null` and not an empty recording, because the two mean different things to every caller: nothing
 * to send is an ordinary outcome of holding a microphone open and saying nothing, and a zero-length
 * WAV is a recording that failed.
 */
export function trimToSpeech(
  samples: Float32Array,
  probabilities: ArrayLike<number>,
  config: GateConfig = DEFAULT_GATE,
): Float32Array | null {
  const span = speechSpan(probabilities, config);
  if (span === null) return null;
  // `toFrame` is a frame count and the recording is samples: the tail that does not fill a frame has
  // no probability of its own, and clamping to the length keeps it rather than rounding it away.
  return samples.slice(
    span.fromFrame * FRAME_SAMPLES,
    Math.min(samples.length, span.toFrame * FRAME_SAMPLES),
  );
}

/**
 * The whole job for a caller holding 16 kHz mono samples and a probe: ask Silero, then cut.
 *
 * The probe is reset first. Silero threads a recurrent state and 64 samples of context between
 * frames, so a probe reused across recordings would judge this one while still remembering the last —
 * and the symptom of that is a dictation discarded because of what the previous one sounded like.
 */
export async function speechOnly(
  samples: Float32Array,
  probe: SpeechProbe,
  config: GateConfig = DEFAULT_GATE,
): Promise<Float32Array | null> {
  probe.reset();
  const probabilities: number[] = [];
  for (let at = 0; at < samples.length; at += FRAME_SAMPLES) {
    probabilities.push(await probe.probability(samples.subarray(at, at + FRAME_SAMPLES)));
  }
  return trimToSpeech(samples, probabilities, config);
}
