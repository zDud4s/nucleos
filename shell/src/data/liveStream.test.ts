import { beforeEach, describe, expect, it, vi } from "vitest";

const daemon = vi.hoisted(() => ({ apiStream: vi.fn() }));
vi.mock("./client", () => daemon);

import { sseFrames, streamLiveTurn } from "./liveStream";

const encoder = new TextEncoder();

/** A response whose body yields these chunks and then ends, as a stream the daemon closed. */
function responseOf(chunks: string[], status = 200) {
  const queue = [...chunks];
  return {
    status,
    body: {
      getReader: () => ({
        read: async () => {
          const next = queue.shift();
          return next === undefined
            ? { done: true, value: undefined }
            : { done: false, value: encoder.encode(next) };
        },
        cancel: async () => {},
        releaseLock: () => {},
      }),
    },
  };
}

const live = (text: string) =>
  `event: live\ndata: ${JSON.stringify({ text, doing: null, did: [], thought: [], thought_tokens: null })}\n\n`;

beforeEach(() => {
  daemon.apiStream.mockReset();
});

describe("sseFrames", () => {
  it("parses event frames split across chunks", () => {
    const first = sseFrames(": keep-alive\n\nevent: live\ndata: {\"text\":");
    expect(first.events).toEqual([]);
    expect(first.rest).toContain("event: live");

    const second = sseFrames(first.rest + '"hi"}\n\nevent: end\ndata: {}\n\n');
    expect(second.events).toEqual([
      { event: "live", data: '{"text":"hi"}' },
      { event: "end", data: "{}" },
    ]);
    expect(second.rest).toBe("");
  });
});

describe("streamLiveTurn", () => {
  it("reports each live state and the end", async () => {
    // One frame is split across two chunks on purpose.
    const whole = live("second");
    const cut = Math.floor(whole.length / 2);
    daemon.apiStream.mockResolvedValue(
      responseOf([live("first"), whole.slice(0, cut), whole.slice(cut), "event: end\ndata: {}\n\n"]),
    );
    const seen: string[] = [];

    const outcome = await streamLiveTurn(7, {
      signal: new AbortController().signal,
      onLive: (state) => seen.push(state.text),
    });

    expect(outcome).toBe("ended");
    expect(seen).toEqual(["first", "second"]);
    expect(String(daemon.apiStream.mock.calls[0][0])).toBe("/assistant/7/live/stream");
  });

  it("reports failure when the stream drops without an end", async () => {
    daemon.apiStream.mockResolvedValue(responseOf([live("only")]));
    const seen: string[] = [];

    const outcome = await streamLiveTurn(7, {
      signal: new AbortController().signal,
      onLive: (state) => seen.push(state.text),
    });

    expect(outcome).toBe("failed");
    expect(seen).toEqual(["only"]);

    daemon.apiStream.mockResolvedValue(responseOf([], 204));
    expect(
      await streamLiveTurn(7, { signal: new AbortController().signal, onLive: () => {} }),
    ).toBe("failed");

    daemon.apiStream.mockRejectedValue(new Error("refused"));
    expect(
      await streamLiveTurn(7, { signal: new AbortController().signal, onLive: () => {} }),
    ).toBe("failed");
  });
});
