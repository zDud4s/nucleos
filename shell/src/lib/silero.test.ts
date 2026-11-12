import { describe, expect, it } from "vitest";

import {
  CONTEXT_SAMPLES,
  loadSileroSession,
  SpeechProbe,
  STATE_SIZE,
  VadSession,
  WINDOW_SAMPLES,
} from "./silero";
import { FRAME_SAMPLES } from "./vad";

/** Records every window and state it is handed, and threads a state back so carrying is observable. */
function recording(probability = 0.9) {
  const windows: Float32Array[] = [];
  const states: Float32Array[] = [];
  let generation = 0;
  const session: VadSession = {
    async infer(window, state) {
      windows.push(window.slice());
      states.push(state.slice());
      generation += 1;
      const next = new Float32Array(STATE_SIZE);
      next.fill(generation);
      return { probability, state: next };
    },
  };
  return { session, windows, states };
}

const ramp = (from: number, length = FRAME_SAMPLES) =>
  Float32Array.from({ length }, (_, i) => from + i);

describe("the Silero probe", () => {
  /**
   * The trap this file exists around: a bare 512-sample window is ACCEPTED by the model and returns
   * a plausible near-zero on real speech — no error, no warning, a VAD that simply never fires.
   */
  it("always feeds the model a full window, never the bare frame", async () => {
    const { session, windows } = recording();
    const probe = new SpeechProbe(session);

    await probe.probability(ramp(0));

    expect(windows[0].length).toBe(WINDOW_SAMPLES);
    expect(WINDOW_SAMPLES).toBe(FRAME_SAMPLES + CONTEXT_SAMPLES);
  });

  /** The very first window has no previous audio, so its context is silence rather than nothing. */
  it("starts with silent context instead of a short window", async () => {
    const { session, windows } = recording();

    await new SpeechProbe(session).probability(ramp(1000));

    expect(Array.from(windows[0].subarray(0, CONTEXT_SAMPLES))).toEqual(
      Array(CONTEXT_SAMPLES).fill(0),
    );
    expect(windows[0][CONTEXT_SAMPLES]).toBe(1000);
  });

  /**
   * Context first, frame second. Reversed, the model is handed a recording with four milliseconds of
   * the future spliced onto its front — which is not an error anywhere, just a worse answer.
   */
  it("puts the previous frame's tail in front of the current frame", async () => {
    const { session, windows } = recording();
    const probe = new SpeechProbe(session);

    const first = ramp(0);
    await probe.probability(first);
    const second = ramp(10_000);
    await probe.probability(second);

    const context = Array.from(windows[1].subarray(0, CONTEXT_SAMPLES));
    expect(context).toEqual(
      Array.from(first.subarray(FRAME_SAMPLES - CONTEXT_SAMPLES)),
    );
    // And the frame itself follows it, unshifted.
    expect(windows[1][CONTEXT_SAMPLES]).toBe(10_000);
  });

  /** The recurrent state is the model's memory of what it just heard; it has to come back in. */
  it("threads the state from one frame into the next", async () => {
    const { session, states } = recording();
    const probe = new SpeechProbe(session);

    await probe.probability(ramp(0));
    await probe.probability(ramp(1));
    await probe.probability(ramp(2));

    expect(Array.from(states[0].slice(0, 3))).toEqual([0, 0, 0]);
    expect(Array.from(states[1].slice(0, 3))).toEqual([1, 1, 1]);
    expect(Array.from(states[2].slice(0, 3))).toEqual([2, 2, 2]);
  });

  /**
   * A state carried across a closed microphone describes audio from another conversation — and
   * Silero's state is a memory, so it would start the next one mid-thought about the last.
   */
  it("forgets the conversation when reset", async () => {
    const { session, states, windows } = recording();
    const probe = new SpeechProbe(session);

    await probe.probability(ramp(0));
    await probe.probability(ramp(1));
    probe.reset();
    await probe.probability(ramp(2));

    expect(Array.from(states[2])).toEqual(Array(STATE_SIZE).fill(0));
    expect(Array.from(windows[2].subarray(0, CONTEXT_SAMPLES))).toEqual(
      Array(CONTEXT_SAMPLES).fill(0),
    );
  });

  /**
   * The same guard `vad.ts` applies for the same reason: a runtime that fails mid-stream yields NaN,
   * and NaN compares false against both thresholds — freezing the gate in whatever state it was in.
   * Frozen OPEN means recording until somebody notices.
   */
  it("reports a broken probability as silence rather than passing NaN on", async () => {
    const session: VadSession = {
      async infer() {
        return { probability: Number.NaN, state: new Float32Array(STATE_SIZE) };
      },
    };

    expect(await new SpeechProbe(session).probability(ramp(0))).toBe(0);
  });

  /** A short final frame must still produce a full window and a full context. */
  it("pads rather than shrinks when the last frame is short", async () => {
    const { session, windows } = recording();
    const probe = new SpeechProbe(session);

    await probe.probability(ramp(0, 100));
    await probe.probability(ramp(5000));

    expect(windows[0].length).toBe(WINDOW_SAMPLES);
    expect(windows[1].subarray(0, CONTEXT_SAMPLES).length).toBe(CONTEXT_SAMPLES);
    // The 100 real samples land at the END of the context, so the newest audio stays adjacent to the
    // frame that follows it. Padding at the front is silence; padding at the back would be a gap.
    expect(windows[1][CONTEXT_SAMPLES - 1]).toBe(99);
  });
});

/**
 * What happens when the runtime does not load, which on this machine was every time.
 *
 * The session of 2026-09-18 shipped a VAD that never ran: the app said "listening by loudness" and
 * the reason was a `catch {}` with nothing in it. The policy refused to compile WebAssembly — a
 * legible error, thrown, caught, and dropped — and the next session spent its first hour finding out
 * what was already known inside this function.
 */
describe("loading the runtime", () => {
  /** The real module's type, so the fakes below are the only place that casts. */
  type OrtWasm = typeof import("onnxruntime-web/wasm");

  it("says why it could not load instead of only answering null", async () => {
    const load = await loadSileroSession(async () => {
      throw new Error("Compiling WebAssembly violates the Content Security Policy");
    });

    expect(load.session).toBeNull();
    expect(load.why).toContain("Content Security Policy");
  });

  /**
   * The second failure mode, and the one a CSP fix does not cover: the runtime loads and the MODEL
   * does not. In `tauri dev` Vite serves `index.html` for the wasm's own URL, so the failure arrives
   * from `create` rather than from the import — a different line, the same silence before this.
   */
  it("says why when the runtime loads and the model does not", async () => {
    const ort = {
      env: { wasm: { numThreads: 0 } },
      InferenceSession: {
        async create() {
          throw new Error("no available backend found");
        },
      },
    } as unknown as OrtWasm;

    const load = await loadSileroSession(async () => ort);

    expect(load.session).toBeNull();
    expect(load.why).toContain("no available backend found");
  });

  /** A runtime that works answers a session and no reason, so a caller cannot report both. */
  it("answers a session and nothing to explain when it works", async () => {
    const ort = {
      env: { wasm: { numThreads: 0 } },
      InferenceSession: {
        async create() {
          return {
            async run() {
              return {
                output: { data: Float32Array.from([0.42]) },
                stateN: { data: new Float32Array(STATE_SIZE) },
              };
            },
          };
        },
      },
      Tensor: class {
        constructor(
          readonly type: string,
          readonly data: unknown,
          readonly dims: unknown,
        ) {}
      },
    } as unknown as OrtWasm;

    const load = await loadSileroSession(async () => ort);

    expect(load.why).toBeNull();
    const probability = await load.session?.infer(
      new Float32Array(WINDOW_SAMPLES),
      new Float32Array(STATE_SIZE),
    );
    // `toBeCloseTo` and not `toBe`: the data comes back as float32, where 0.42 is 0.41999998688697815.
    expect(probability?.probability).toBeCloseTo(0.42);
  });
});
