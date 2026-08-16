import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, fireEvent, render, screen } from "@testing-library/react";

import Agents from "./Agents";
import type { Agent } from "./api";

const fetchMock = vi.fn();
vi.stubGlobal("fetch", fetchMock);

function agent(overrides: Partial<Agent> = {}): Agent {
  return {
    id: "copywriter",
    name: "copywriter",
    speciality: "writes short copy",
    prompt: "You write short copy.",
    engine: "claude",
    model: "claude-opus-5",
    tool_policy: "mcp_only",
    created_at: "2026-08-12T10:00:00Z",
    updated_at: "2026-08-12T10:00:00Z",
    ...overrides,
  };
}

/** A daemon holding this catalogue, answering every write with the status given. */
function daemonHolding(roster: Agent[], write: { ok: boolean; status: number } = { ok: true, status: 200 }) {
  fetchMock.mockImplementation((_url: string, init?: { method?: string }) => {
    const method = init?.method ?? "GET";
    if (method === "GET") {
      return Promise.resolve({ ok: true, status: 200, json: async () => roster });
    }
    return Promise.resolve({ ...write, json: async () => roster[0] ?? agent() });
  });
}

async function show(roster: Agent[], write?: { ok: boolean; status: number }) {
  daemonHolding(roster, write);
  await act(async () => {
    render(<Agents token="t" connection="connected" />);
  });
}

function writeCalls() {
  return fetchMock.mock.calls.filter(([, init]) => (init as { method?: string } | undefined)?.method !== undefined);
}

/** `ConfirmButton` discards a second click inside its 300ms dwell, so the clock has to move. */
function advance(ms: number) {
  act(() => {
    vi.advanceTimersByTime(ms);
  });
}

beforeEach(() => vi.useFakeTimers({ shouldAdvanceTime: true }));
afterEach(() => {
  vi.useRealTimers();
  fetchMock.mockReset();
});

describe("the agent catalogue", () => {
  it("lists who is in the house, with what each one is for", async () => {
    await show([
      agent({ id: "analyst", name: "analyst", speciality: "reads the numbers" }),
      agent(),
    ]);

    expect(screen.getByText("analyst")).toBeTruthy();
    expect(screen.getByText("reads the numbers")).toBeTruthy();
    expect(screen.getByText("copywriter")).toBeTruthy();
    expect(screen.getByText("writes short copy")).toBeTruthy();
  });

  it("sends the whole declaration when creating one, and lists again", async () => {
    await show([]);

    fireEvent.click(screen.getByRole("button", { name: "New agent" }));
    fireEvent.change(screen.getByLabelText("Name"), { target: { value: "Head of Content" } });
    fireEvent.change(screen.getByLabelText("Speciality"), {
      target: { value: "writes long copy" },
    });
    fireEvent.change(screen.getByLabelText("Prompt"), { target: { value: "You write." } });
    fireEvent.change(screen.getByLabelText("Model"), { target: { value: "claude-opus-5" } });

    const listsBefore = fetchMock.mock.calls.length;
    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: "Save agent" }));
    });

    const [url, init] = writeCalls()[0] as [string, { method: string; body: string }];
    expect(url).toContain("/agents");
    expect(init.method).toBe("POST");
    expect(JSON.parse(init.body)).toEqual({
      name: "Head of Content",
      speciality: "writes long copy",
      prompt: "You write.",
      engine: "claude",
      model: "claude-opus-5",
      tool_policy: "mcp_only",
    });
    // The catalogue is re-read rather than patched in memory: the id is the daemon's to derive.
    expect(fetchMock.mock.calls.length).toBeGreaterThan(listsBefore + 1);
  });

  /**
   * The only refusal the owner meets in practice, and the reason `deleteAgent` carries a status
   * instead of a boolean. A generic "could not delete" would hide the one fact that says what to do
   * next — that a team is standing on this agent.
   */
  it("says why a delete was refused instead of failing silently", async () => {
    await show([agent()], { ok: false, status: 409 });

    fireEvent.click(screen.getByRole("button", { name: "Delete" }));
    advance(400);
    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: "Confirm delete?" }));
    });

    expect(screen.getByRole("alert").textContent).toContain("team");
  });
});
