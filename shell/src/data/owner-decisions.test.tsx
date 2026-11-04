import { describe, expect, it, vi, beforeEach } from "vitest";
import type { ReactNode } from "react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { act, renderHook, waitFor } from "@testing-library/react";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
const apiFetch = vi.fn();
vi.mock("./client", async (original) => ({
  ...(await original<typeof import("./client")>()),
  apiFetch: (...args: unknown[]) => apiFetch(...args),
}));

import { keys } from "./keys";
import {
  answerableCount,
  isAnswerable,
  useAnswerOwnerDecision,
  useOwnerDecisions,
  type OwnerDecision,
} from "./owner-decisions";

const rows = [
  { id: "d1", tree_id: "t", node_id: "n", question: "Q1?", at: "2026-11-04T10:00:00+00:00", status: "open", answer: null, kind: "decision" },
  { id: "pending-1", tree_id: "t", node_id: "n2", question: "FYI", at: "2026-11-04T10:00:00+00:00", status: "open", answer: null, kind: "notice" },
  // An owner_decisions row: no tree fields, no kind, and a field this client has never heard of.
  { id: "d2", question: "Q2?", at: "2026-11-04T09:00:00+00:00", status: "open", answer: null, origin: "owner_decisions" },
];

function setup() {
  const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const wrapper = ({ children }: { children: ReactNode }) => (
    <QueryClientProvider client={queryClient}>{children}</QueryClientProvider>
  );
  return { queryClient, wrapper };
}

beforeEach(() => apiFetch.mockReset());

describe("useOwnerDecisions", () => {
  it("reads GET /decisions?status=open and defaults a missing kind to decision", async () => {
    apiFetch.mockResolvedValue(rows);
    const { wrapper } = setup();
    const { result } = renderHook(() => useOwnerDecisions(), { wrapper });
    await waitFor(() => expect(result.current.data).toBeDefined());
    expect(apiFetch).toHaveBeenCalledWith("/decisions?status=open");
    const data = result.current.data as OwnerDecision[];
    expect(data.map((d) => d.kind)).toEqual(["decision", "notice", "decision"]);
    expect(data[2].tree_id).toBeNull();
    expect(data[2].node_id).toBeNull();
  });

  it("counts only answerable decisions, never notices", async () => {
    apiFetch.mockResolvedValue(rows);
    const { wrapper } = setup();
    const { result } = renderHook(() => useOwnerDecisions(), { wrapper });
    await waitFor(() => expect(result.current.data).toBeDefined());
    const data = result.current.data as OwnerDecision[];
    expect(data.filter(isAnswerable)).toHaveLength(2);
    expect(answerableCount(data)).toBe(2);
    expect(answerableCount(undefined)).toBeUndefined();
  });
});

describe("useAnswerOwnerDecision", () => {
  it("posts the answer to /decisions/{id} and invalidates the open list", async () => {
    apiFetch.mockResolvedValue(undefined);
    const { wrapper, queryClient } = setup();
    const spy = vi.spyOn(queryClient, "invalidateQueries");
    const { result } = renderHook(() => useAnswerOwnerDecision(), { wrapper });
    await act(async () => {
      await result.current.mutateAsync({ id: "d/1", answer: "yes, go" });
    });
    expect(apiFetch).toHaveBeenCalledWith("/decisions/d%2F1", {
      method: "POST",
      body: JSON.stringify({ answer: "yes, go" }),
    });
    expect(spy).toHaveBeenCalledWith({ queryKey: keys.ownerDecisions.open });
  });
});
