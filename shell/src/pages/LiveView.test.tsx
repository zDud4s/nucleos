// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, createEvent, fireEvent, render, screen } from "@testing-library/react";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

const daemon = vi.hoisted(() => ({ openStream: vi.fn(), apiFetch: vi.fn() }));
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
const json = (value: unknown) => new TextEncoder().encode(JSON.stringify(value));
const meta = (over: Record<string, number> = {}) =>
  record(
    "M",
    json({
      frameWidth: 800,
      frameHeight: 600,
      deviceWidth: 800,
      deviceHeight: 600,
      offsetTop: 0,
      pageScaleFactor: 1,
      scrollOffsetX: 0,
      scrollOffsetY: 0,
      ...over,
    }),
  );
const prompt = (body: Record<string, unknown>) => record("P", json(body));
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
  daemon.apiFetch.mockReset();
  daemon.apiFetch.mockResolvedValue({});
  // The shown image is 400x300 at the origin, for an 800x600 frame.
  vi.spyOn(HTMLElement.prototype, "getBoundingClientRect").mockReturnValue({
    left: 0,
    top: 0,
    right: 400,
    bottom: 300,
    width: 400,
    height: 300,
    x: 0,
    y: 0,
    toJSON: () => ({}),
  });
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
  vi.restoreAllMocks();
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

  /* ------------------------------------------------------- drive mode -- */

  /** Render a view that has received its geometry and first frame. */
  async function drive(nonce: string | null | undefined = "nonce-1") {
    const pipe = openPipe();
    daemon.openStream.mockResolvedValue(pipe.stream);
    render(<LiveView sessionId={7} nonce={nonce} />);
    await settle();
    pipe.send(meta());
    pipe.send(frame(1, 2, 3));
    await settle();
    return { pipe, img: screen.getByAltText("live view of session 7") as HTMLImageElement };
  }

  function inputPosts(): { seat_nonce: string; events: Record<string, unknown>[] }[] {
    return daemon.apiFetch.mock.calls
      .filter((call) => call[0] === "/browser/sessions/7/input")
      .map((call) => JSON.parse((call[1] as { body: string }).body));
  }

  function answerPosts(): { seat_nonce: string; prompt: unknown; answer: unknown }[] {
    return daemon.apiFetch.mock.calls
      .filter((call) => call[0] === "/browser/sessions/7/answer")
      .map((call) => JSON.parse((call[1] as { body: string }).body));
  }

  it("drive mode posts a mapped click to input with the seat nonce", async () => {
    const { img } = await drive();

    // 400x300 shown for an 800x600 frame: the point (100, 50) is page (200, 100).
    fireEvent.mouseDown(img, { clientX: 100, clientY: 50, button: 0, buttons: 1, detail: 1 });
    await settle();
    fireEvent.mouseUp(img, { clientX: 100, clientY: 50, button: 0, buttons: 0, detail: 1 });
    await settle();

    const posts = inputPosts();
    expect(posts.length).toBe(2);
    expect(posts[0].seat_nonce).toBe("nonce-1");
    expect(posts[0].events).toEqual([
      expect.objectContaining({ kind: "mouse", type: "mousePressed", x: 200, y: 100, button: "left" }),
    ]);
    expect(posts[1].events).toEqual([
      expect.objectContaining({ kind: "mouse", type: "mouseReleased", x: 200, y: 100, button: "left" }),
    ]);
  });

  it("drive mode batches mouse moves into one post per 30 ms", async () => {
    const { img } = await drive();

    fireEvent.mouseMove(img, { clientX: 10, clientY: 10 });
    fireEvent.mouseMove(img, { clientX: 20, clientY: 20 });
    fireEvent.mouseMove(img, { clientX: 40, clientY: 30 });
    expect(inputPosts().length).toBe(0);

    await settle(29);
    expect(inputPosts().length).toBe(0);
    await settle(1);

    const posts = inputPosts();
    expect(posts.length).toBe(1);
    expect(posts[0].events).toEqual([expect.objectContaining({ kind: "mouse", type: "mouseMoved", x: 80, y: 60 })]);
  });

  it("keys reach input only while the view is focused and paste and compositionend send text", async () => {
    await drive();
    const wrapper = screen.getByLabelText("drive session 7");
    expect(wrapper.getAttribute("tabindex")).toBe("0");

    // Not focused: a key aimed at the page body goes nowhere, and neither does
    // one fired on the wrapper itself while it does not hold focus.
    fireEvent.keyDown(document.body, { key: "a", code: "KeyA", keyCode: 65 });
    fireEvent.keyDown(wrapper, { key: "a", code: "KeyA", keyCode: 65 });
    await settle();
    expect(inputPosts().length).toBe(0);

    wrapper.focus();
    fireEvent.keyDown(wrapper, { key: "a", code: "KeyA", keyCode: 65 });
    await settle();
    fireEvent.keyUp(wrapper, { key: "a", code: "KeyA", keyCode: 65 });
    await settle();
    expect(inputPosts().map((post) => post.events)).toEqual([
      [{ kind: "key", type: "keyDown", key: "a", code: "KeyA", keyCode: 65, text: "a", modifiers: 0 }],
      [{ kind: "key", type: "keyUp", key: "a", code: "KeyA", keyCode: 65, text: "", modifiers: 0 }],
    ]);

    // An IME composition in flight, and the paste shortcut, are not keys.
    daemon.apiFetch.mockClear();
    fireEvent.keyDown(wrapper, { key: "Process", code: "KeyK", keyCode: 229 });
    fireEvent.keyDown(wrapper, { key: "v", code: "KeyV", ctrlKey: true });
    await settle();
    expect(inputPosts().length).toBe(0);

    fireEvent.paste(wrapper, { clipboardData: { getData: () => "pasted text" } });
    await settle();
    fireEvent.compositionEnd(wrapper, { data: "é" });
    await settle();
    expect(inputPosts().map((post) => post.events)).toEqual([
      [{ kind: "text", value: "pasted text" }],
      [{ kind: "text", value: "é" }],
    ]);
  });

  it("a P record opens a prompt whose answer goes to answer and a resolved P closes it", async () => {
    const { pipe } = await drive();

    pipe.send(prompt({ id: "5", kind: "dialog", dialogType: "confirm", message: "Delete it?", defaultPrompt: "" }));
    await settle();
    expect(screen.getByText("Delete it?")).toBeDefined();

    fireEvent.click(screen.getByRole("button", { name: "OK" }));
    await settle();
    const answers = answerPosts();
    expect(answers.length).toBe(1);
    expect(answers[0].seat_nonce).toBe("nonce-1");
    expect(String(answers[0].prompt)).toBe("5");
    expect(answers[0].answer).toEqual({ accept: true, text: "" });
    expect(screen.queryByText("Delete it?")).toBeNull();

    // A prompt the page itself dismissed disappears without an answer.
    pipe.send(prompt({ id: "6", kind: "dialog", dialogType: "alert", message: "Heads up", defaultPrompt: "" }));
    await settle();
    expect(screen.getByText("Heads up")).toBeDefined();
    pipe.send(prompt({ id: "6", kind: "dialog", resolved: true }));
    await settle();
    expect(screen.queryByText("Heads up")).toBeNull();
    expect(answerPosts().length).toBe(1);
  });

  it("watch mode sends no input and ignores P", async () => {
    const { pipe, img } = await drive(null);

    expect(screen.queryByLabelText("drive session 7")).toBeNull();

    fireEvent.mouseDown(img, { clientX: 100, clientY: 50, button: 0, buttons: 1, detail: 1 });
    fireEvent.mouseMove(img, { clientX: 120, clientY: 60 });
    fireEvent.keyDown(img, { key: "a", code: "KeyA" });
    pipe.send(prompt({ id: "5", kind: "dialog", dialogType: "confirm", message: "Delete it?", defaultPrompt: "" }));
    await settle(100);

    expect(daemon.apiFetch).not.toHaveBeenCalled();
    expect(screen.queryByText("Delete it?")).toBeNull();
    // The view itself keeps showing frames.
    expect(img.getAttribute("src")).toBe("blob:live-1");
  });

  it("a 409 from input drops back to watching with a sentence", async () => {
    const { img } = await drive();
    daemon.apiFetch.mockRejectedValue(new ApiRefusal(409, "conflict", "not the seat holder"));

    fireEvent.mouseDown(img, { clientX: 100, clientY: 50, button: 0, buttons: 1, detail: 1 });
    await settle();

    expect(screen.getByText("The wheel is no longer yours — watching only.")).toBeDefined();
    expect(screen.queryByLabelText("drive session 7")).toBeNull();
    expect(screen.getByAltText("live view of session 7")).toBeDefined();

    // Watching only: further input goes nowhere.
    daemon.apiFetch.mockClear();
    fireEvent.mouseDown(screen.getByAltText("live view of session 7"), { clientX: 100, clientY: 50, button: 0, buttons: 1 });
    await settle(100);
    expect(daemon.apiFetch).not.toHaveBeenCalled();
  });

  it("Tab and Shift+Tab are neither sent nor default-prevented, so focus can leave", async () => {
    await drive();
    const wrapper = screen.getByLabelText("drive session 7");
    wrapper.focus();

    const events = [
      createEvent.keyDown(wrapper, { key: "Tab", code: "Tab", keyCode: 9 }),
      createEvent.keyDown(wrapper, { key: "Tab", code: "Tab", keyCode: 9, shiftKey: true }),
      createEvent.keyUp(wrapper, { key: "Tab", code: "Tab", keyCode: 9 }),
    ];
    for (const event of events) fireEvent(wrapper, event);
    await settle();

    for (const event of events) expect(event.defaultPrevented).toBe(false);
    expect(inputPosts().length).toBe(0);
  });

  it("any failed answer shows a sentence, and a 413 names the size", async () => {
    const { pipe } = await drive();
    pipe.send(prompt({ id: "5", kind: "dialog", dialogType: "confirm", message: "Delete it?", defaultPrompt: "" }));
    await settle();

    daemon.apiFetch.mockRejectedValue(new ApiRefusal(400, "bad_request", "malformed answer"));
    fireEvent.click(screen.getByRole("button", { name: "OK" }));
    await settle();
    expect(screen.getByText("The answer could not be sent.")).toBeDefined();
    // The prompt stays open so it can be tried again.
    expect(screen.getByText("Delete it?")).toBeDefined();

    daemon.apiFetch.mockRejectedValue(new ApiRefusal(413, "too_large", "body too large"));
    fireEvent.click(screen.getByRole("button", { name: "OK" }));
    await settle();
    expect(screen.getByText("Those files are too large together.")).toBeDefined();
  });

  it("the prompt submit is disabled while an answer is in flight", async () => {
    const { pipe } = await drive();
    pipe.send(prompt({ id: "5", kind: "dialog", dialogType: "confirm", message: "Delete it?", defaultPrompt: "" }));
    await settle();

    let release!: () => void;
    daemon.apiFetch.mockReturnValue(new Promise<unknown>((resolve) => (release = () => resolve({}))));
    fireEvent.click(screen.getByRole("button", { name: "OK" }));
    await settle();
    expect((screen.getByRole("button", { name: "OK" }) as HTMLButtonElement).disabled).toBe(true);

    release();
    await settle();
    expect(screen.queryByText("Delete it?")).toBeNull();
  });

  it("both CSPs admit blob images", () => {
    const conf = JSON.parse(
      readFileSync(join(dirname(fileURLToPath(import.meta.url)), "../../src-tauri/tauri.conf.json"), "utf8"),
    );
    expect(conf.app.security.csp["img-src"]).toContain("blob:");
    expect(conf.app.security.devCsp["img-src"]).toContain("blob:");
  });
});
