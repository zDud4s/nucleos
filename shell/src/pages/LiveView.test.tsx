import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, render, screen } from "@testing-library/react";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

const daemon = vi.hoisted(() => ({ openStream: vi.fn() }));
vi.mock("../data/client", async (original) => ({
  ...(await original<typeof import("../data/client")>()),
  ...daemon,
}));

import { LiveView } from "./LiveView";
import { ApiRefusal } from "../data/client";

/* ------------------------------------------------------------- helpers -- */

function record(type: string, body: Uint8Array): Uint8Array {
  const out = new Uint8Array(5 + body.length);
  out[0] = type.charCodeAt(0);
  new DataView(out.buffer).setUint32(1, body.length, false);
  out.set(body, 5);
  return out;
}
const frame = (...bytes: number[]) => record("F", new Uint8Array(bytes));
const end = (reason: string) => record("E", new TextEncoder().encode(JSON.stringify({ reason })));

/** A stream the test feeds by hand. */
function openPipe() {
  let controller!: ReadableStreamDefaultController<Uint8Array>;
  const stream = new ReadableStream<Uint8Array>({
    start(c) {
      controller = c;
    },
  });
  return { stream, send: (chunk: Uint8Array) => controller.enqueue(chunk) };
}

/** A stream that delivers these chunks and ends. */
function streamOf(...chunks: Uint8Array[]): ReadableStream<Uint8Array> {
  return new ReadableStream<Uint8Array>({
    start(c) {
      for (const chunk of chunks) c.enqueue(chunk);
      c.close();
    },
  });
}

/** Let promises and timers settle, inside act. */
async function settle(ms = 0) {
  await act(async () => {
    await vi.advanceTimersByTimeAsync(ms);
  });
  await act(async () => {
    await vi.advanceTimersByTimeAsync(0);
  });
}

function setVisibility(state: "visible" | "hidden") {
  Object.defineProperty(document, "visibilityState", { configurable: true, get: () => state });
  document.dispatchEvent(new Event("visibilitychange"));
}

const created = { n: 0 };
const createObjectURL = vi.fn();
const revokeObjectURL = vi.fn();

beforeEach(() => {
  vi.useFakeTimers();
  daemon.openStream.mockReset();
  created.n = 0;
  createObjectURL.mockReset();
  createObjectURL.mockImplementation(() => `blob:live-${++created.n}`);
  revokeObjectURL.mockReset();
  Object.defineProperty(URL, "createObjectURL", { configurable: true, writable: true, value: createObjectURL });
  Object.defineProperty(URL, "revokeObjectURL", { configurable: true, writable: true, value: revokeObjectURL });
  setVisibility("visible");
});

afterEach(() => {
  cleanup();
  vi.useRealTimers();
});

/* ---------------------------------------------------------------- tests -- */

describe("LiveView", () => {
  it("shows each frame and revokes the previous blob URL", async () => {
    const pipe = openPipe();
    daemon.openStream.mockResolvedValue(pipe.stream);

    render(<LiveView sessionId={7} />);
    await settle();
    expect(daemon.openStream).toHaveBeenCalledTimes(1);
    expect(daemon.openStream.mock.calls[0][0]).toBe("/browser/sessions/7/live");

    pipe.send(frame(1, 2, 3));
    await settle();
    const img = screen.getByAltText("live view of session 7") as HTMLImageElement;
    expect(img.getAttribute("src")).toBe("blob:live-1");
    expect(revokeObjectURL).not.toHaveBeenCalled();

    pipe.send(frame(4, 5, 6));
    await settle();
    expect(img.getAttribute("src")).toBe("blob:live-2");
    expect(revokeObjectURL).toHaveBeenCalledWith("blob:live-1");
    expect(revokeObjectURL).not.toHaveBeenCalledWith("blob:live-2");

    // The marker says frames are flowing.
    expect(screen.getByText(/live/i)).toBeDefined();
  });

  it("ends on wheel with its sentence and does not reconnect", async () => {
    daemon.openStream.mockImplementation(async () => streamOf(end("wheel")));

    render(<LiveView sessionId={7} />);
    await settle();

    expect(screen.getByText("The wheel was asked for, or a person has it.")).toBeDefined();
    await settle(120_000);
    expect(daemon.openStream).toHaveBeenCalledTimes(1);
  });

  it("clears the last frame and revokes its URL when the wheel is asked for", async () => {
    const pipe = openPipe();
    daemon.openStream.mockResolvedValue(pipe.stream);

    render(<LiveView sessionId={7} />);
    await settle();
    pipe.send(frame(1, 2, 3));
    await settle();
    expect(screen.getByAltText("live view of session 7").getAttribute("src")).toBe("blob:live-1");

    pipe.send(end("wheel"));
    await settle();

    expect(screen.getByText("The wheel was asked for, or a person has it.")).toBeDefined();
    expect(screen.queryByRole("img")).toBeNull();
    expect(revokeObjectURL).toHaveBeenCalledWith("blob:live-1");
  });

  it("ends on closed and does not reconnect", async () => {
    daemon.openStream.mockImplementation(async () => streamOf(frame(1), end("closed")));

    render(<LiveView sessionId={7} />);
    await settle();

    expect(screen.getByText("Session closed.")).toBeDefined();
    await settle(120_000);
    expect(daemon.openStream).toHaveBeenCalledTimes(1);
  });

  it("reconnects after gone with backoff capped at 30s", async () => {
    daemon.openStream.mockImplementation(async () => streamOf(end("gone")));

    render(<LiveView sessionId={7} />);
    await settle();
    expect(daemon.openStream).toHaveBeenCalledTimes(1);
    expect(screen.getByText(/Connection lost — reconnecting/)).toBeDefined();

    const delays = [1000, 2000, 4000, 8000, 16000, 30000, 30000];
    let calls = 1;
    for (const delay of delays) {
      await settle(delay - 1);
      expect(daemon.openStream).toHaveBeenCalledTimes(calls);
      await settle(1);
      calls += 1;
      expect(daemon.openStream).toHaveBeenCalledTimes(calls);
    }
  });

  it("a 409 shows the refusal detail and does not reconnect", async () => {
    daemon.openStream.mockRejectedValue(new ApiRefusal(409, "conflict", "the agent has let go of the wheel"));

    render(<LiveView sessionId={7} />);
    await settle();

    expect(screen.getByText(/the agent has let go of the wheel/)).toBeDefined();
    await settle(120_000);
    expect(daemon.openStream).toHaveBeenCalledTimes(1);
  });

  it("stops streaming while the window is hidden", async () => {
    const first = openPipe();
    const second = openPipe();
    daemon.openStream.mockResolvedValueOnce(first.stream).mockResolvedValueOnce(second.stream);

    render(<LiveView sessionId={7} />);
    await settle();
    first.send(frame(1));
    await settle();
    const signal = daemon.openStream.mock.calls[0][1] as AbortSignal;
    expect(signal.aborted).toBe(false);

    act(() => setVisibility("hidden"));
    await settle();
    expect(signal.aborted).toBe(true);
    expect(revokeObjectURL).toHaveBeenCalledWith("blob:live-1");
    await settle(120_000);
    expect(daemon.openStream).toHaveBeenCalledTimes(1);

    act(() => setVisibility("visible"));
    await settle();
    expect(daemon.openStream).toHaveBeenCalledTimes(2);
  });

  it("says how long the page has been unchanged after 5s", async () => {
    const pipe = openPipe();
    daemon.openStream.mockResolvedValue(pipe.stream);

    render(<LiveView sessionId={7} />);
    await settle();
    pipe.send(frame(1));
    await settle();

    await settle(3000);
    expect(screen.queryByText(/unchanged for/)).toBeNull();

    await settle(4000);
    expect(screen.getByText(/unchanged for 7s/)).toBeDefined();

    // A new frame ends the silence.
    pipe.send(frame(2));
    await settle();
    expect(screen.queryByText(/unchanged for/)).toBeNull();
  });

  it("both CSPs admit blob images", () => {
    const conf = JSON.parse(
      readFileSync(join(dirname(fileURLToPath(import.meta.url)), "../../src-tauri/tauri.conf.json"), "utf8"),
    );
    expect(conf.app.security.csp["img-src"]).toContain("blob:");
    expect(conf.app.security.devCsp["img-src"]).toContain("blob:");
  });
});
