import type { LiveTurn } from "./chats";
import { apiStream } from "./client";

/** One server-sent event: its name and its `data:` lines joined by a newline. */
export interface SseEvent {
  event: string;
  data: string;
}

/**
 * Cut as many complete frames as `buffer` holds.
 *
 * A frame ends at a blank line. Whatever follows the last blank line is an unfinished frame and
 * comes back as `rest`, to be put in front of the next chunk. Lines starting with `:` are
 * comments (the daemon's keep-alives) and carry nothing.
 */
export function sseFrames(buffer: string): { events: SseEvent[]; rest: string } {
  const text = buffer.replace(/\r\n?/g, "\n");
  const parts = text.split("\n\n");
  const rest = parts.pop() ?? "";
  const events: SseEvent[] = [];
  for (const part of parts) {
    let event = "message";
    const data: string[] = [];
    let named = false;
    for (const line of part.split("\n")) {
      if (line === "" || line.startsWith(":")) continue;
      const colon = line.indexOf(":");
      const field = colon === -1 ? line : line.slice(0, colon);
      let value = colon === -1 ? "" : line.slice(colon + 1);
      if (value.startsWith(" ")) value = value.slice(1);
      if (field === "event") {
        event = value;
        named = true;
      } else if (field === "data") {
        data.push(value);
      }
    }
    if (named || data.length > 0) events.push({ event, data: data.join("\n") });
  }
  return { events, rest };
}

/**
 * Follow a live turn over its stream until it ends.
 *
 * "ended" is the daemon saying the turn is over. Everything else is "failed" — no stream to open
 * (204), no body, a drop before `end`, a refusal, or an aborted signal — and the caller falls back
 * to polling; an abort is the caller's own doing, so it ignores the answer.
 */
export async function streamLiveTurn(
  turnId: number,
  { signal, onLive }: { signal: AbortSignal; onLive: (state: LiveTurn) => void },
): Promise<"ended" | "failed"> {
  try {
    const res = await apiStream(`/assistant/${turnId}/live/stream`, { signal });
    if (res.status === 204 || res.body === null || res.body === undefined) return "failed";
    const reader = res.body.getReader();
    const decoder = new TextDecoder("utf-8");
    let buffer = "";
    try {
      for (;;) {
        const { done, value } = await reader.read();
        if (done) return "failed";
        buffer += decoder.decode(value, { stream: true });
        const { events, rest } = sseFrames(buffer);
        buffer = rest;
        for (const frame of events) {
          if (frame.event === "live") onLive(JSON.parse(frame.data) as LiveTurn);
          else if (frame.event === "end") return "ended";
        }
      }
    } finally {
      reader.cancel().catch(() => {});
    }
  } catch {
    return "failed";
  }
}
