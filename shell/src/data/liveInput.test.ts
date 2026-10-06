import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { FrameMeta, InputEvent } from "./liveRecords";
import { InputBatcher, keyEvent, modifiers, mouseButton, toPage } from "./liveInput";

const meta = (over: Partial<FrameMeta> = {}): FrameMeta =>
  ({
    frameWidth: 1280,
    frameHeight: 720,
    deviceWidth: 1920,
    deviceHeight: 1080,
    offsetTop: 0,
    pageScaleFactor: 1,
    ...over,
  }) as FrameMeta;

const rect = { left: 10, top: 20, width: 640, height: 360 };

describe("toPage", () => {
  it("maps a point on the shown image to page css pixels through M", () => {
    expect(toPage(330, 200, rect, meta())).toEqual({ x: 960, y: 540 });
  });

  it("subtracts offsetTop and divides by pageScaleFactor", () => {
    expect(toPage(330, 200, rect, meta({ offsetTop: 20, pageScaleFactor: 2 }))).toEqual({
      x: 480,
      y: 260,
    });
  });

  it("a point outside the page or before any M maps to null", () => {
    expect(toPage(330, 200, rect, null)).toBeNull();
    expect(toPage(330, 200, { ...rect, width: 0 }, meta())).toBeNull();
    expect(toPage(5, 200, rect, meta())).toBeNull();
    expect(toPage(700, 200, rect, meta())).toBeNull();
    expect(toPage(330, 10, rect, meta())).toBeNull();
    expect(toPage(330, 400, rect, meta())).toBeNull();
    // Above the page top once offsetTop is subtracted.
    expect(toPage(330, 20, rect, meta({ offsetTop: 50 }))).toBeNull();
  });
});

describe("InputBatcher", () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });
  afterEach(() => {
    vi.useRealTimers();
  });

  const move = (x: number): InputEvent => ({
      kind: "mouse",
      type: "mouseMoved",
      x,
      y: 1,
      button: "none",
      buttons: 0,
      clickCount: 0,
      modifiers: 0,
    }) as InputEvent;
  const press = (): InputEvent =>
    ({
      kind: "mouse",
      type: "mousePressed",
      x: 5,
      y: 6,
      button: "left",
      buttons: 1,
      clickCount: 1,
      modifiers: 0,
    }) as InputEvent;

  it("batches moves for 30 ms keeping the last and flushes before a press", async () => {
    const send = vi.fn().mockResolvedValue(undefined);
    const b = new InputBatcher({ send });
    b.push(move(1));
    b.push(move(2));
    b.push(move(3));
    expect(send).not.toHaveBeenCalled();
    await vi.advanceTimersByTimeAsync(30);
    expect(send).toHaveBeenCalledTimes(1);
    expect(send.mock.calls[0][0]).toEqual([move(3)]);

    b.push(move(4));
    b.push(press());
    await vi.advanceTimersByTimeAsync(0);
    expect(send).toHaveBeenCalledTimes(2);
    expect(send.mock.calls[1][0]).toEqual([move(4), press()]);
    // The pending timer was consumed by the flush: nothing more goes out.
    await vi.advanceTimersByTimeAsync(100);
    expect(send).toHaveBeenCalledTimes(2);
    b.dispose();
  });

  it("sends batches one at a time in order and splits long text", async () => {
    const resolvers: Array<() => void> = [];
    const send = vi.fn().mockImplementation(
      () => new Promise<void>((resolve) => resolvers.push(resolve)),
    );
    const onError = vi.fn();
    const b = new InputBatcher({ send, onError });

    b.push(press());
    b.push({
      kind: "mouse",
      type: "mouseReleased",
      x: 5,
      y: 6,
      button: "left",
      buttons: 0,
      clickCount: 1,
      modifiers: 0,
    } as InputEvent);
    await vi.advanceTimersByTimeAsync(0);
    // The first batch is still in flight, so the second waits.
    expect(send).toHaveBeenCalledTimes(1);
    resolvers[0]();
    await vi.advanceTimersByTimeAsync(0);
    expect(send).toHaveBeenCalledTimes(2);
    expect(send.mock.calls[0][0][0].type).toBe("mousePressed");
    expect(send.mock.calls[1][0][0].type).toBe("mouseReleased");
    resolvers[1]();
    await vi.advanceTimersByTimeAsync(0);

    const long = "a".repeat(16000) + "b".repeat(16000) + "c".repeat(5);
    b.push({ kind: "text", value: long } as InputEvent);
    for (let i = 0; i < 5; i++) {
      await vi.advanceTimersByTimeAsync(0);
      resolvers[resolvers.length - 1]?.();
    }
    await vi.advanceTimersByTimeAsync(0);
    const texts = send.mock.calls.slice(2).map((c) => c[0]);
    expect(texts).toHaveLength(3);
    expect(texts.every((batch) => batch.length === 1 && batch[0].kind === "text")).toBe(true);
    expect(texts.map((batch) => batch[0].value.length)).toEqual([16000, 16000, 5]);

    send.mockRejectedValueOnce(new Error("boom"));
    b.push(press());
    await vi.advanceTimersByTimeAsync(0);
    expect(onError).toHaveBeenCalledTimes(1);
    b.dispose();
  });
});

describe("modifiers, mouseButton and keyEvent", () => {
  it("translates keys with cdp modifiers and text only for printable keys", () => {
    const none = { altKey: false, ctrlKey: false, metaKey: false, shiftKey: false };
    expect(modifiers(none)).toBe(0);
    expect(modifiers({ ...none, altKey: true })).toBe(1);
    expect(modifiers({ ...none, ctrlKey: true })).toBe(2);
    expect(modifiers({ ...none, metaKey: true })).toBe(4);
    expect(modifiers({ ...none, shiftKey: true })).toBe(8);
    expect(modifiers({ altKey: true, ctrlKey: true, metaKey: true, shiftKey: true })).toBe(15);

    expect(mouseButton(0)).toBe("left");
    expect(mouseButton(1)).toBe("middle");
    expect(mouseButton(2)).toBe("right");
    expect(mouseButton(7)).toBe("none");

    const a = keyEvent("keyDown", { key: "a", code: "KeyA", keyCode: 65, ...none });
    expect(a).toMatchObject({ kind: "key", type: "keyDown", key: "a", code: "KeyA" });
    expect(a.text).toBe("a");
    expect(keyEvent("keyUp", { key: "a", code: "KeyA", keyCode: 65, ...none }).text).toBe("");
    expect(keyEvent("keyDown", { key: "Enter", code: "Enter", keyCode: 13, ...none }).text).toBe("\r");
    expect(keyEvent("keyDown", { key: "a", code: "KeyA", keyCode: 65, ...none, ctrlKey: true }).text).toBe("");
    expect(keyEvent("keyDown", { key: "a", code: "KeyA", keyCode: 65, ...none, metaKey: true }).text).toBe("");
    const shifted = keyEvent("keyDown", { key: "A", code: "KeyA", keyCode: 65, ...none, shiftKey: true });
    expect(shifted.text).toBe("A");
    expect(shifted.modifiers).toBe(8);
  });
});

describe("keyEvent and AltGr", () => {
  const none = { altKey: false, ctrlKey: false, metaKey: false, shiftKey: false };
  it("sends the text of an AltGr character with no ctrl or alt modifiers", () => {
    const at = keyEvent("keyDown", { key: "@", code: "Digit2", keyCode: 50, ...none, ctrlKey: true, altKey: true });
    expect(at.text).toBe("@");
    expect(at.modifiers & 3).toBe(0);
    const viaState = keyEvent("keyDown", {
      key: "{",
      code: "Digit7",
      keyCode: 55,
      ...none,
      getModifierState: (name: string) => name === "AltGraph",
    });
    expect(viaState.text).toBe("{");
    expect(viaState.modifiers & 3).toBe(0);
  });
  it("still sends no text for a plain Ctrl shortcut", () => {
    expect(keyEvent("keyDown", { key: "a", code: "KeyA", keyCode: 65, ...none, ctrlKey: true }).text).toBe("");
    expect(keyEvent("keyDown", { key: "a", code: "KeyA", keyCode: 65, ...none, ctrlKey: true, altKey: true, metaKey: true }).text).toBe("");
  });
});

describe("keyEvent and a real Ctrl+Alt shortcut", () => {
  const none = { altKey: false, ctrlKey: false, metaKey: false, shiftKey: false };
  it("keeps Ctrl+Alt+letter and Ctrl+Alt+digit as shortcuts with no text", () => {
    const m = keyEvent("keyDown", { key: "m", code: "KeyM", keyCode: 77, ...none, ctrlKey: true, altKey: true });
    expect(m.text).toBe("");
    expect(m.modifiers).toBe(3);
    const one = keyEvent("keyDown", { key: "1", code: "Digit1", keyCode: 49, ...none, ctrlKey: true, altKey: true });
    expect(one.text).toBe("");
    expect(one.modifiers).toBe(3);
  });
});

describe("keyEvent carries the virtual key code", () => {
  const none = { altKey: false, ctrlKey: false, metaKey: false, shiftKey: false };
  it("sends keyCode 8 and no text for Backspace", () => {
    expect(keyEvent("keyDown", { key: "Backspace", code: "Backspace", keyCode: 8, ...none })).toMatchObject({
      keyCode: 8,
      text: "",
    });
  });
  it("sends keyCode 13 and a carriage return for Enter on keyDown", () => {
    expect(keyEvent("keyDown", { key: "Enter", code: "Enter", keyCode: 13, ...none })).toMatchObject({
      keyCode: 13,
      text: "\r",
    });
    expect(keyEvent("keyUp", { key: "Enter", code: "Enter", keyCode: 13, ...none }).text).toBe("");
  });
  it("sends keyCode 65 and the character for a", () => {
    expect(keyEvent("keyDown", { key: "a", code: "KeyA", keyCode: 65, ...none })).toMatchObject({
      keyCode: 65,
      text: "a",
    });
  });
});

describe("InputBatcher text splitting", () => {
  it("does not split a surrogate pair at the chunk boundary", async () => {
    const sent: InputEvent[][] = [];
    const b = new InputBatcher({ send: async (events) => void sent.push(events) });
    const value = "a".repeat(15999) + "\u{1F600}" + "b".repeat(10);
    b.push({ kind: "text", value });
    await new Promise((r) => setTimeout(r, 0));
    const parts = sent.flat().map((e) => (e.kind === "text" ? e.value : ""));
    expect(parts.join("")).toBe(value);
    for (const part of parts) {
      const last = part.charCodeAt(part.length - 1);
      expect(last >= 0xd800 && last <= 0xdbff).toBe(false);
      const first = part.charCodeAt(0);
      expect(first >= 0xdc00 && first <= 0xdfff).toBe(false);
    }
    b.dispose();
  });
});
