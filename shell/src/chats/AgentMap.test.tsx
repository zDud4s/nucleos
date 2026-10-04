import { describe, expect, it } from "vitest";
import { fireEvent, render, screen } from "@testing-library/react";
import { AgentMapBody, durationText, groupAgents } from "./AgentMap";
import type { ToolCall, Turn } from "../lib/turns";

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

describe("groupAgents", () => {
  it("separates subagents from background tasks and attaches a subagent's own calls", () => {
    const g = groupAgents([
      turn([
        call({ id: "a", name: "Task", detail: "explore" }),
        call({ id: "x", name: "Grep", parent: "a" }),
        call({ name: "Read" }),
        call({ id: "b", name: "Bash", background: true, status: "running", detail: "sleep" }),
        call({ id: "c", name: "Agent", detail: "second", finished_at: "2026-10-03T10:01:00Z" }),
      ]),
    ]);
    expect(g.subagents.map((s) => s.key)).toEqual(["a", "c"]);
    expect(g.subagents[0].calls.map((c) => c.name)).toEqual(["Grep"]);
    expect(g.background.map((s) => s.key)).toEqual(["b"]);
    expect(g.total).toBe(3);
    // Settled turn: nothing is working, even a background task saved as "running" -- that word was
    // current only when the turn ended.
    expect(g.running).toBe(0);
  });

  it("counts a subagent without finished_at as working while its turn is live", () => {
    const g = groupAgents([turn([call({ id: "a", name: "Task" })], "running")]);
    expect(g.running).toBe(1);
  });

  it("has nothing for a daemon that sends none of the new fields", () => {
    expect(groupAgents([turn([call({ name: "Read" })])]).total).toBe(0);
    expect(groupAgents(undefined).total).toBe(0);
  });
});

describe("durationText", () => {
  it("formats", () => {
    expect(durationText(45_000)).toBe("45s");
    expect(durationText(1_430_000)).toBe("23m 50s");
  });
});

describe("AgentMapBody", () => {
  const turns = [
    turn([
      call({ id: "a", name: "Task", detail: "explore", subagent_type: "Explore", tokens: 5000 }),
      call({ id: "x", name: "Grep", detail: "needle", parent: "a" }),
      call({ id: "b", name: "Bash", detail: "sleep 9", background: true, status: "running", started_at: "2026-10-03T10:00:00Z" }),
    ], "running"),
  ];

  it("draws the root, the children and the background count, and opens a detail view", () => {
    render(<AgentMapBody turns={turns} chatTitle="My chat" now={Date.parse("2026-10-03T10:23:50Z")} />);
    expect(screen.getByText("My chat")).toBeTruthy();
    expect(screen.getByText(/12\.3k context/)).toBeTruthy();
    expect(screen.getByText("1 background task")).toBeTruthy();
    expect(screen.getByText(/shell · 23m 50s/)).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: /explore/ }));
    expect(screen.getByText("needle")).toBeTruthy();
    expect(screen.getByText(/Explore/)).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Back to the map" }));
    expect(screen.getByText("1 background task")).toBeTruthy();
  });
});
