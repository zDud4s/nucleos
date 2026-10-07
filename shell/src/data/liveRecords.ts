/**
 * The live view's wire format: a stream of records, each a one-byte type, a
 * u32 big-endian body length, then the body. `F` carries one JPEG frame; `M`
 * carries the frame's metadata (JSON, see `FrameMeta`); `P` carries a prompt
 * the page opened, or `{id, kind, resolved: true}` once it is gone; `E`
 * carries `{"reason": ...}` and is the last record of a stream.
 *
 * This file is also the shell's wire-types file for the way back: `InputEvent`
 * and `PromptAnswer` are what the shell posts to the core's input/answer routes.
 */

export type LiveEnd = "wheel" | "closed" | "gone";

/** Geometry of the last frame, in the terms CDP screencast reports it. */
export interface FrameMeta {
  frameWidth: number;
  frameHeight: number;
  deviceWidth: number;
  deviceHeight: number;
  offsetTop: number;
  pageScaleFactor: number;
  scrollOffsetX: number;
  scrollOffsetY: number;
}

export type OpenPrompt =
  | { id: string; kind: "dialog"; dialogType: "alert" | "confirm" | "prompt" | "beforeunload"; message: string; defaultPrompt: string }
  | { id: string; kind: "select"; options: { value: string; label: string; selected: boolean }[]; multiple: boolean }
  | { id: string; kind: "file"; multiple: boolean; accept: string }
  | { id: string; kind: "auth"; origin: string; realm: string };

export type PromptRecord = OpenPrompt | { id: string; kind: OpenPrompt["kind"]; resolved: true };

export type InputEvent =
  | {
      kind: "mouse";
      type: "mousePressed" | "mouseReleased" | "mouseMoved";
      x: number;
      y: number;
      button: "none" | "left" | "middle" | "right";
      buttons: number;
      clickCount: number;
      modifiers: number;
    }
  | { kind: "wheel"; x: number; y: number; dx: number; dy: number }
  | { kind: "key"; type: "keyDown" | "keyUp" | "char"; key: string; code: string; keyCode: number; text: string; modifiers: number }
  | { kind: "text"; value: string };

export type PromptAnswer =
  | { accept: boolean; text: string }
  | { value: string }
  | { files: { name: string; mime: string; data_b64: string }[] }
  | { username: string; password: string }
  | { cancel: true };

export type LiveRecord =
  | { kind: "frame"; jpeg: Uint8Array }
  | { kind: "meta"; meta: FrameMeta }
  | { kind: "prompt"; prompt: PromptRecord }
  | { kind: "end"; reason: LiveEnd };

const FRAME = "F".charCodeAt(0);
const END = "E".charCodeAt(0);
const META = "M".charCodeAt(0);
const PROMPT = "P".charCodeAt(0);
const HEADER = 5;

const META_KEYS: (keyof FrameMeta)[] = [
  "frameWidth",
  "frameHeight",
  "deviceWidth",
  "deviceHeight",
  "offsetTop",
  "pageScaleFactor",
  "scrollOffsetX",
  "scrollOffsetY",
];

function parseJson(body: Uint8Array): Record<string, unknown> | null {
  try {
    const parsed: unknown = JSON.parse(new TextDecoder().decode(body));
    if (typeof parsed === "object" && parsed !== null && !Array.isArray(parsed)) return parsed as Record<string, unknown>;
  } catch {
    // Not JSON: the caller treats null as a stream that is not what we think.
  }
  return null;
}

function parseMeta(body: Uint8Array): FrameMeta | null {
  const raw = parseJson(body);
  if (!raw) return null;
  const meta = {} as FrameMeta;
  for (const key of META_KEYS) {
    const value = raw[key];
    if (typeof value !== "number" || !Number.isFinite(value)) return null;
    meta[key] = value;
  }
  return meta;
}

const str = (value: unknown): string => (typeof value === "string" ? value : "");
const bool = (value: unknown): boolean => value === true;
const DIALOG_TYPES = ["alert", "confirm", "prompt", "beforeunload"] as const;
const KINDS = ["dialog", "select", "file", "auth"] as const;

function parsePrompt(body: Uint8Array): PromptRecord | null {
  const raw = parseJson(body);
  if (!raw || typeof raw.id !== "string") return null;
  const id = raw.id;
  const kind = KINDS.find((one) => one === raw.kind);
  if (!kind) return null;
  if (raw.resolved === true) return { id, kind, resolved: true };

  switch (kind) {
    case "dialog": {
      const dialogType = DIALOG_TYPES.find((one) => one === raw.dialogType) ?? "alert";
      return { id, kind, dialogType, message: str(raw.message), defaultPrompt: str(raw.defaultPrompt) };
    }
    case "select": {
      if (!Array.isArray(raw.options)) return null;
      const options: { value: string; label: string; selected: boolean }[] = [];
      for (const item of raw.options) {
        if (typeof item !== "object" || item === null) return null;
        const option = item as Record<string, unknown>;
        if (typeof option.value !== "string" || typeof option.label !== "string") return null;
        options.push({ value: option.value, label: option.label, selected: bool(option.selected) });
      }
      return { id, kind, options, multiple: bool(raw.multiple) };
    }
    case "file":
      return { id, kind, multiple: bool(raw.multiple), accept: str(raw.accept) };
    case "auth":
      return { id, kind, origin: str(raw.origin), realm: str(raw.realm) };
  }
}

function endReason(body: Uint8Array): LiveEnd {
  try {
    const parsed: unknown = JSON.parse(new TextDecoder().decode(body));
    if (typeof parsed === "object" && parsed !== null) {
      const reason = (parsed as Record<string, unknown>).reason;
      if (reason === "wheel" || reason === "closed" || reason === "gone") return reason;
    }
  } catch {
    // Unparseable: the stream is not what we think it is, which reads as gone.
  }
  return "gone";
}

export class RecordParser {
  private buffer = new Uint8Array(0);

  /** Feed bytes; get back every record they completed. Partial bytes are kept. */
  push(chunk: Uint8Array): LiveRecord[] {
    if (chunk.length > 0) {
      const joined = new Uint8Array(this.buffer.length + chunk.length);
      joined.set(this.buffer, 0);
      joined.set(chunk, this.buffer.length);
      this.buffer = joined;
    }

    const out: LiveRecord[] = [];
    while (this.buffer.length >= HEADER) {
      const type = this.buffer[0];
      const length = new DataView(this.buffer.buffer, this.buffer.byteOffset, this.buffer.byteLength).getUint32(1, false);
      if (this.buffer.length < HEADER + length) break;
      const body = this.buffer.slice(HEADER, HEADER + length);
      this.buffer = this.buffer.slice(HEADER + length);

      if (type === FRAME) out.push({ kind: "frame", jpeg: body });
      else if (type === END) out.push({ kind: "end", reason: endReason(body) });
      else if (type === META) {
        const meta = parseMeta(body);
        out.push(meta ? { kind: "meta", meta } : { kind: "end", reason: "gone" });
      } else if (type === PROMPT) {
        const prompt = parsePrompt(body);
        out.push(prompt ? { kind: "prompt", prompt } : { kind: "end", reason: "gone" });
      } else out.push({ kind: "end", reason: "gone" });
    }
    return out;
  }
}

export async function* readRecords(stream: ReadableStream<Uint8Array>): AsyncGenerator<LiveRecord> {
  const reader = stream.getReader();
  const parser = new RecordParser();
  try {
    for (;;) {
      const { done, value } = await reader.read();
      if (done) return;
      for (const record of parser.push(value)) yield record;
    }
  } finally {
    reader.cancel().catch(() => undefined);
  }
}

/** Reconnect delay in ms for the nth consecutive failure (0-based), capped at 30s. */
export function backoffDelay(attempt: number): number {
  return Math.min(1000 * 2 ** attempt, 30_000);
}
