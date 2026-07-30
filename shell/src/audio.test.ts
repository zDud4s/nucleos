import { describe, expect, it } from "vitest";

import {
  downmixToMono,
  durationMs,
  encodeCapture,
  resampleLinear,
  TARGET_SAMPLE_RATE,
  toPcm16,
  wavBytes,
} from "./audio";

describe("downmixToMono", () => {
  it("averages the channels instead of dropping one", () => {
    // Both channels carry signal; taking channel 0 would report 1.0 and lose the other side.
    expect(Array.from(downmixToMono(new Float32Array([1, 0, 0.5, 0.5]), 2))).toEqual([0.5, 0.5]);
  });

  it("passes mono through untouched", () => {
    const mono = new Float32Array([0.25, -0.5, 0.75]);
    expect(downmixToMono(mono, 1)).toBe(mono);
  });

  it("drops a partial trailing frame rather than averaging it against nothing", () => {
    // Five samples of stereo is two frames and a stray. Keeping the stray emits a sample averaged
    // against absent channels — a click at the end of every recording.
    expect(Array.from(downmixToMono(new Float32Array([1, 1, 0.5, 0.5, 0.9]), 2))).toEqual([1, 0.5]);
  });

  it("yields no audio for a device reporting no channels rather than dividing by zero", () => {
    expect(downmixToMono(new Float32Array([1, 2]), 0).length).toBe(0);
  });
});

describe("resampleLinear", () => {
  it("changes nothing when the rate already matches", () => {
    const input = new Float32Array([0, 0.25, -0.5]);
    expect(resampleLinear(input, 16000, 16000)).toBe(input);
  });

  it("interpolates between neighbours when upsampling", () => {
    // 2 samples at 8k -> 4 at 16k, read at input positions 0, 0.5, 1.0, 1.5. The last two clamp to
    // the final sample: there is nothing beyond it to interpolate towards.
    expect(Array.from(resampleLinear(new Float32Array([0, 1]), 8000, 16000))).toEqual([0, 0.5, 1, 1]);
  });

  it("picks every third sample coming down from a real device rate", () => {
    // 48 kHz is what a laptop hands out; 16 kHz is what the transcriber wants.
    const input = new Float32Array([0, 0.125, 0.25, 0.375, 0.5, 0.625]);
    expect(Array.from(resampleLinear(input, 48000, 16000))).toEqual([0, 0.375]);
  });

  it("yields no audio for a device reporting no rate rather than a pitch shift", () => {
    expect(resampleLinear(new Float32Array([0, 1]), 0, 16000).length).toBe(0);
  });
});

describe("toPcm16", () => {
  it("clips a sample over full scale instead of wrapping it", () => {
    // Wrapping turns a shout into full-scale noise of the opposite sign, which a transcriber hears
    // as a completely different sound rather than as loudness.
    expect(Array.from(toPcm16(new Float32Array([2, -2])))).toEqual([32767, -32767]);
  });

  it("turns a NaN sample into silence", () => {
    expect(Array.from(toPcm16(new Float32Array([Number.NaN])))).toEqual([0]);
  });
});

describe("wavBytes", () => {
  it("says mono sixteen-bit at the transcriber's rate", () => {
    const wav = wavBytes(new Int16Array([0, -1]), TARGET_SAMPLE_RATE);
    const view = new DataView(wav.buffer);
    expect(String.fromCharCode(...wav.slice(0, 4))).toBe("RIFF");
    expect(String.fromCharCode(...wav.slice(8, 12))).toBe("WAVE");
    expect(String.fromCharCode(...wav.slice(12, 16))).toBe("fmt ");
    expect(view.getUint16(20, true)).toBe(1); // uncompressed PCM
    expect(view.getUint16(22, true)).toBe(1); // mono
    expect(view.getUint32(24, true)).toBe(TARGET_SAMPLE_RATE);
    expect(view.getUint16(34, true)).toBe(16); // bits per sample
  });

  it("keeps its declared lengths in step with the bytes that follow them", () => {
    // A header claiming more data than it carries is what makes a reader run past the end, and it is
    // invisible without checking both fields against the real length.
    const wav = wavBytes(new Int16Array([1, 2, 3]), TARGET_SAMPLE_RATE);
    const view = new DataView(wav.buffer);
    expect(wav.length).toBe(44 + 6);
    expect(view.getUint32(4, true)).toBe(wav.length - 8);
    expect(view.getUint32(40, true)).toBe(6);
    expect(view.getInt16(44, true)).toBe(1);
  });

  it("still produces a valid header for an empty capture", () => {
    const wav = wavBytes(new Int16Array(0), TARGET_SAMPLE_RATE);
    expect(wav.length).toBe(44);
    expect(new DataView(wav.buffer).getUint32(40, true)).toBe(0);
  });
});

describe("encodeCapture", () => {
  it("lands one second of 48 kHz stereo at 16 kHz mono", () => {
    const interleaved = new Float32Array(48000 * 2).fill(0.25);
    const wav = encodeCapture(interleaved, 2, 48000);
    expect(wav.length).toBe(44 + 16000 * 2);
    expect(new DataView(wav.buffer).getUint32(24, true)).toBe(TARGET_SAMPLE_RATE);
  });
});

describe("durationMs", () => {
  it("comes from the frames, not from a clock", () => {
    expect(durationMs(48000, 48000)).toBe(1000);
    expect(durationMs(24000, 48000)).toBe(500);
    expect(durationMs(0, 48000)).toBe(0);
    // Twenty minutes at 48 kHz, the longest capture the daemon accepts.
    expect(durationMs(57600000, 48000)).toBe(1200000);
  });

  it("reports no duration for a device with no rate rather than dividing by zero", () => {
    expect(durationMs(1000, 0)).toBe(0);
  });
});
