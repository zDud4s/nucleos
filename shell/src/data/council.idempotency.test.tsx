import { createElement, type ReactNode } from "react";
import { QueryClientProvider } from "@tanstack/react-query";
import { act, renderHook } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

const client = vi.hoisted(() => ({ apiFetch: vi.fn() }));
vi.mock("./client", async (original) => ({
  ...(await original<typeof import("./client")>()),
  apiFetch: client.apiFetch,
}));

import { createAppQueryClient } from "../app/queryClient";
import { COUNCIL_KEYS, useCreateCouncil } from "./council";
import { keys } from "./keys";

beforeEach(() => {
  client.apiFetch.mockReset();
});

function setup() {
  const qc = createAppQueryClient();
  const wrapper = ({ children }: { children: ReactNode }) =>
    createElement(QueryClientProvider, { client: qc }, children);
  return renderHook(() => useCreateCouncil(), { wrapper });
}

function keyOf(call: number): string | null {
  const init = client.apiFetch.mock.calls[call][1] as RequestInit;
  return new Headers(init.headers).get("Idempotency-Key");
}

describe("COUNCIL_KEYS", () => {
  it("is exported and sits under the council root", () => {
    expect(COUNCIL_KEYS.list.slice(0, keys.council.all.length)).toEqual([...keys.council.all]);
    expect(COUNCIL_KEYS.detail("x")).toContain("x");
  });
});

describe("useCreateCouncil idempotency", () => {
  it("reuses the key across retries of the same intent, then mints a fresh one after success", async () => {
    client.apiFetch.mockRejectedValueOnce(new Error("net")).mockRejectedValueOnce(new Error("net"));
    client.apiFetch.mockResolvedValue({ id: "c1" });
    const { result } = setup();

    await act(async () => { await result.current.mutateAsync({ question: "q?" }).catch(() => {}); });
    await act(async () => { await result.current.mutateAsync({ question: "q?" }).catch(() => {}); });
    await act(async () => { await result.current.mutateAsync({ question: "q?" }); });
    expect(keyOf(0)).toBeTruthy();
    expect(keyOf(1)).toBe(keyOf(0));
    expect(keyOf(2)).toBe(keyOf(0));

    await act(async () => { await result.current.mutateAsync({ question: "q?" }); });
    expect(keyOf(3)).toBeTruthy();
    expect(keyOf(3)).not.toBe(keyOf(0));
  });

  it("mints a fresh key when the input changed", async () => {
    client.apiFetch.mockRejectedValue(new Error("net"));
    const { result } = setup();
    await act(async () => { await result.current.mutateAsync({ question: "a" }).catch(() => {}); });
    await act(async () => { await result.current.mutateAsync({ question: "b" }).catch(() => {}); });
    expect(keyOf(1)).not.toBe(keyOf(0));
  });
});
