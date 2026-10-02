/**
 * A run's live output, read as events rather than as the wire it arrives on.
 *
 * The runner speaks `--output-format stream-json`: one JSON object per line, each one a whole
 * envelope — session ids, uuids, the tool result printed twice (`content` and `tool_use_result`).
 * Shown verbatim that is a wall of braces where the one sentence somebody opened the page for is
 * buried in the middle of a line. This keeps what a person reads — what the agent said, which tool
 * it called on what, the head of what came back, how it ended — and drops the envelope.
 *
 * Pure, no React: `run-stream.test.ts` asserts every rule here without mounting anything. Nothing
 * is lost by it — the page keeps the raw text one click away, and a line this does not recognise
 * is shown as it came rather than guessed at.
 */

export type RunEvent =
  | { kind: "said"; text: string }
  | { kind: "thought"; text: string }
  | { kind: "tool"; name: string; detail: string }
  | { kind: "result"; text: string; more: number; error: boolean }
  | { kind: "meta"; text: string }
  | { kind: "done"; text: string; error: boolean }
  | { kind: "raw"; text: string };

/** How many lines of a tool's answer are shown before the rest is a count. */
export const RESULT_LINES = 8;

/** The input field that says what a tool call was about, in the order they are looked for. */
const TELLING_FIELDS = [
  "command",
  "file_path",
  "path",
  "pattern",
  "url",
  "query",
  "description",
  "prompt",
  "skill",
];

type Json = Record<string, unknown>;

function isObject(value: unknown): value is Json {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function str(value: unknown): string | undefined {
  return typeof value === "string" ? value : undefined;
}

/** One line of a tool's input: its telling field, or the whole input squeezed onto a line. */
function toolDetail(input: unknown): string {
  if (!isObject(input)) return "";
  for (const field of TELLING_FIELDS) {
    const value = str(input[field]);
    if (value !== undefined && value.trim() !== "") return value.trim();
  }
  const compact = JSON.stringify(input);
  return compact === "{}" ? "" : compact;
}

/** A tool result's text, whether it came as a string or as a list of content blocks. */
function resultText(content: unknown): string {
  if (typeof content === "string") return content;
  if (Array.isArray(content)) {
    return content
      .map((block) => (isObject(block) ? (str(block.text) ?? (block.type === "image" ? "[image]" : "")) : ""))
      .filter((text) => text !== "")
      .join("\n");
  }
  return "";
}

function headOf(text: string): { text: string; more: number } {
  const lines = text.replace(/\s+$/, "").split("\n");
  if (lines.length <= RESULT_LINES) return { text: lines.join("\n"), more: 0 };
  return { text: lines.slice(0, RESULT_LINES).join("\n"), more: lines.length - RESULT_LINES };
}

function money(value: unknown): string | undefined {
  return typeof value === "number" ? `$${value.toFixed(2)}` : undefined;
}

/** The events one parsed line stands for — none for an envelope that carries nothing to read. */
function eventsOf(line: Json): RunEvent[] {
  const type = str(line.type);
  const message = isObject(line.message) ? line.message : undefined;
  const content = Array.isArray(message?.content) ? message.content : [];

  if (type === "assistant") {
    return content.flatMap((block): RunEvent[] => {
      if (!isObject(block)) return [];
      if (block.type === "text") {
        const text = str(block.text)?.trim() ?? "";
        return text === "" ? [] : [{ kind: "said", text }];
      }
      if (block.type === "thinking") {
        const text = str(block.thinking)?.trim() ?? "";
        return text === "" ? [] : [{ kind: "thought", text }];
      }
      if (block.type === "tool_use") {
        return [{ kind: "tool", name: str(block.name) ?? "tool", detail: toolDetail(block.input) }];
      }
      return [];
    });
  }

  if (type === "user") {
    return content.flatMap((block): RunEvent[] => {
      if (!isObject(block) || block.type !== "tool_result") return [];
      const { text, more } = headOf(resultText(block.content));
      return [{ kind: "result", text: text === "" ? "(no output)" : text, more, error: block.is_error === true }];
    });
  }

  if (type === "system") {
    const subtype = str(line.subtype);
    // A running estimate re-sent every few seconds: the context bar above already carries it.
    if (subtype === "thinking_tokens") return [];
    if (subtype === "init") {
      const model = str(line.model);
      return [{ kind: "meta", text: model === undefined ? "session started" : `session started · ${model}` }];
    }
    return [{ kind: "meta", text: subtype ?? "system" }];
  }

  if (type === "result") {
    const error = line.is_error === true;
    const bits = [error ? "ended with an error" : "finished"];
    if (typeof line.num_turns === "number") bits.push(`${line.num_turns} turns`);
    const cost = money(line.total_cost_usd);
    if (cost !== undefined) bits.push(cost);
    const said = str(line.result)?.trim();
    return [{ kind: "done", text: said ? `${bits.join(" · ")}\n${said}` : bits.join(" · "), error }];
  }

  // Partial-message deltas and other envelopes with nothing a person reads.
  if (type === "stream_event" || type === "rate_limit_event") return [];

  return [{ kind: "meta", text: type ?? "event" }];
}

/**
 * The whole tail so far, as events.
 *
 * A line still being written — the text after the last newline — is held back when it looks like
 * the start of an object, because half a JSON object is neither an event nor worth reading raw; it
 * is drawn on the next poll, whole. Anything else unfinished is plain text and is shown as it is.
 */
export function readRunStream(text: string): RunEvent[] {
  const lines = text.split("\n");
  const last = lines.pop() ?? "";
  const events: RunEvent[] = [];
  let raw: string[] = [];

  const flush = () => {
    if (raw.length > 0) events.push({ kind: "raw", text: raw.join("\n") });
    raw = [];
  };

  const take = (line: string) => {
    const trimmed = line.trim();
    if (trimmed.startsWith("{")) {
      try {
        const parsed: unknown = JSON.parse(trimmed);
        if (isObject(parsed)) {
          flush();
          events.push(...eventsOf(parsed));
          return;
        }
      } catch {
        // Not JSON after all: it is somebody's text, and text is shown as it came.
      }
    }
    raw.push(line);
  };

  for (const line of lines) take(line);
  if (last !== "" && !last.trimStart().startsWith("{")) raw.push(last);
  flush();

  // Runs of blank raw lines between events say nothing.
  return events.filter((event) => event.kind !== "raw" || event.text.trim() !== "");
}
