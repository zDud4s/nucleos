import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { act, renderHook, waitFor } from "@testing-library/react";
import type { ReactNode } from "react";
import { describe, expect, it, vi } from "vitest";
import { useSWR } from "./swr";

function wrap(qc: QueryClient) {
  return ({ children }: { children: ReactNode }) => <QueryClientProvider client={qc}>{children}</QueryClientProvider>;
}

describe("useSWR", () => {
  it("returns cached data immediately while revalidating in the background", async () => {
    const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    qc.setQueryData(["k"], "cached");
    const queryFn = vi.fn(() => Promise.resolve("fresh"));
    const { result } = renderHook(() => useSWR({ queryKey: ["k"], queryFn }), { wrapper: wrap(qc) });
    expect(result.current.data).toBe("cached");
    expect(result.current.isLoading).toBe(false);
    await waitFor(() => expect(result.current.data).toBe("fresh"));
  });
  it("mutate writes the cache without a request", async () => {
    const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    qc.setQueryData(["m"], 1);
    const queryFn = vi.fn(() => new Promise<number>(() => {}));
    const { result } = renderHook(() => useSWR({ queryKey: ["m"], queryFn, enabled: false }), { wrapper: wrap(qc) });
    act(() => void result.current.mutate(2));
    await waitFor(() => expect(result.current.data).toBe(2));
    expect(queryFn).not.toHaveBeenCalled();
  });
});
