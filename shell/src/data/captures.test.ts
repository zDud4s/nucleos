import { createElement, type ReactNode } from "react";
import { QueryClientProvider } from "@tanstack/react-query";
import { renderHook, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

const client = vi.hoisted(() => ({ apiFetch: vi.fn() }));
vi.mock("./client", async (original) => ({
  ...(await original<typeof import("./client")>()),
  apiFetch: client.apiFetch,
}));

import { createAppQueryClient } from "../app/queryClient";
import { useAllCaptures, useAnswerCapture, useDismissCapture, useOpenCaptures } from "./captures";

beforeEach(() => {
  client.apiFetch.mockReset();
});

function wrapper() {
  const queryClient = createAppQueryClient();
  return ({ children }: { children: ReactNode }) =>
    createElement(QueryClientProvider, { client: queryClient }, children);
}

describe("capture hooks", () => {
  it("reads the open requests from the bare route", async () => {
    client.apiFetch.mockResolvedValue([]);
    const { result } = renderHook(() => useOpenCaptures(), { wrapper: wrapper() });
    await waitFor(() => expect(result.current.data).toEqual([]));
    expect(client.apiFetch).toHaveBeenCalledWith("/capture-requests");
  });

  it("reads every state with ?state=all", async () => {
    client.apiFetch.mockResolvedValue([]);
    const { result } = renderHook(() => useAllCaptures(), { wrapper: wrapper() });
    await waitFor(() => expect(result.current.data).toEqual([]));
    expect(client.apiFetch).toHaveBeenCalledWith("/capture-requests?state=all");
  });

  it("posts the answer with the shell as its origin", async () => {
    client.apiFetch.mockResolvedValue({ note_id: 1, released: true });
    const { result } = renderHook(() => useAnswerCapture(), { wrapper: wrapper() });
    result.current.mutate({ jobId: 7, text: "because" });
    await waitFor(() => expect(result.current.isSuccess).toBe(true));
    expect(client.apiFetch).toHaveBeenCalledWith("/capture-requests/7/answer", {
      method: "POST",
      body: JSON.stringify({ text: "because", origin: "shell" }),
    });
  });

  it("posts a dismissal to the job's dismiss route", async () => {
    client.apiFetch.mockResolvedValue({});
    const { result } = renderHook(() => useDismissCapture(), { wrapper: wrapper() });
    result.current.mutate({ jobId: 7 });
    await waitFor(() => expect(result.current.isSuccess).toBe(true));
    expect(client.apiFetch).toHaveBeenCalledWith("/capture-requests/7/dismiss", { method: "POST" });
  });
});
