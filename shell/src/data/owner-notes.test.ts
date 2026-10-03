import { renderHook, waitFor } from "@testing-library/react";
import { createElement, type ReactNode } from "react";
import { QueryClientProvider } from "@tanstack/react-query";
import { beforeEach, describe, expect, it, vi } from "vitest";

const daemon = vi.hoisted(() => ({ apiFetch: vi.fn() }));
vi.mock("./client", async (original) => ({
  ...(await original<typeof import("./client")>()),
  apiFetch: daemon.apiFetch,
}));

import { createAppQueryClient } from "../app/queryClient";
import { keys } from "./keys";
import { useCreateNote, useRemoveLink, useSearchOwnerNotes } from "./owner-notes";

function setup() {
  const client = createAppQueryClient();
  const wrapper = ({ children }: { children: ReactNode }) =>
    createElement(QueryClientProvider, { client }, children);
  return { client, wrapper };
}

beforeEach(() => {
  daemon.apiFetch.mockReset();
  daemon.apiFetch.mockResolvedValue(undefined);
});

describe("owner notes hooks", () => {
  it("creating a note posts text and origin and invalidates the list", async () => {
    daemon.apiFetch.mockResolvedValue({ id: 7 });
    const { client, wrapper } = setup();
    const invalidate = vi.spyOn(client, "invalidateQueries");
    const { result } = renderHook(() => useCreateNote(), { wrapper });

    result.current.mutate({ text: "hello", origin: "shell" });

    await waitFor(() => expect(result.current.isSuccess).toBe(true));
    expect(daemon.apiFetch).toHaveBeenCalledWith("/owner-notes", {
      method: "POST",
      body: JSON.stringify({ text: "hello", origin: "shell" }),
    });
    expect(invalidate).toHaveBeenCalledWith({ queryKey: keys.ownerNotes.all });
  });

  it("search is not sent for a blank query", async () => {
    const { wrapper } = setup();
    const { result } = renderHook(() => useSearchOwnerNotes("   "), { wrapper });

    await new Promise((resolve) => setTimeout(resolve, 20));
    expect(result.current.fetchStatus).toBe("idle");
    expect(daemon.apiFetch).not.toHaveBeenCalled();
  });

  it("removing a link sends DELETE to /owner-notes/links/{id}", async () => {
    const { wrapper } = setup();
    const { result } = renderHook(() => useRemoveLink(), { wrapper });

    result.current.mutate(42);

    await waitFor(() => expect(result.current.isSuccess).toBe(true));
    expect(daemon.apiFetch).toHaveBeenCalledWith("/owner-notes/links/42", { method: "DELETE" });
  });
});
