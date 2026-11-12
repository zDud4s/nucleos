import type { FrameMeta, InputEvent } from "./liveRecords";

interface Rect {
  left: number;
  top: number;
  width: number;
  height: number;
}

/**
 * Maps a client point on the shown image to page CSS pixels through the last
 * `M` record. Formula, pinned by tests:
 *   fx = (clientX - rect.left) * frameWidth / rect.width
 *   fy = (clientY - rect.top) * frameHeight / rect.height
 *   x  = fx * (deviceWidth / frameWidth) / pageScaleFactor
 *   y  = (fy * (deviceHeight / frameHeight) - offsetTop) / pageScaleFactor
 * Null when there is no meta yet, the rect has no size, the point is outside
 * the rect, or it lands above the page top (y < 0).
 */
export function toPage(
  clientX: number,
  clientY: number,
  rect: Rect,
  meta: FrameMeta | null,
): { x: number; y: number } | null {
  if (!meta || rect.width <= 0 || rect.height <= 0) return null;
  if (meta.frameWidth <= 0 || meta.frameHeight <= 0 || meta.pageScaleFactor <= 0) return null;
  if (clientX < rect.left || clientX > rect.left + rect.width) return null;
  if (clientY < rect.top || clientY > rect.top + rect.height) return null;
  const fx = ((clientX - rect.left) * meta.frameWidth) / rect.width;
  const fy = ((clientY - rect.top) * meta.frameHeight) / rect.height;
  const x = (fx * (meta.deviceWidth / meta.frameWidth)) / meta.pageScaleFactor;
  const y = (fy * (meta.deviceHeight / meta.frameHeight) - meta.offsetTop) / meta.pageScaleFactor;
  if (y < 0) return null;
  return { x, y };
}

interface ModifierKeys {
  altKey: boolean;
  ctrlKey: boolean;
  metaKey: boolean;
  shiftKey: boolean;
}

/** CDP modifier bitmask: Alt=1, Ctrl=2, Meta=4, Shift=8. */
export function modifiers(e: ModifierKeys): number {
  return (e.altKey ? 1 : 0) | (e.ctrlKey ? 2 : 0) | (e.metaKey ? 4 : 0) | (e.shiftKey ? 8 : 0);
}

export function mouseButton(n: number): "none" | "left" | "middle" | "right" {
  if (n === 0) return "left";
  if (n === 1) return "middle";
  if (n === 2) return "right";
  return "none";
}

/** True unless `key` is exactly the base character that `code` (KeyX / DigitN) names. */
function differsFromBase(key: string, code: string): boolean {
  const letter = /^Key([A-Z])$/.exec(code);
  if (letter) return key.toLowerCase() !== letter[1].toLowerCase();
  const digit = /^Digit([0-9])$/.exec(code);
  if (digit) return key !== digit[1];
  return true;
}

export function keyEvent(
  type: "keyDown" | "keyUp",
  e: ModifierKeys & { key: string; code: string; keyCode: number; getModifierState?: (key: "AltGraph") => boolean },
): Extract<InputEvent, { kind: "key" }> {
  // Windows reports AltGr as Ctrl+Alt (plus the AltGraph state): `@ { [ ] }` on a PT layout are
  // typed text, not a shortcut, so they carry the character and neither modifier bit.
  // Without the AltGraph state, Ctrl+Alt counts as AltGr only when the key differs from the base
  // character its physical code names; Ctrl+Alt+M still reports "m" and stays a shortcut.
  const altGr =
    e.key.length === 1 &&
    !e.metaKey &&
    (e.getModifierState?.("AltGraph") === true || (e.ctrlKey && e.altKey && differsFromBase(e.key, e.code)));
  const printable = type === "keyDown" && e.key.length === 1 && (altGr || (!e.ctrlKey && !e.metaKey));
  // Enter inserts a line break only when its keyDown carries "\r", as puppeteer sends it.
  const text = printable ? e.key : type === "keyDown" && e.key === "Enter" ? "\r" : "";
  return {
    kind: "key",
    type,
    key: e.key,
    code: e.code,
    keyCode: e.keyCode,
    text,
    modifiers: modifiers(altGr ? { ...e, ctrlKey: false, altKey: false } : e),
  };
}

const TEXT_CHUNK = 16000;

interface BatcherOptions {
  send: (events: InputEvent[]) => Promise<unknown>;
  onError?: (error: unknown) => void;
  windowMs?: number;
}

/**
 * Collects input events and posts them in order, one batch at a time. Moves
 * and wheel events wait up to `windowMs`; consecutive moves collapse to the
 * last one. Anything else flushes the queue and goes out at once.
 */
export class InputBatcher {
  private queue: InputEvent[] = [];
  private timer: ReturnType<typeof setTimeout> | null = null;
  private chain: Promise<void> = Promise.resolve();
  private disposed = false;
  private readonly windowMs: number;

  constructor(private readonly options: BatcherOptions) {
    this.windowMs = options.windowMs ?? 30;
  }

  push(event: InputEvent): void {
    if (this.disposed) return;
    if (event.kind === "wheel" || (event.kind === "mouse" && event.type === "mouseMoved")) {
      const last = this.queue[this.queue.length - 1];
      if (event.kind === "mouse" && last && last.kind === "mouse" && last.type === "mouseMoved") {
        this.queue[this.queue.length - 1] = event;
      } else {
        this.queue.push(event);
      }
      if (this.timer === null) {
        this.timer = setTimeout(() => {
          this.timer = null;
          this.flush();
        }, this.windowMs);
      }
      return;
    }
    if (event.kind === "text" && event.value.length > TEXT_CHUNK) {
      this.flush();
      let i = 0;
      while (i < event.value.length) {
        let end = Math.min(i + TEXT_CHUNK, event.value.length);
        // Never end a chunk on a high surrogate: that would split the pair.
        const last = event.value.charCodeAt(end - 1);
        if (end < event.value.length && last >= 0xd800 && last <= 0xdbff) end -= 1;
        this.enqueue([{ kind: "text", value: event.value.slice(i, end) }]);
        i = end;
      }
      return;
    }
    this.queue.push(event);
    this.flush();
  }

  flush(): void {
    if (this.timer !== null) {
      clearTimeout(this.timer);
      this.timer = null;
    }
    if (this.queue.length === 0) return;
    const batch = this.queue;
    this.queue = [];
    this.enqueue(batch);
  }

  dispose(): void {
    this.disposed = true;
    if (this.timer !== null) {
      clearTimeout(this.timer);
      this.timer = null;
    }
    this.queue = [];
  }

  private enqueue(batch: InputEvent[]): void {
    this.chain = this.chain
      .then(() => this.options.send(batch))
      .then(
        () => undefined,
        (error: unknown) => {
          this.options.onError?.(error);
        },
      );
  }
}
