import { describe, expect, it } from "vitest";
import { readRunStream, RESULT_LINES } from "./run-stream";

const line = (value: unknown) => JSON.stringify(value) + "\n";

describe("readRunStream", () => {
  it("reads what the agent said and which tool it called on what", () => {
    const events = readRunStream(
      line({
        type: "assistant",
        message: {
          content: [
            { type: "text", text: "Looking at the processes." },
            { type: "tool_use", name: "Bash", input: { command: "ps -ef", description: "list" } },
          ],
        },
      }),
    );
    expect(events).toEqual([
      { kind: "said", text: "Looking at the processes." },
      { kind: "tool", name: "Bash", detail: "ps -ef" },
    ]);
  });

  it("keeps the head of a tool result and counts the rest", () => {
    const body = Array.from({ length: RESULT_LINES + 3 }, (_, at) => `row ${at}`).join("\n");
    const [event] = readRunStream(
      line({
        type: "user",
        message: { content: [{ type: "tool_result", content: body, is_error: false }] },
        tool_use_result: { stdout: body },
      }),
    );
    expect(event).toMatchObject({ kind: "result", more: 3, error: false });
    expect(event.kind === "result" && event.text.split("\n")).toHaveLength(RESULT_LINES);
  });

  it("drops the thinking-token estimate and reads the end of the run", () => {
    const events = readRunStream(
      line({ type: "system", subtype: "thinking_tokens", estimated_tokens: 50 }) +
        line({ type: "result", is_error: false, num_turns: 4, total_cost_usd: 0.123, result: "All green." }),
    );
    expect(events).toEqual([{ kind: "done", text: "finished · 4 turns · $0.12\nAll green.", error: false }]);
  });

  it("holds back half an object until it is whole, and shows plain text as it came", () => {
    expect(readRunStream('{"type":"assist')).toEqual([]);
    expect(readRunStream("olá mundo")).toEqual([{ kind: "raw", text: "olá mundo" }]);
    expect(readRunStream("not json {\n")).toEqual([{ kind: "raw", text: "not json {" }]);
  });
});
