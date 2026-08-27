import { describe, expect, it } from "vitest";

import {
  DEFAULT_GATE,
  energyOf,
  FRAME_MS,
  GateSignal,
  GateState,
  IDLE_GATE,
  onFrame,
} from "./vad";

/** Feeds a run of probabilities and collects everything the gate decided. */
function feed(
  probabilities: number[],
  from: GateState = IDLE_GATE,
): { state: GateState; signals: GateSignal[] } {
  let state = from;
  const signals: GateSignal[] = [];
  for (const p of probabilities) {
    const next = onFrame(state, p, DEFAULT_GATE);
    state = next.state;
    if (next.signal) signals.push(next.signal);
  }
  return { state, signals };
}

const SPEECH = 0.9;
const SILENCE = 0.05;
/** Between `exit` and `enter`: neither clearly speech nor clearly silence. */
const AMBIGUOUS = 0.45;

const run = (value: number, frames: number) => Array<number>(frames).fill(value);

describe("the speech gate", () => {
  it("opens a turn once speech has lasted long enough, and closes it after the hangover", () => {
    const { signals, state } = feed([
      ...run(SPEECH, 10),
      ...run(SILENCE, DEFAULT_GATE.hangoverFrames),
    ]);

    expect(signals).toEqual(["speechStarted", "speechEnded"]);
    expect(state.speaking).toBe(false);
  });

  /**
   * The failure this prevents costs a transcription, a chat turn, and an answer to a question nobody
   * asked — every time somebody coughs or shuts a door.
   */
  it("ignores a click too short to be a word", () => {
    const { signals, state } = feed([
      ...run(SPEECH, DEFAULT_GATE.minSpeechFrames - 1),
      ...run(SILENCE, 40),
    ]);

    expect(signals).toEqual([]);
    expect(state.speaking).toBe(false);
  });

  /** Consecutive, not cumulative: two coughs a second apart are two coughs. */
  it("does not add two separate clicks together into a word", () => {
    const short = run(SPEECH, DEFAULT_GATE.minSpeechFrames - 1);
    const { signals } = feed([...short, SILENCE, ...short, SILENCE]);

    expect(signals).toEqual([]);
  });

  /**
   * The one that ruins a conversation if it is wrong: pauses inside a sentence must not end the turn.
   * "o que… está a correr?" ended early is half a question, answered.
   */
  it("keeps the turn open through a pause inside a sentence", () => {
    const pause = DEFAULT_GATE.hangoverFrames - 1;
    const { signals, state } = feed([
      ...run(SPEECH, 5),
      ...run(SILENCE, pause),
      ...run(SPEECH, 5),
      ...run(SILENCE, pause),
      ...run(SPEECH, 5),
    ]);

    expect(signals).toEqual(["speechStarted"]);
    expect(state.speaking).toBe(true);
  });

  /**
   * And the hangover must RESET on speech, not accumulate. Without the reset, a sentence with enough
   * pauses in it ends itself while somebody is still talking — the same bug, arriving later and
   * therefore harder to recognise.
   */
  it("resets the hangover when talking resumes, rather than accumulating towards an ending", () => {
    const almost = DEFAULT_GATE.hangoverFrames - 1;
    const many = [
      ...run(SPEECH, 5),
      ...Array.from({ length: 6 }, () => [...run(SILENCE, almost), SPEECH]).flat(),
    ];

    expect(feed(many).signals).toEqual(["speechStarted"]);
  });

  /**
   * Hysteresis. With one threshold, a probability sitting at it chatters: three starts and three
   * stops inside one word, each producing a recording of a syllable.
   */
  it("does not chatter on a probability sitting between the thresholds", () => {
    const { signals } = feed(run(AMBIGUOUS, 60));

    expect(signals).toEqual([]);
  });

  it("holds an open turn through ambiguous frames, because they are above the exit", () => {
    const { signals, state } = feed([...run(SPEECH, 5), ...run(AMBIGUOUS, 60)]);

    expect(signals).toEqual(["speechStarted"]);
    expect(state.speaking).toBe(true);
  });

  /**
   * An ONNX session that fails mid-stream yields NaN, and NaN compares false against both thresholds.
   * Un-guarded, that freezes the gate — and a frozen OPEN gate records until somebody notices.
   */
  it("treats a broken probability as silence rather than freezing", () => {
    const { signals, state } = feed([
      ...run(SPEECH, 5),
      ...run(Number.NaN, DEFAULT_GATE.hangoverFrames),
    ]);

    expect(signals).toEqual(["speechStarted", "speechEnded"]);
    expect(state.speaking).toBe(false);
  });

  it("opens a second turn after the first one closed", () => {
    const { signals } = feed([
      ...run(SPEECH, 5),
      ...run(SILENCE, DEFAULT_GATE.hangoverFrames),
      ...run(SPEECH, 5),
      ...run(SILENCE, DEFAULT_GATE.hangoverFrames),
    ]);

    expect(signals).toEqual([
      "speechStarted",
      "speechEnded",
      "speechStarted",
      "speechEnded",
    ]);
  });

  it("decides nothing at all from silence", () => {
    expect(feed(run(SILENCE, 200)).signals).toEqual([]);
  });

  /** The durations the constants above are reasoned about have to be the durations they produce. */
  it("counts frames that are worth what the comments say they are", () => {
    expect(FRAME_MS).toBeCloseTo(32, 5);
    expect(DEFAULT_GATE.hangoverFrames * FRAME_MS).toBeCloseTo(640, 5);
    expect(DEFAULT_GATE.minSpeechFrames * FRAME_MS).toBeLessThan(100);
    expect(DEFAULT_GATE.exit).toBeLessThan(DEFAULT_GATE.enter);
  });
});

describe("the placeholder energy source", () => {
  it("reports silence as silence and a loud frame as speech", () => {
    expect(energyOf(new Float32Array(512))).toBe(0);
    expect(energyOf(new Float32Array(512).fill(0.5))).toBeGreaterThan(
      DEFAULT_GATE.enter,
    );
  });

  it("never reports more than certainty, however loud the frame", () => {
    expect(energyOf(new Float32Array(512).fill(1))).toBe(1);
  });

  it("has nothing to say about an empty frame", () => {
    expect(energyOf(new Float32Array(0))).toBe(0);
  });

  /**
   * Named as the reason this is a placeholder: a fan and a voice are not distinguishable by level,
   * and a gate driven by this will open on the room.
   */
  it("cannot tell a steady hum from a voice, which is why Silero replaces it", () => {
    const hum = new Float32Array(512);
    for (let i = 0; i < hum.length; i += 1) hum[i] = 0.2 * Math.sin(i / 4);

    expect(energyOf(hum)).toBeGreaterThan(DEFAULT_GATE.enter);
  });
});
