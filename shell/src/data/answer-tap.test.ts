import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { ANSWER_BANDS, createAnswerTap } from "./answer-tap";

interface FakeAnalyser {
  fftSize: number;
  readonly frequencyBinCount: number;
  getFloatTimeDomainData(buffer: Float32Array): void;
  getByteFrequencyData(buffer: Uint8Array): void;
  connect: ReturnType<typeof vi.fn>;
}

let analyser: FakeAnalyser;
let source: { connect: ReturnType<typeof vi.fn>; disconnect: ReturnType<typeof vi.fn> };
let original: PropertyDescriptor | undefined;

function answerElement(): HTMLMediaElement {
  return {
    captureStream: () => ({ getAudioTracks: () => [{}], addEventListener: vi.fn() }),
  } as unknown as HTMLMediaElement;
}

beforeEach(() => {
  source = { connect: vi.fn(), disconnect: vi.fn() };
  analyser = {
    fftSize: 2048,
    get frequencyBinCount() {
      return this.fftSize / 2;
    },
    getFloatTimeDomainData(buffer: Float32Array) {
      buffer.fill(0.25);
    },
    getByteFrequencyData(buffer: Uint8Array) {
      buffer.fill(128);
    },
    connect: vi.fn(),
  };
  original = Object.getOwnPropertyDescriptor(window, "AudioContext");
  Object.defineProperty(window, "AudioContext", {
    configurable: true,
    value: class {
      sampleRate = 48_000;
      destination = {};
      state = "running";
      resume = vi.fn().mockResolvedValue(undefined);
      close = vi.fn().mockResolvedValue(undefined);
      createMediaStreamSource = vi.fn(() => source);
      createAnalyser = vi.fn(() => analyser);
    },
  });
});

afterEach(() => {
  if (original === undefined) {
    Reflect.deleteProperty(window, "AudioContext");
  } else {
    Object.defineProperty(window, "AudioContext", original);
  }
});

describe("the answer tap", () => {
  it("without captureStream the tap reads silence", () => {
    const { tap, attach } = createAnswerTap();
    attach({} as unknown as HTMLMediaElement);

    const out = new Float32Array(ANSWER_BANDS).fill(1);
    expect(tap.level()).toBe(0);
    expect(tap.bands(out)).toBe(false);
    expect(Array.from(out).every((band) => band === 0)).toBe(true);
  });

  it("an attached answer is analysed without being routed to the speakers", () => {
    const { attach } = createAnswerTap();
    attach(answerElement());

    expect(source.connect).toHaveBeenCalledTimes(1);
    expect(source.connect).toHaveBeenCalledWith(analyser);
    expect(analyser.connect).not.toHaveBeenCalled();
  });

  it("the level follows the answer and the bands fill low to high", () => {
    const { tap, attach } = createAnswerTap();
    attach(answerElement());

    expect(tap.level()).toBeGreaterThan(0.9);

    const out = new Float32Array(ANSWER_BANDS);
    expect(tap.bands(out)).toBe(true);
    expect(out).toHaveLength(24);
    for (const band of out) expect(band).toBeCloseTo(0.5, 1);
  });

  it("detaching returns the tap to silence", () => {
    const { tap, attach, detach } = createAnswerTap();
    attach(answerElement());
    expect(tap.level()).toBeGreaterThan(0);

    detach();

    const out = new Float32Array(ANSWER_BANDS).fill(1);
    expect(source.disconnect).toHaveBeenCalled();
    expect(tap.level()).toBe(0);
    expect(tap.bands(out)).toBe(false);
    expect(Array.from(out).every((band) => band === 0)).toBe(true);
  });
});
