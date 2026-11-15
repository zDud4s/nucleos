import { beforeEach, describe, expect, it, vi } from "vitest";
import { createElement, type ReactNode } from "react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { renderHook, waitFor } from "@testing-library/react";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
const apiFetch = vi.fn();
vi.mock("./client", async (original) => ({
  ...(await original<typeof import("./client")>()),
  apiFetch: (...args: unknown[]) => apiFetch(...args),
}));

import { answerableCount, parseOwnerDecision, useOwnerDecisions, type OwnerDecision } from "./owner-decisions";

const at = "2026-11-04T10:00:00+00:00";
const open = { id: "d1", question: "Q1?", at, status: "open", answer: null, kind: "decision" };

function setup() {
  const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const wrapper = ({ children }: { children: ReactNode }) =>
    createElement(QueryClientProvider, { client: queryClient }, children);
  return { wrapper };
}

async function load(rows: unknown[]): Promise<OwnerDecision[]> {
  apiFetch.mockResolvedValue(rows);
  const { wrapper } = setup();
  const { result } = renderHook(() => useOwnerDecisions(), { wrapper });
  await waitFor(() => expect(result.current.data).toBeDefined());
  return result.current.data as OwnerDecision[];
}

beforeEach(() => apiFetch.mockReset());

describe("owner decisions: obsolete rows are never waiting", () => {
  it("excludes obsolete and answered rows from the list and the count", async () => {
    const data = await load([
      open,
      { id: "o1", question: "Stale?", at, status: "obsolete", obsolete_reason: "branch merged", kind: "decision" },
      { id: "a1", question: "Done?", at, status: "answered", answer: "yes", kind: "decision" },
    ]);
    expect(data.map((d) => d.id)).toEqual(["d1"]);
    expect(answerableCount(data)).toBe(1);
  });

  it("still shows open rows, including one with no status", async () => {
    const data = await load([open, { id: "n1", question: "No status?" }]);
    expect(data.map((d) => d.id)).toEqual(["d1", "n1"]);
    expect(answerableCount(data)).toBe(2);
  });

  it("does not crash on an unknown status and does not treat it as waiting", async () => {
    const data = await load([open, { id: "u1", question: "?", at, status: "something-new" }]);
    expect(data.map((d) => d.id)).toEqual(["d1"]);
  });

  it("accepts obsolete_reason on the wire type", () => {
    expect(parseOwnerDecision({ id: "o1", status: "obsolete", obsolete_reason: "gone" }).obsolete_reason).toBe("gone");
    expect(parseOwnerDecision({ id: "x" }).obsolete_reason).toBeNull();
  });
});
