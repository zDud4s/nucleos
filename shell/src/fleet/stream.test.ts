// @vitest-environment node
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { startFleetStream } from "./stream";
import { ndjson, node, snapshot } from "./test-fixtures";
import type { ConnectionState, FleetEvent } from "./types";

// `stream.ts` imports the real `openStream`, which imports the Tauri bridge; every test injects its own.
vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

const enc = new TextEncoder();

function pipe() {
  let controller!: ReadableStreamDefaultController<Uint8Array>;
  const stream = new ReadableStream<Uint8Array>({ start: (c) => void (controller = c) });
  return {
    stream,
    send: (text: string) => controller.enqueue(enc.encode(text)),
    close: () => controller.close(),
  };
}

function setup(opens: Array<() => Promise<ReadableStream<Uint8Array>>>) {
  const events: FleetEvent[] = [];
  const states: ConnectionState[] = [];
  const signals: AbortSignal[] = [];
  let calls = 0;
  const open = vi.fn(async (_path: string, signal: AbortSignal) => {
    signals.push(signal);
    return await opens[Math.min(calls++, opens.length - 1)]();
  });
  const stop = startFleetStream({ onEvent: (e) => events.push(e), onState: (s) => states.push(s) }, open);
  return { events, states, signals, open, stop };
}

const flush = () => vi.advanceTimersByTimeAsync(0);

beforeEach(() => void vi.useFakeTimers());
afterEach(() => void vi.useRealTimers());

describe("startFleetStream", () => {
  it("reads events split across chunks and reports live on the first snapshot", async () => {
    const p = pipe();
    const { events, states } = setup([async () => p.stream]);
    await flush();
    const body = ndjson(snapshot([node("a:1")]));
    p.send(body.slice(0, 20));
    await flush();
    expect(events).toEqual([]);
    p.send(body.slice(20));
    await flush();
    expect(events).toEqual([snapshot([node("a:1")])]);
    expect(states).toEqual(["connecting", "live"]);
  });

  it("reconnects with a growing backoff: 1 s, then 2 s", async () => {
    const { open, states } = setup([
      async () => Promise.reject(new Error("down")),
      async () => Promise.reject(new Error("down")),
      async () => new ReadableStream<Uint8Array>(),
    ]);
    await flush();
    expect(open).toHaveBeenCalledTimes(1);
    expect(states).toEqual(["connecting", "reconnecting"]);
    await vi.advanceTimersByTimeAsync(999);
    expect(open).toHaveBeenCalledTimes(1);
    await vi.advanceTimersByTimeAsync(1);
    expect(open).toHaveBeenCalledTimes(2);
    await vi.advanceTimersByTimeAsync(1999);
    expect(open).toHaveBeenCalledTimes(2);
    await vi.advanceTimersByTimeAsync(1);
    expect(open).toHaveBeenCalledTimes(3);
  });

  it("resets the backoff once a snapshot arrives", async () => {
    const first = pipe();
    const { open } = setup([
      async () => Promise.reject(new Error("down")),
      async () => first.stream,
      async () => new ReadableStream<Uint8Array>(),
    ]);
    await vi.advanceTimersByTimeAsync(1000); // second attempt
    first.send(ndjson(snapshot([])));
    await flush();
    first.close();
    await flush();
    expect(open).toHaveBeenCalledTimes(2);
    await vi.advanceTimersByTimeAsync(1000); // back to 1 s, not 2 s
    expect(open).toHaveBeenCalledTimes(3);
  });

  it("treats 30 s without any byte as a dead connection and reconnects", async () => {
    const quiet = pipe();
    const { open, states, signals } = setup([async () => quiet.stream, async () => new ReadableStream<Uint8Array>()]);
    await flush();
    await vi.advanceTimersByTimeAsync(29_999);
    expect(signals[0].aborted).toBe(false);
    await vi.advanceTimersByTimeAsync(1);
    expect(signals[0].aborted).toBe(true);
    expect(states[states.length - 1]).toBe("reconnecting");
    await vi.advanceTimersByTimeAsync(1000);
    expect(open).toHaveBeenCalledTimes(2);
  });

  it("any byte, a heartbeat included, restarts the silence clock", async () => {
    const p = pipe();
    const { signals } = setup([async () => p.stream]);
    await flush();
    await vi.advanceTimersByTimeAsync(20_000);
    p.send(ndjson({ t: "heartbeat" }));
    await vi.advanceTimersByTimeAsync(29_999);
    expect(signals[0].aborted).toBe(false);
  });

  it("stop() aborts the connection and never opens another", async () => {
    const p = pipe();
    const { open, signals, stop } = setup([async () => p.stream]);
    await flush();
    stop();
    await vi.advanceTimersByTimeAsync(60_000);
    expect(signals[0].aborted).toBe(true);
    expect(open).toHaveBeenCalledTimes(1);
  });
});
