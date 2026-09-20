import { describe, expect, it } from "vitest";

import { speechOnly, speechSpan, trimToSpeech } from "./speech";
import type { VadSession } from "./silero";
import { SpeechProbe } from "./silero";
import { DEFAULT_GATE, FRAME_SAMPLES, PREROLL_FRAMES } from "./vad";

/** `frames` probabilities, with `speech` set to 1 over the half-open range given. */
function probabilities(frames: number, speech: [number, number] | null): number[] {
  const out = Array(frames).fill(0);
  if (speech !== null) for (let i = speech[0]; i < speech[1]; i += 1) out[i] = 1;
  return out;
}

/** Samples whose value is the frame they belong to, so a slice says where it came from. */
function numberedFrames(frames: number): Float32Array {
  const out = new Float32Array(frames * FRAME_SAMPLES);
  for (let frame = 0; frame < frames; frame += 1) {
    out.fill(frame, frame * FRAME_SAMPLES, (frame + 1) * FRAME_SAMPLES);
  }
  return out;
}

describe("finding the speech in a recording", () => {
  /**
   * The measured failure this file exists for. On 2026-09-18 the dictation sent 40 s of a quiet room
   * to whisper, which answered `[IMHA METALL [ ice / ice / ice …` — and `[música]`, and `Allah,
   * iosha' mumu`, in the other captures of the same evening. Whisper hallucinates on audio with no
   * speech in it; its own `--suppress-nst` measured WORSE, turning a discardable `[música]` into a
   * plausible sentence. So the recording must not be sent at all.
   */
  it("finds nothing in a recording with no speech in it", () => {
    expect(speechSpan(probabilities(1250, null))).toBeNull();
  });

  /** A cough, a door, a key press. Under `minSpeechFrames` is not a word, and the gate agrees. */
  it("finds nothing in a blip shorter than the gate's minimum", () => {
    const blip = probabilities(100, [50, 50 + DEFAULT_GATE.minSpeechFrames - 1]);

    expect(speechSpan(blip)).toBeNull();
  });

  /**
   * The gate cannot declare speech until `minSpeechFrames` of it have gone by, so the frame it fires
   * on is not the frame speech began on. Trimming from the signal rather than from the start of the
   * run would cut the first syllable off every dictation — which is the error that reads as the
   * transcriber's fault, the reason `PREROLL_FRAMES` exists for the conversation, and the reason the
   * margin here is the same constant rather than a second opinion about the same audio.
   */
  it("keeps the audio from before the gate could know, not from where it fired", () => {
    const span = speechSpan(probabilities(200, [80, 120]));

    expect(span).toEqual({ fromFrame: 80 - PREROLL_FRAMES, toFrame: 120 + PREROLL_FRAMES });
  });

  /** The same margin at the end, so a final consonant is not the thing that gets cut. */
  it("keeps a margin at both ends", () => {
    const span = speechSpan(probabilities(200, [80, 120]));

    expect(span!.toFrame - span!.fromFrame).toBe(40 + 2 * PREROLL_FRAMES);
  });

  /** Speech from the first frame: the margin cannot reach back past the start of the recording. */
  it("stops at the start of the recording rather than before it", () => {
    expect(speechSpan(probabilities(100, [0, 40]))?.fromFrame).toBe(0);
  });

  /** And not past the end of it, or the slice would be shorter than the span claims. */
  it("stops at the end of the recording rather than after it", () => {
    expect(speechSpan(probabilities(100, [60, 100]))?.toFrame).toBe(100);
  });

  /** Two sentences with a pause between them are one span, not two recordings. */
  it("covers everything between the first word and the last", () => {
    const two = probabilities(300, [50, 80]);
    for (let i = 200; i < 240; i += 1) two[i] = 1;

    expect(speechSpan(two)).toEqual({
      fromFrame: 50 - PREROLL_FRAMES,
      toFrame: 240 + PREROLL_FRAMES,
    });
  });

  /**
   * An ONNX session that fails mid-stream yields `NaN`, and `onFrame` already treats that as silence
   * rather than freezing. Asserted here too because the consequence is different: there it holds a
   * turn open, here it would decide what audio exists.
   */
  it("treats a broken probability as silence rather than as speech", () => {
    expect(speechSpan(Array(100).fill(Number.NaN))).toBeNull();
  });

  describe("cutting the recording down to it", () => {
    it("returns nothing at all when there is no speech, rather than an empty recording", () => {
      expect(trimToSpeech(numberedFrames(1250), probabilities(1250, null))).toBeNull();
    });

    /** The whole point, in the shape the measurement had: 40 s in, about 2 s out. */
    it("cuts 40 s of a quiet room down to the sentence in the middle of it", () => {
      const frames = 1250; // 40 s at 32 ms a frame
      const spoken: [number, number] = [600, 662]; // just under 2 s of speech
      const trimmed = trimToSpeech(numberedFrames(frames), probabilities(frames, spoken));

      expect(trimmed).not.toBeNull();
      expect(trimmed!.length).toBe((62 + 2 * PREROLL_FRAMES) * FRAME_SAMPLES);
      expect(trimmed![0]).toBe(600 - PREROLL_FRAMES);
    });

    /** A recording is samples, not frames: the tail that does not fill a frame must survive. */
    it("keeps the samples of a final part-frame", () => {
      const samples = new Float32Array(10 * FRAME_SAMPLES + 17).fill(1);
      const trimmed = trimToSpeech(samples, probabilities(11, [0, 11]));

      expect(trimmed!.length).toBe(samples.length);
    });
  });

  describe("asking Silero for the probabilities", () => {
    /** A session that calls anything above the gate's `enter` speech, so the frames decide. */
    const sessionOver = (isSpeech: (call: number) => boolean): VadSession => {
      let call = -1;
      return {
        async infer(_window, state) {
          call += 1;
          return { probability: isSpeech(call) ? 1 : 0, state };
        },
      };
    };

    it("answers with the speech it found", async () => {
      const probe = new SpeechProbe(sessionOver((call) => call >= 40 && call < 80));

      const trimmed = await speechOnly(numberedFrames(200), probe);

      expect(trimmed!.length).toBe((40 + 2 * PREROLL_FRAMES) * FRAME_SAMPLES);
    });

    it("answers nothing for a recording Silero heard no speech in", async () => {
      const probe = new SpeechProbe(sessionOver(() => false));

      expect(await speechOnly(numberedFrames(200), probe)).toBeNull();
    });

    /**
     * The probe is stateful — Silero threads a recurrent state and 64 samples of context between
     * frames — so a probe reused across two recordings would judge the second one while still
     * remembering the first. Reset here rather than at the call sites, where forgetting it is
     * invisible until a dictation is discarded for what the previous one sounded like.
     */
    it("forgets the previous recording before judging this one", async () => {
      const states: number[] = [];
      let call = -1;
      const probe = new SpeechProbe({
        async infer(_window, state) {
          call += 1;
          states.push(state[0]);
          const next = new Float32Array(state.length);
          next.fill(call + 1);
          return { probability: 0, state: next };
        },
      });

      await speechOnly(numberedFrames(3), probe);
      states.length = 0;
      await speechOnly(numberedFrames(3), probe);

      expect(states[0]).toBe(0);
    });
  });
});
