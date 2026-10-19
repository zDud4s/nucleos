/**
 * The contract the ears offer their two callers, tested at the contract.
 *
 * `data/dictation.ts` mocks this module wholesale and `data/conversation.ts` drives it through a faked
 * audio graph, so between them the two suites prove what each CALLER decides — and neither proves what
 * this module promises. Three of those promises are the kind that fail in silence:
 *
 * - a caller that chooses the gate per frame, which is how the bar rises while an answer plays;
 * - a caller that REFUSES a signal, which has to re-arm the gate and not merely drop the signal;
 * - the measurements behind the decision, which a caller cannot recompute because the probability
 *   came from a model it does not hold.
 */

import { act, renderHook, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { DEFAULT_GATE, energyOf, FRAME_MS, FRAME_SAMPLES, SPEAKING_GATE } from "../lib/vad";

const silero = vi.hoisted(() => ({ loadSileroSession: vi.fn(), probability: vi.fn() }));
vi.mock("../lib/silero", () => ({
  loadSileroSession: silero.loadSileroSession,
  SpeechProbe: class {
    probability = silero.probability;
    reset = vi.fn();
  },
}));

import { useListening, type ListeningHandlers } from "./listening";

interface FakeProcessor {
  onaudioprocess: ((event: AudioProcessingEvent) => void) | null;
  connect: ReturnType<typeof vi.fn>;
  disconnect: ReturnType<typeof vi.fn>;
}

let processor: FakeProcessor;

/**
 * The amplitude whose loudness is `probability`, because the energy fallback reports mapped dBFS and
 * not raw RMS — filling a frame with 0.2 reports certainty, not a fifth of it. Asserted below, so a
 * change to `energyOf` breaks this arithmetic rather than quietly moving every gate in this file.
 */
function amplitudeFor(probability: number): number {
  return 10 ** ((probability * 30 - 50) / 20);
}

function sendFrames(count: number, amplitude: number): void {
  const callback = processor.onaudioprocess;
  if (callback === null) throw new Error("the microphone has no audio callback");
  const frames = new Float32Array(FRAME_SAMPLES * count).fill(amplitude);
  callback({ inputBuffer: { getChannelData: () => frames } } as unknown as AudioProcessingEvent);
}

/** An open microphone whose signals are collected, plus whatever the test overrides. */
async function anOpenMicrophone(handlers: Partial<ListeningHandlers> = {}) {
  const signals: Array<"speechStarted" | "speechEnded"> = [];
  const listening = {
    onSignal: (signal: "speechStarted" | "speechEnded") => {
      signals.push(signal);
    },
    onTrouble: () => {},
    ...handlers,
  } satisfies ListeningHandlers;

  const { result } = renderHook(() => useListening(listening));
  await act(async () => {
    await result.current.open();
  });
  return { result, signals };
}

beforeEach(() => {
  silero.loadSileroSession.mockReset();
  silero.probability.mockReset();
  // The energy fallback by default: it is synchronous, so a test about counting consecutive frames
  // counts them all. The probe's chain drops a frame once four are waiting, which is correct of it
  // and would make a gate test lie.
  silero.loadSileroSession.mockResolvedValue({ session: null, why: "no runtime in this suite" });

  processor = { onaudioprocess: null, connect: vi.fn(), disconnect: vi.fn() };
  const source = { connect: vi.fn(), disconnect: vi.fn() };
  const sink = { gain: { value: 1 }, connect: vi.fn(), disconnect: vi.fn() };
  const stream = { getTracks: () => [{ stop: vi.fn() }] };

  Object.defineProperty(navigator, "mediaDevices", {
    configurable: true,
    value: { getUserMedia: vi.fn().mockResolvedValue(stream) },
  });
  Object.defineProperty(window, "AudioContext", {
    configurable: true,
    value: class {
      sampleRate = 16_000;
      destination = {};
      createMediaStreamSource = vi.fn(() => source);
      createScriptProcessor = vi.fn(() => processor);
      createGain = vi.fn(() => sink);
      close = vi.fn().mockResolvedValue(undefined);
    },
  });
});

describe("useListening", () => {
  /* Every frame here is 512 samples AT 16 kHz — Silero's input, and the unit `hangoverFrames` and
     `PREROLL_FRAMES` count in. A context left at the device's own rate (48 kHz on an ordinary
     microphone) hands Silero speech three times too fast and shrinks every one of those durations to a
     third: a 640 ms hangover becomes ~210 ms, so each pause for breath ends a segment, and whisper gets
     fragments. This suite's fake used to report 16 kHz whatever it was asked, which is how it hid. */
  it("asks for a 16 kHz context, whatever rate the device runs at", async () => {
    const asked: Array<AudioContextOptions | undefined> = [];
    const base = window.AudioContext;
    Object.defineProperty(window, "AudioContext", {
      configurable: true,
      value: class extends (base as unknown as new () => object) {
        sampleRate: number;
        constructor(options?: AudioContextOptions) {
          super();
          asked.push(options);
          this.sampleRate = options?.sampleRate ?? 48_000;
        }
      },
    });

    const { result } = await anOpenMicrophone();
    expect(asked).toEqual([expect.objectContaining({ sampleRate: 16_000 })]);
    act(() => {
      result.current.record();
      sendFrames(1, 0.1);
    });
    expect(result.current.take()?.rate).toBe(16_000);
  });

  it("maps loudness the way this suite's amplitudes assume", () => {
    expect(energyOf(new Float32Array(FRAME_SAMPLES).fill(amplitudeFor(0.7)))).toBeCloseTo(0.7, 2);
  });

  /* The bar rises while the assistant is speaking, and it has to rise DURING an open microphone —
     the answer starts and stops many times over one session. A gate chosen when the graph was built
     would be the resting bar for the life of the microphone. */
  it("asks which gate is in force on every frame", async () => {
    let raised = true;
    const { signals } = await anOpenMicrophone({ gateNow: () => (raised ? SPEAKING_GATE : DEFAULT_GATE) });

    // Speech to the resting bar, silence to the raised one: exactly the attenuated copy an imperfect
    // canceller leaves behind, which is what `SPEAKING_GATE` exists to ignore.
    act(() => sendFrames(6, amplitudeFor(0.7)));
    expect(signals).toEqual([]);

    raised = false;
    act(() => sendFrames(DEFAULT_GATE.minSpeechFrames, amplitudeFor(0.7)));
    expect(signals).toEqual(["speechStarted"]);
  });

  /* The self-guard's whole point, and the reason refusing is not the same as ignoring. An onset the
     caller refuses leaves the gate OPEN with nothing recording; the person who really is talking then
     cannot open a turn until they stop, because a gate that is already open has no onset left to
     give. Re-arming is what makes a refusal cost one onset instead of the rest of the session. */
  it("re-arms the gate when a signal is refused, rather than merely dropping it", async () => {
    let refusing = true;
    const heard: string[] = [];
    await anOpenMicrophone({
      onSignal: (signal) => {
        heard.push(signal);
        if (!refusing) return;
        refusing = false;
        return false;
      },
    });

    act(() => sendFrames(DEFAULT_GATE.minSpeechFrames, 1));
    expect(heard).toEqual(["speechStarted"]);

    act(() => sendFrames(DEFAULT_GATE.minSpeechFrames, 1));
    expect(heard).toEqual(["speechStarted", "speechStarted"]);
  });

  /* Both numbers, because they are not the same number and the caller can recompute neither: the
     probability came from a model this module holds, and a meter drawn against the gate's own bar has
     to show the value the gate judged. Loudness is the separate evidence that the device is alive at
     all — a flat signal is a muted or wrong microphone, whatever the model says about it. */
  it("reports the probability it judged and the loudness beside it", async () => {
    silero.loadSileroSession.mockResolvedValue({ session: {}, why: null });
    silero.probability.mockResolvedValue(0.9);
    const measured: Array<{ probability: number; loudness: number }> = [];
    await anOpenMicrophone({ onMeasured: (m) => measured.push(m) });

    act(() => sendFrames(1, amplitudeFor(0.4)));

    await waitFor(() => expect(measured).toHaveLength(1));
    expect(measured[0].probability).toBeCloseTo(0.9);
    expect(measured[0].loudness).toBeCloseTo(0.4, 2);
  });

  /* Each frame carries when IT was captured, not when it was judged, because the two differ by more
     than the shortest thing anybody measures against them. A buffer holds eight frames and arrives
     once they are all in, so a callback's clock is up to 256 ms late for the frame at its front; and
     the probe's answer arrives later still. `SELF_GUARD_MS` is 350, so either error alone eats most
     of it, and the hole they leave is the assistant's own voice opening a turn. */
  it("times each frame at its own capture, not at the callback or the decision", async () => {
    const measured: Array<{ at: number }> = [];
    await anOpenMicrophone({ onMeasured: (m) => measured.push(m) });

    const before = performance.now();
    act(() => sendFrames(3, 1));
    const after = performance.now();

    expect(measured).toHaveLength(3);
    expect(measured[1].at - measured[0].at).toBeCloseTo(FRAME_MS, 3);
    expect(measured[2].at - measured[1].at).toBeCloseTo(FRAME_MS, 3);
    // The last frame of a buffer is the one captured as the callback fired; every earlier frame is
    // dated backwards from it, so the oldest may well predate the moment this test looked.
    expect(measured[2].at).toBeGreaterThanOrEqual(before);
    expect(measured[2].at).toBeLessThanOrEqual(after);
  });

  /* `noMicrophone` is the first thing `diagnoseListening` looks for and the only one of its reasons
     that nothing could produce: the caller passed `micOpen: true` because it had no way to ask. */
  it("says whether the device is open", async () => {
    const { result } = await anOpenMicrophone();
    expect(result.current.isOpen()).toBe(true);

    act(() => result.current.close());
    expect(result.current.isOpen()).toBe(false);
  });
});

describe("useListening open lifecycle", () => {
  /** A getUserMedia whose answer the test releases by hand, and the tracks it hands out. */
  function slowMicrophone() {
    const stops: Array<ReturnType<typeof vi.fn>> = [];
    const resolvers: Array<() => void> = [];
    const getUserMedia = vi.fn(
      () =>
        new Promise((resolve) => {
          const stop = vi.fn();
          stops.push(stop);
          resolvers.push(() => resolve({ getTracks: () => [{ stop }] }));
        }),
    );
    Object.defineProperty(navigator, "mediaDevices", { configurable: true, value: { getUserMedia } });
    return { stops, resolvers, getUserMedia };
  }

  const handlers = { onSignal: () => {}, onTrouble: () => {} } satisfies ListeningHandlers;

  it("stops the tracks when it was closed while the permission prompt was open", async () => {
    const mic = slowMicrophone();
    const { result } = renderHook(() => useListening(handlers));

    let opened: Promise<boolean> = Promise.resolve(true);
    await act(async () => {
      opened = result.current.open();
    });
    await waitFor(() => expect(mic.getUserMedia).toHaveBeenCalled());
    act(() => result.current.close());
    await act(async () => {
      mic.resolvers[0]();
      await opened;
    });

    expect(await opened).toBe(false);
    expect(mic.stops[0]).toHaveBeenCalled();
    expect(result.current.isOpen()).toBe(false);
  });

  it("does not open a second stream while one is opening", async () => {
    const mic = slowMicrophone();
    const { result } = renderHook(() => useListening(handlers));

    let first: Promise<boolean> = Promise.resolve(false);
    let second: Promise<boolean> = Promise.resolve(false);
    await act(async () => {
      first = result.current.open();
      second = result.current.open();
    });
    await waitFor(() => expect(mic.getUserMedia).toHaveBeenCalled());
    await act(async () => {
      for (const release of mic.resolvers) release();
      await Promise.all([first, second]);
    });

    expect(mic.getUserMedia).toHaveBeenCalledTimes(1);
    expect(await first).toBe(true);
    expect(await second).toBe(true);
  });

  it("stops the tracks when building the audio graph fails", async () => {
    const stop = vi.fn();
    Object.defineProperty(navigator, "mediaDevices", {
      configurable: true,
      value: { getUserMedia: vi.fn().mockResolvedValue({ getTracks: () => [{ stop }] }) },
    });
    Object.defineProperty(window, "AudioContext", {
      configurable: true,
      value: class {
        destination = {};
        createMediaStreamSource() {
          throw new Error("no graph");
        }
        close = vi.fn().mockResolvedValue(undefined);
      },
    });
    const onTrouble = vi.fn();
    const { result } = renderHook(() => useListening({ ...handlers, onTrouble }));

    await act(async () => {
      expect(await result.current.open()).toBe(false);
    });

    expect(stop).toHaveBeenCalled();
    expect(onTrouble).toHaveBeenCalled();
  });
});
