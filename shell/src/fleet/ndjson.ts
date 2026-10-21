import type { FleetEvent } from "./types";

const KNOWN = new Set(["snapshot", "node", "metrics", "lines", "removed", "heartbeat"]);

/**
 * Cuts a byte stream into complete NDJSON lines.
 *
 * One decoder in streaming mode, so a character split across two network chunks is not turned into
 * U+FFFD; the unterminated tail is held until its `\n` arrives.
 */
export class NdjsonSplitter {
  private readonly decoder = new TextDecoder("utf-8");
  private rest = "";

  push(chunk: Uint8Array): string[] {
    this.rest += this.decoder.decode(chunk, { stream: true });
    const parts = this.rest.split("\n");
    this.rest = parts.pop() ?? "";
    return parts.map((part) => part.trim()).filter((part) => part !== "");
  }
}

/** One line as an event, or `null` for anything this shell does not understand (forward compatible). */
export function parseEvent(line: string): FleetEvent | null {
  let value: unknown;
  try {
    value = JSON.parse(line);
  } catch {
    return null;
  }
  if (typeof value !== "object" || value === null) return null;
  const t = (value as { t?: unknown }).t;
  return typeof t === "string" && KNOWN.has(t) ? (value as FleetEvent) : null;
}
