/**
 * The live view's wire format: a stream of records, each a one-byte type, a
 * u32 big-endian body length, then the body. `F` carries one JPEG frame; `E`
 * carries `{"reason": ...}` and is the last record of a stream.
 */

export type LiveEnd = "wheel" | "closed" | "gone";

export type LiveRecord = { kind: "frame"; jpeg: Uint8Array } | { kind: "end"; reason: LiveEnd };

const FRAME = "F".charCodeAt(0);
const END = "E".charCodeAt(0);
const HEADER = 5;

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
      else out.push({ kind: "end", reason: "gone" });
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
