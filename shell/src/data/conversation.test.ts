import { act, renderHook, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { FRAME_MS, FRAME_SAMPLES } from "../lib/vad";

const tauri = vi.hoisted(() => ({ listen: vi.fn() }));
vi.mock("@tauri-apps/api/event", () => tauri);

const silero = vi.hoisted(() => ({ loadSileroSession: vi.fn() }));
vi.mock("../lib/silero", () => ({
  loadSileroSession: silero.loadSileroSession,
  SpeechProbe: class {},
}));

import { useVoiceConversation } from "./conversation";

interface FakeProcessor {
  onaudioprocess: ((event: AudioProcessingEvent) => void) | null;
  connect: ReturnType<typeof vi.fn>;
  disconnect: ReturnType<typeof vi.fn>;
}

let processor: FakeProcessor;

function sendFrames(count: number, amplitude: number): void {
  const callback = processor.onaudioprocess;
  if (callback === null) throw new Error("the microphone has no audio callback");

  const batchFrames = 8;
  const fullBatch = new Float32Array(FRAME_SAMPLES * batchFrames).fill(amplitude);
  let remaining = count;
  while (remaining >= batchFrames) {
    callback({ inputBuffer: { getChannelData: () => fullBatch } } as unknown as AudioProcessingEvent);
    remaining -= batchFrames;
  }
  if (remaining > 0) {
    const tail = new Float32Array(FRAME_SAMPLES * remaining).fill(amplitude);
    callback({ inputBuffer: { getChannelData: () => tail } } as unknown as AudioProcessingEvent);
  }
}

beforeEach(() => {
  tauri.listen.mockReset();
  tauri.listen.mockResolvedValue(() => {});
  silero.loadSileroSession.mockReset();
  // The load's shape, not a session: these suites drive the energy fallback, which is what a
  // machine with no runtime gets. `why` is what the fallback now has to carry.
  silero.loadSileroSession.mockResolvedValue({ session: null, why: "no runtime in this suite" });

  processor = {
    onaudioprocess: null,
    connect: vi.fn(),
    disconnect: vi.fn(),
  };
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

describe("useVoiceConversation", () => {
  it("does not abandon the mode while speech is being recorded", async () => {
    const { result } = renderHook(() => useVoiceConversation("chat-1"));

    act(() => result.current.toggle());
    await waitFor(() => expect(result.current.phase).toBe("listening"));

    act(() => {
      // Four speech frames open the recording and cross the ten-minute limit on its next frame.
      // Counting that frame as idle abandons the mode in the middle of the first word.
      sendFrames(10 * 60_000 / FRAME_MS - 4, 0);
      sendFrames(4, 1);
    });

    expect(result.current.phase).toBe("hearing");
  });
});
