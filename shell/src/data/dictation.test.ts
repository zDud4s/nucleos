import { beforeEach, describe, expect, it, vi } from "vitest";
import { act, renderHook, waitFor } from "@testing-library/react";

/* The audio graph and the route, which are the two things jsdom cannot have: it has no
   `AudioContext`, no `getUserMedia`, and no daemon. What is left under test is the whole of what
   this hook decides — and since 2026-09-20 that is a choreography rather than a single round trip,
   so the fake microphone below is driven frame by frame. */
const listening = vi.hoisted(() => ({ useListening: vi.fn() }));
vi.mock("./listening", () => listening);

const voice = vi.hoisted(() => ({ postCapture: vi.fn(), postDraft: vi.fn() }));
vi.mock("./voice", async (original) => ({
  ...(await original<typeof import("./voice")>()),
  ...voice,
}));

import { DRAFT_EVERY_FRAMES, useDictation } from "./dictation";
import type { ListeningHandlers } from "./listening";

/** A microphone with no audio in it, whose handlers the test calls in place of the device. */
function aMicrophone() {
  const mic = {
    handlers: null as ListeningHandlers | null,
    open: vi.fn().mockResolvedValue(true),
    close: vi.fn(),
    record: vi.fn(),
    peek: vi.fn(() => ({ samples: new Float32Array(8000), rate: 16000, ms: 500 })),
    take: vi.fn(() => ({ samples: new Float32Array(32000), rate: 16000, ms: 2000 })),
    isRecording: vi.fn(() => true),
  };
  listening.useListening.mockImplementation((handlers: ListeningHandlers) => {
    mic.handlers = handlers;
    return mic;
  });
  return mic;
}

/** Enough frames of somebody talking for the loop to want another draft. */
function talks(mic: ReturnType<typeof aMicrophone>, frames = DRAFT_EVERY_FRAMES) {
  act(() => {
    for (let at = 0; at < frames; at += 1) mic.handlers?.onFrame?.(true);
  });
}

/** What the box was told, as `[text, final]` pairs. */
function recorder() {
  const said: Array<[string, boolean]> = [];
  return { said, onText: (text: string, final: boolean) => said.push([text, final]) };
}

beforeEach(() => {
  listening.useListening.mockReset();
  voice.postCapture.mockReset();
  voice.postDraft.mockReset();
});

describe("useDictation", () => {
  it("opens the microphone on the first press and closes it on the second", async () => {
    const mic = aMicrophone();
    const { result } = renderHook(() => useDictation(() => {}));

    expect(result.current.phase).toBe("off");
    act(() => result.current.toggle());
    await waitFor(() => expect(result.current.phase).toBe("listening"));
    expect(mic.open).toHaveBeenCalledTimes(1);

    mic.isRecording.mockReturnValue(false);
    act(() => result.current.toggle());
    await waitFor(() => expect(result.current.phase).toBe("off"));
    expect(mic.close).toHaveBeenCalled();
  });

  /* The point of the whole thing: words while somebody is still talking, each revision replacing the
     last rather than following it. "come" then "come view" then "câmbio" is not a made-up example —
     it is what `ggml-base` actually did to "câmbio" on this machine on 2026-09-18. */
  it("revises the box while the sentence is still being spoken", async () => {
    const mic = aMicrophone();
    const box = recorder();
    const { result } = renderHook(() => useDictation(box.onText));
    act(() => result.current.toggle());
    await waitFor(() => expect(result.current.phase).toBe("listening"));

    act(() => mic.handlers?.onSignal("speechStarted"));
    expect(mic.record).toHaveBeenCalledTimes(1);

    voice.postDraft.mockResolvedValue("come");
    talks(mic);
    await waitFor(() => expect(box.said).toEqual([["come", false]]));

    voice.postDraft.mockResolvedValue("come view");
    talks(mic);
    await waitFor(() => expect(box.said).toHaveLength(2));
    expect(box.said[1]).toEqual(["come view", false]);

    expect(voice.postCapture).not.toHaveBeenCalled();
  });

  /* One in flight at a time. Without this the loop would post a draft every 256 ms regardless of how
     long the last one took, and a transcriber having a slow moment would be handed a queue it can
     only fall further behind — the same failure `listening.ts` drops frames to avoid. */
  it("never has two drafts in flight at once", async () => {
    const mic = aMicrophone();
    let answer: (text: string) => void = () => {};
    voice.postDraft.mockImplementation(() => new Promise<string>((done) => (answer = done)));
    const { result } = renderHook(() => useDictation(() => {}));
    act(() => result.current.toggle());
    await waitFor(() => expect(result.current.phase).toBe("listening"));
    act(() => mic.handlers?.onSignal("speechStarted"));

    talks(mic);
    talks(mic);
    talks(mic);
    await waitFor(() => expect(voice.postDraft).toHaveBeenCalledTimes(1));

    await act(async () => answer("olá"));
    talks(mic);
    await waitFor(() => expect(voice.postDraft).toHaveBeenCalledTimes(2));
  });

  /* The sentence that ends goes as `dictation`, which is the call that writes the row — and it is
     the same audio the drafts were guesses at, so the text does not change under somebody's feet
     after they have stopped looking at it. */
  it("writes down the sentence that ended, and that is the last word on it", async () => {
    const mic = aMicrophone();
    const box = recorder();
    voice.postCapture.mockResolvedValue({ text: "câmbio" });
    const { result } = renderHook(() => useDictation(box.onText));
    act(() => result.current.toggle());
    await waitFor(() => expect(result.current.phase).toBe("listening"));

    act(() => mic.handlers?.onSignal("speechStarted"));
    await act(async () => mic.handlers?.onSignal("speechEnded"));

    await waitFor(() => expect(box.said).toEqual([["câmbio", true]]));
    expect(voice.postCapture).toHaveBeenCalledWith(expect.any(Uint8Array), "dictation", 2000);
    // Still listening: a second sentence follows the first without anybody pressing anything.
    expect(result.current.phase).toBe("listening");
  });

  /* The defect this hook had until 2026-09-20: `toggle` refused to start while a transcription was
     in flight, which was right when a dictation was one recording and one round trip. Sentences
     overlap now — the second begins while the first is still being written down — and a microphone
     that went deaf for the length of a round trip would eat the start of every other sentence. */
  it("records the next sentence while the last one is still being written down", async () => {
    const mic = aMicrophone();
    const box = recorder();
    let answer: (result: { text: string }) => void = () => {};
    voice.postCapture.mockImplementation(
      () => new Promise<{ text: string }>((done) => (answer = done)),
    );
    const { result } = renderHook(() => useDictation(box.onText));
    act(() => result.current.toggle());
    await waitFor(() => expect(result.current.phase).toBe("listening"));

    act(() => mic.handlers?.onSignal("speechStarted"));
    act(() => mic.handlers?.onSignal("speechEnded"));
    act(() => mic.handlers?.onSignal("speechStarted"));

    expect(mic.record).toHaveBeenCalledTimes(2);
    // The first sentence is genuinely in flight — this is what "still being written down" means, and
    // answering before it was asked would resolve a promise that does not exist yet.
    await waitFor(() => expect(voice.postCapture).toHaveBeenCalledTimes(1));
    await act(async () => answer({ text: "a primeira" }));
    await waitFor(() => expect(box.said).toEqual([["a primeira", true]]));
  });

  /* A draft is a guess at a sentence that is still open. Once that sentence has been written down,
     its guesses are worth less than what replaced them — and one arriving late would re-open a
     region over text the person has stopped expecting to move. */
  it("drops a draft that arrives after its own sentence closed", async () => {
    const mic = aMicrophone();
    const box = recorder();
    let answerDraft: (text: string) => void = () => {};
    voice.postDraft.mockImplementation(() => new Promise<string>((done) => (answerDraft = done)));
    voice.postCapture.mockResolvedValue({ text: "a frase toda" });
    const { result } = renderHook(() => useDictation(box.onText));
    act(() => result.current.toggle());
    await waitFor(() => expect(result.current.phase).toBe("listening"));

    act(() => mic.handlers?.onSignal("speechStarted"));
    talks(mic);
    await waitFor(() => expect(voice.postDraft).toHaveBeenCalledTimes(1));

    await act(async () => mic.handlers?.onSignal("speechEnded"));
    await waitFor(() => expect(box.said).toEqual([["a frase toda", true]]));

    await act(async () => answerDraft("a frase"));
    expect(box.said).toEqual([["a frase toda", true]]);
  });

  /* 204: the microphone was open and nothing in it was speech. An empty final revision is what takes
     the drafts back out of the box — see `lib/provisional.ts` — so the person is not left holding a
     guess at a sentence nobody said. */
  it("takes its own words back when the daemon heard nothing", async () => {
    const mic = aMicrophone();
    const box = recorder();
    voice.postCapture.mockResolvedValue(undefined);
    const { result } = renderHook(() => useDictation(box.onText));
    act(() => result.current.toggle());
    await waitFor(() => expect(result.current.phase).toBe("listening"));

    act(() => mic.handlers?.onSignal("speechStarted"));
    await act(async () => mic.handlers?.onSignal("speechEnded"));

    await waitFor(() => expect(box.said).toEqual([["", true]]));
    expect(result.current.trouble).toBe("nothing was heard");
  });

  /* Pressing stop mid-sentence is the ordinary way a dictation ends: nobody waits out the hangover.
     The half-said sentence still has to be written down, or the last thing anybody said is lost. */
  it("writes down a sentence that was still open when the microphone was closed", async () => {
    const mic = aMicrophone();
    const box = recorder();
    voice.postCapture.mockResolvedValue({ text: "por fim" });
    const { result } = renderHook(() => useDictation(box.onText));
    act(() => result.current.toggle());
    await waitFor(() => expect(result.current.phase).toBe("listening"));
    act(() => mic.handlers?.onSignal("speechStarted"));

    act(() => result.current.toggle());
    await waitFor(() => expect(result.current.phase).toBe("off"));
    expect(box.said).toEqual([["por fim", true]]);
  });

  it("says so when the microphone will not open, and stays off", async () => {
    const mic = aMicrophone();
    mic.open.mockImplementation(async () => {
      mic.handlers?.onTrouble("the microphone could not be opened");
      return false;
    });
    const { result } = renderHook(() => useDictation(() => {}));

    act(() => result.current.toggle());
    await waitFor(() => expect(result.current.trouble).toBe("the microphone could not be opened"));
    expect(result.current.phase).toBe("off");
  });

  /* Whatever `listening.ts` says about the detector it got is the person's to see: a gate running on
     loudness opens a sentence on a fridge, and that reads as a broken microphone unless it is named. */
  it("carries the reason the good detector was not used", async () => {
    const mic = aMicrophone();
    mic.open.mockImplementation(async () => {
      mic.handlers?.onDetector?.("energy", "the policy refuses to compile WebAssembly");
      return true;
    });
    const { result } = renderHook(() => useDictation(() => {}));

    act(() => result.current.toggle());
    await waitFor(() =>
      expect(result.current.trouble).toBe(
        "no speech gate — the policy refuses to compile WebAssembly",
      ),
    );
  });
});
