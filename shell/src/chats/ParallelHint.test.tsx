import { describe, expect, it, vi } from "vitest";
import { QueryClientProvider } from "@tanstack/react-query";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

const daemon = vi.hoisted(() => ({
  apiFetch: vi.fn(),
  apiText: vi.fn(),
  apiBlob: vi.fn(),
  probeHealth: vi.fn(),
}));
vi.mock("../data/client", async (original) => ({
  ...(await original<typeof import("../data/client")>()),
  ...daemon,
}));

import { render, screen } from "@testing-library/react";
import { PARALLEL_HINT, ParallelHint, parallelHint } from "./AgentMap";
import type { ToolCall, Turn } from "../lib/turns";
import { createAppQueryClient } from "../app/queryClient";

const call = (over: Partial<ToolCall>): ToolCall => ({ name: "Bash", detail: null, todos: [], ...over });

function turn(did: ToolCall[], status = "completed"): Turn {
  return {
    id: 1,
    asked: "q",
    answer: "a",
    status,
    createdAt: "2026-10-03T10:00:00Z",
    did,
    contextFill: 12_300,
    model: "claude-opus-4",
  } as Turn;
}

const two = () => [call({ id: "a", name: "Task" }), call({ id: "b", name: "Agent" })];

describe("parallelHint", () => {
  it("returns the hint for 2 open subagents on a live turn under manual", () => {
    expect(parallelHint([turn(two(), "running")], "manual")).toBe(PARALLEL_HINT);
  });

  it.each(["accept_edits", "auto"])("also returns the hint under %s", (mode) => {
    expect(parallelHint([turn(two(), "running")], mode)).toBe(PARALLEL_HINT);
  });

  it.each(["bypass", "dont_ask", "plan"])("returns null under %s", (mode) => {
    expect(parallelHint([turn(two(), "running")], mode)).toBeNull();
  });

  it("returns null for a single subagent", () => {
    expect(parallelHint([turn([call({ id: "a", name: "Task" })], "running")], "manual")).toBeNull();
  });

  it("returns null when the turn is settled", () => {
    expect(parallelHint([turn(two(), "completed")], "manual")).toBeNull();
  });

  it("does not count finished subagents or a subagent's own nested calls", () => {
    const did = [
      call({ id: "a", name: "Task", finished_at: "2026-10-03T10:01:00Z" }),
      call({ id: "b", name: "Task" }),
      call({ name: "Task", parent: "a" }),
    ];
    expect(parallelHint([turn(did, "running")], "manual")).toBeNull();
  });

  it("returns null when mode is undefined", () => {
    expect(parallelHint([turn(two(), "running")], undefined)).toBeNull();
  });
});

describe("ParallelHint", () => {
  it("renders the hint as a status note and nothing otherwise", async () => {
    daemon.apiFetch.mockImplementation(async (path: string) => {
      if (path === "/assistant/chats/c/project") {
        return { cwd: "/r", tools: true, session: null, permission_mode: "manual" };
      }
      return null;
    });
    const client = createAppQueryClient();
    const view = render(
      <QueryClientProvider client={client}>
        <ParallelHint chatId="c" turns={[turn(two(), "running")]} />
      </QueryClientProvider>,
    );
    const note = await screen.findByRole("note");
    expect(note.textContent).toBe(PARALLEL_HINT);

    view.rerender(
      <QueryClientProvider client={client}>
        <ParallelHint chatId="c" turns={[]} />
      </QueryClientProvider>,
    );
    expect(screen.queryByRole("note")).toBeNull();
  });
});
