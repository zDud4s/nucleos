import { act, renderHook, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { energyOf, FLAT_LEVEL, FRAME_MS, FRAME_SAMPLES } from "../lib/vad";

const tauri = vi.hoisted(() => ({ listen: vi.fn() }));
vi.mock("@tauri-apps/api/event", () => tauri);

const silero = vi.hoisted(() => ({ loadSileroSession: vi.fn(), probability: 0 }));
vi.mock("../lib/silero", () => ({
  loadSileroSession: silero.loadSileroSession,
  SpeechProbe: class {
    probability(_frame: Float32Array) {
      return Promise.resolve(silero.probability);
    }

    reset() {}
  },
}));

const voice = vi.hoisted(() => ({ fetchSpeechUnit: vi.fn(), postSegment: vi.fn() }));
vi.mock("./voice", async (importOriginal) => ({
  ...(await importOriginal()),
  fetchSpeechUnit: voice.fetchSpeechUnit,
  postSegment: voice.postSegment,
}));

const client = vi.hoisted(() => ({ apiFetch: vi.fn() }));
vi.mock("./client", () => ({ apiFetch: client.apiFetch }));

import {
  SILENCE_AFTER_FRAMES,
  SILENCE_WINDOW_FRAMES,
  useVoiceConversation,
} from "./conversation";
import * as conversation from "./conversation";

interface FakeProcessor {
  onaudioprocess: ((event: AudioProcessingEvent) => void) | null;
  connect: ReturnType<typeof vi.fn>;
  disconnect: ReturnType<typeof vi.fn>;
}

let processor: FakeProcessor;
let player: { onended: (() => void) | null; onerror: (() => void) | null; play: ReturnType<typeof vi.fn> } | null;
let now: ReturnType<typeof vi.spyOn>;

const QUESTION = "como vai estar o tempo em Lisboa?";
const FORECAST = "Amanhã em Lisboa vai estar sol, com máximas de vinte graus.";

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

function amplitudeBetweenFlatAndExit(): number {
  const amplitude = [0.001, 0.003, 0.01, 0.02].find((candidate) => {
    const level = energyOf(new Float32Array(FRAME_SAMPLES).fill(candidate));
    return level > FLAT_LEVEL && level < 0.35;
  });
  if (amplitude === undefined) throw new Error("no test amplitude is between flat and the exit gate");
  return amplitude;
}

async function reachSpeaking() {
  voice.postSegment.mockResolvedValueOnce({ text: QUESTION, verdict: "closes" });
  voice.fetchSpeechUnit
    .mockResolvedValueOnce({ type: "audio", wav: new Blob(["wav"]) })
    .mockResolvedValueOnce({ type: "ended" });
  const { result } = renderHook(() => useVoiceConversation("chat-1"));

  act(() => result.current.toggle());
  await waitFor(() => expect(result.current.phase).toBe("listening"));
  act(() => {
    sendFrames(4, 1);
    sendFrames(25, 0);
  });
  await waitFor(() => expect(result.current.phase).toBe("speaking"));
  return result;
}

beforeEach(() => {
  tauri.listen.mockReset();
  tauri.listen.mockResolvedValue(() => {});
  silero.loadSileroSession.mockReset();
  // The load's shape, not a session: these suites drive the energy fallback, which is what a
  // machine with no runtime gets. `why` is what the fallback now has to carry.
  silero.loadSileroSession.mockResolvedValue({ session: null, why: "no runtime in this suite" });
  silero.probability = 0;
  client.apiFetch.mockReset();
  client.apiFetch.mockImplementation((path: string) => {
    if (path === "/assistant/message") return Promise.resolve({ turn_id: 7 });
    if (path === "/assistant/7/live") {
      return Promise.resolve({ text: FORECAST, doing: null, did: [], thought: [], thought_tokens: null });
    }
    throw new Error(`unexpected path: ${path}`);
  });
  voice.fetchSpeechUnit.mockReset();
  voice.postSegment.mockReset();
  player = null;
  now = vi.spyOn(performance, "now").mockReturnValue(0);

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
  Object.defineProperty(globalThis, "Audio", {
    configurable: true,
    value: class {
      onended: (() => void) | null = null;
      onerror: (() => void) | null = null;
      src = "";
      play = vi.fn().mockResolvedValue(undefined);
      pause = vi.fn();
      removeAttribute = vi.fn();
      constructor() {
        player = this;
      }
    },
  });
  vi.stubGlobal("URL", {
    createObjectURL: vi.fn(() => "blob:answer"),
    revokeObjectURL: vi.fn(),
  });
});

describe("the runner while answering", () => {
  it("the bar is raised while the answer plays", async () => {
    const result = await reachSpeaking();
    now.mockReturnValue(1000);

    expect(result.current.threshold).toBe(0.85);
    act(() => sendFrames(10, amplitudeBetweenFlatAndExit()));
    expect(result.current.phase).toBe("speaking");

    act(() => sendFrames(4, 1));
    expect(result.current.phase).toBe("hearing");
  });

  it("an onset in the first 350 ms of a unit is not a barge-in", async () => {
    const result = await reachSpeaking();
    now.mockReturnValue(100);

    act(() => sendFrames(4, 1));
    expect(result.current.phase).toBe("speaking");
  });

  it("the swallowed onset re-arms the gate, so a real interruption still gets through", async () => {
    const result = await reachSpeaking();
    now.mockReturnValue(100);
    act(() => sendFrames(4, 1));
    expect(result.current.phase).toBe("speaking");

    // 600 and not 400, since 2026-09-21: a frame now carries when IT was captured rather than when
    // the callback ran, and a callback at 400 holds frames captured from 304 ms — so the onset the
    // gate declares lands at 336 ms, inside the 350 ms guard. The old clock called that batch 400
    // throughout and let it through. The test's point is an interruption AFTER the guard, so the
    // clock moves; what changed is the accuracy of the measurement, not the rule.
    now.mockReturnValue(600);
    act(() => sendFrames(4, 1));
    expect(result.current.phase).toBe("hearing");
  });

  it("the answer's own words coming back are not a turn", async () => {
    const result = await reachSpeaking();
    now.mockReturnValue(1000);
    voice.postSegment.mockResolvedValueOnce({ text: "lisboa vai estar sol", verdict: "closes" });

    act(() => {
      sendFrames(4, 1);
      sendFrames(25, 0);
    });

    await waitFor(() => expect(result.current.ignoredEcho).toBe(1));
    expect(result.current.assembling).toBeNull();
    expect(client.apiFetch.mock.calls.filter(([path]) => path === "/assistant/message")).toHaveLength(1);
  });

  it("a real interruption with new words is delivered", async () => {
    const result = await reachSpeaking();
    now.mockReturnValue(1000);
    voice.postSegment.mockResolvedValueOnce({ text: "e amanhã no Porto chove", verdict: "closes" });

    act(() => {
      sendFrames(4, 1);
      sendFrames(25, 0);
    });

    await waitFor(() => expect(client.apiFetch.mock.calls.filter(([path]) => path === "/assistant/message")).toHaveLength(2));
    expect(result.current.ignoredEcho).toBe(0);
  });

  it("the answer's words come from the saved turn once the live stream has ended", async () => {
    client.apiFetch.mockImplementation((path: string) => {
      if (path === "/assistant/message") return Promise.resolve({ turn_id: 7 });
      if (path === "/assistant/7/live") return Promise.resolve(undefined);
      if (path === "/assistant/chats/chat-1") {
        return Promise.resolve({
          handed: [],
          turns: [
            {
              id: 7,
              asked: QUESTION,
              answer: FORECAST,
              error: null,
              status: "done",
              cost_usd: null,
              answered_by: null,
              session_id: null,
              created_at: "2026-09-19T00:00:00Z",
              context_fill: null,
              context_window: 0,
              compacted: false,
              did: [],
              thought: [],
              images: [],
              thought_tokens: null,
            },
          ],
          queued: [],
          asks: [],
          notices: [],
        });
      }
      throw new Error(`unexpected path: ${path}`);
    });
    const result = await reachSpeaking();
    now.mockReturnValue(1000);
    voice.postSegment.mockResolvedValueOnce({ text: "lisboa vai estar sol", verdict: "closes" });

    act(() => {
      sendFrames(4, 1);
      sendFrames(25, 0);
    });

    await waitFor(() => expect(result.current.ignoredEcho).toBe(1));
    expect(client.apiFetch.mock.calls.filter(([path]) => path === "/assistant/message")).toHaveLength(1);
  });

  it("echo is judged only near the answer", async () => {
    const result = await reachSpeaking();
    expect(player).not.toBeNull();
    act(() => player?.onended?.());
    await waitFor(() => expect(result.current.phase).toBe("listening"));
    now.mockReturnValue(2000);
    voice.postSegment.mockResolvedValueOnce({ text: "lisboa vai estar sol", verdict: "closes" });

    act(() => {
      sendFrames(4, 1);
      sendFrames(25, 0);
    });

    await waitFor(() => expect(client.apiFetch.mock.calls.filter(([path]) => path === "/assistant/message")).toHaveLength(2));
    expect(result.current.ignoredEcho).toBe(0);
  });

  it("keeps echo judgment to a one-second tail", () => {
    expect(conversation.ECHO_TAIL_MS).toBe(1000);
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

  it("the level follows the microphone", async () => {
    const { result } = renderHook(() => useVoiceConversation("chat-1"));
    const frame = new Float32Array(FRAME_SAMPLES).fill(amplitudeBetweenFlatAndExit());
    const expectedLevel = Number(energyOf(frame).toFixed(2));

    expect(energyOf(frame)).toBeGreaterThan(FLAT_LEVEL);
    expect(energyOf(frame)).toBeLessThan(0.35);
    expect(result.current.level).toBe(0);

    act(() => result.current.toggle());
    await waitFor(() => expect(result.current.phase).toBe("listening"));

    act(() => sendFrames(1, frame[0]));
    expect(result.current.level).toBe(expectedLevel);

    act(() => sendFrames(1, 0));
    expect(result.current.level).toBe(0);
  });

  it("a steady signal does not re-render the view", async () => {
    let renders = 0;
    const { result } = renderHook(() => {
      renders += 1;
      return useVoiceConversation("chat-1");
    });

    act(() => result.current.toggle());
    await waitFor(() => expect(result.current.phase).toBe("listening"));
    const rendersBeforeFrames = renders;
    const amplitude = amplitudeBetweenFlatAndExit();

    act(() => sendFrames(64, amplitude));

    expect(result.current.level).toBe(Number(energyOf(new Float32Array(FRAME_SAMPLES).fill(amplitude)).toFixed(2)));
    expect(renders - rendersBeforeFrames).toBeLessThanOrEqual(2);
  });

  it("with Silero the meter shows the probability the gate judges, not loudness", async () => {
    silero.loadSileroSession.mockResolvedValue({ session: {}, why: null });
    silero.probability = 0.7;
    const { result } = renderHook(() => useVoiceConversation("chat-1"));

    act(() => result.current.toggle());
    await waitFor(() => expect(result.current.phase).toBe("listening"));
    act(() => sendFrames(4, 0));

    await waitFor(() => expect(result.current.level).toBe(0.7));
    expect(result.current.listeningWith).toBe("silero");
  });

  it("with Silero, audible sound that is not speech is explained as below the bar", async () => {
    silero.loadSileroSession.mockResolvedValue({ session: {}, why: null });
    silero.probability = 0.2;
    const { result } = renderHook(() => useVoiceConversation("chat-1"));

    act(() => result.current.toggle());
    await waitFor(() => expect(result.current.phase).toBe("listening"));
    act(() => sendFrames(SILENCE_AFTER_FRAMES, amplitudeBetweenFlatAndExit()));

    await waitFor(() => expect(result.current.silence).toBe("belowThreshold"));
  });

  it("a barge-in segment is judged by its own recording's scope, even when the next one has started", async () => {
    let resolveSegment!: (value: { text: string; verdict: "continues" }) => void;
    const inFlight = new Promise<{ text: string; verdict: "continues" }>((resolve) => {
      resolveSegment = resolve;
    });
    const result = await reachSpeaking();
    now.mockReturnValue(1000);
    voice.postSegment.mockReturnValueOnce(inFlight);

    act(() => {
      sendFrames(4, 1);
      sendFrames(25, 0);
    });
    now.mockReturnValue(5000);
    act(() => sendFrames(4, 1));
    resolveSegment({ text: "lisboa vai estar sol", verdict: "continues" });

    await waitFor(() => expect(result.current.ignoredEcho).toBe(1));
  });

  it("small changes in level do not re-render the composer", async () => {
    expect(conversation.LEVEL_STEP).toBe(0.05);
    let renders = 0;
    const { result } = renderHook(() => {
      renders += 1;
      return useVoiceConversation("chat-1");
    });
    const candidates = Array.from({ length: 190 }, (_, index) => 0.001 + index * 0.0001);
    const pair = candidates.flatMap((a) =>
      candidates.map((b) => ({
        a,
        b,
        difference: Math.abs(
          Number(energyOf(new Float32Array(FRAME_SAMPLES).fill(a)).toFixed(2)) -
            Number(energyOf(new Float32Array(FRAME_SAMPLES).fill(b)).toFixed(2)),
        ),
      })),
    ).find(({ a, b, difference }) => {
      const aLevel = energyOf(new Float32Array(FRAME_SAMPLES).fill(a));
      const bLevel = energyOf(new Float32Array(FRAME_SAMPLES).fill(b));
      return (
        aLevel > FLAT_LEVEL &&
        aLevel < 0.35 &&
        bLevel > FLAT_LEVEL &&
        bLevel < 0.35 &&
        difference > 0 &&
        difference < 0.05
      );
    });
    if (pair === undefined) throw new Error("no level pair is below LEVEL_STEP");
    const aLevel = Number(energyOf(new Float32Array(FRAME_SAMPLES).fill(pair.a)).toFixed(2));
    const bLevel = Number(energyOf(new Float32Array(FRAME_SAMPLES).fill(pair.b)).toFixed(2));

    expect(Math.abs(aLevel - bLevel)).toBeGreaterThan(0);
    expect(Math.abs(aLevel - bLevel)).toBeLessThan(conversation.LEVEL_STEP);
    act(() => result.current.toggle());
    await waitFor(() => expect(result.current.phase).toBe("listening"));
    act(() => sendFrames(1, pair.a));
    const rendersAfterA = renders;
    for (let index = 0; index < 32; index += 1) act(() => sendFrames(1, index % 2 === 0 ? pair.a : pair.b));
    expect(renders - rendersAfterA).toBeLessThanOrEqual(1);

    const next = candidates.find(
      (amplitude) =>
        Math.abs(
          Number(energyOf(new Float32Array(FRAME_SAMPLES).fill(amplitude)).toFixed(2)) - aLevel,
        ) >= 0.05,
    );
    if (next === undefined) throw new Error("no level is at least LEVEL_STEP away");
    const nextLevel = Number(energyOf(new Float32Array(FRAME_SAMPLES).fill(next)).toFixed(2));
    act(() => sendFrames(1, next));
    expect(result.current.level).toBe(nextLevel);
  });

  it("the bar in force is the resting one while listening", async () => {
    const { result } = renderHook(() => useVoiceConversation("chat-1"));

    act(() => result.current.toggle());
    await waitFor(() => expect(result.current.phase).toBe("listening"));

    expect(result.current.threshold).toBe(0.6);
  });

  it("a quiet microphone is explained only after a while", async () => {
    const { result } = renderHook(() => useVoiceConversation("chat-1"));

    act(() => result.current.toggle());
    await waitFor(() => expect(result.current.phase).toBe("listening"));

    act(() => sendFrames(SILENCE_AFTER_FRAMES - 8, 0));
    expect(result.current.silence).toBeNull();

    act(() => sendFrames(16, 0));
    expect(result.current.silence).toBe("flat");
  });

  it("sound that is not loud enough to open a turn is explained as the loudness fallback", async () => {
    const { result } = renderHook(() => useVoiceConversation("chat-1"));

    act(() => result.current.toggle());
    await waitFor(() => expect(result.current.phase).toBe("listening"));
    act(() => sendFrames(SILENCE_AFTER_FRAMES, 0));
    expect(result.current.silence).toBe("flat");

    act(() => sendFrames(SILENCE_WINDOW_FRAMES, amplitudeBetweenFlatAndExit()));
    expect(result.current.silence).toBe("byLoudness");
  });

  it("speaking clears the explanation", async () => {
    const { result } = renderHook(() => useVoiceConversation("chat-1"));

    act(() => result.current.toggle());
    await waitFor(() => expect(result.current.phase).toBe("listening"));
    act(() => sendFrames(SILENCE_AFTER_FRAMES, 0));
    expect(result.current.silence).toBe("flat");

    act(() => sendFrames(4, 1));
    expect(result.current.phase).toBe("hearing");
    expect(result.current.silence).toBeNull();
  });

  it("what has been understood so far is shown while the turn is built", async () => {
    voice.postSegment
      .mockResolvedValueOnce({ text: "marca uma reunião", verdict: "continues" })
      .mockResolvedValueOnce({ text: "para amanhã", verdict: "continues" });
    const { result } = renderHook(() => useVoiceConversation("chat-1"));

    act(() => result.current.toggle());
    await waitFor(() => expect(result.current.phase).toBe("listening"));
    act(() => {
      sendFrames(4, 1);
      sendFrames(25, 0);
    });
    await waitFor(() => expect(result.current.assembling).toBe("marca uma reunião"));

    act(() => {
      sendFrames(4, 1);
      sendFrames(25, 0);
    });
    await waitFor(() => expect(result.current.assembling).toBe("marca uma reunião para amanhã"));
  });
});
