import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { act, renderHook, waitFor } from "@testing-library/react";
import type { ReactNode } from "react";
import { describe, expect, it, vi } from "vitest";
import { useOptimisticMutation } from "./optimistic";

function setup(mutationFn: (v: number) => Promise<unknown>) {
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  qc.setQueryData(["n"], 1);
  const onError = vi.fn();
  const wrapper = ({ children }: { children: ReactNode }) => <QueryClientProvider client={qc}>{children}</QueryClientProvider>;
  const hook = renderHook(
    () => useOptimisticMutation<number, number>({ queryKey: ["n"], mutationFn, apply: (_p, v) => v, onError }),
    { wrapper },
  );
  return { qc, onError, hook };
}

describe("useOptimisticMutation", () => {
  it("applies the change before the request settles", async () => {
    let release!: () => void;
    const { qc, hook } = setup(() => new Promise((r) => (release = () => r(null))));
    act(() => hook.result.current.mutate(5));
    await waitFor(() => expect(qc.getQueryData(["n"])).toBe(5));
    release();
  });
  it("rolls back and reports on failure", async () => {
    const { qc, onError, hook } = setup(() => Promise.reject(new Error("no")));
    act(() => hook.result.current.mutate(9));
    await waitFor(() => expect(onError).toHaveBeenCalledTimes(1));
    expect(qc.getQueryData(["n"])).toBe(1);
  });
});
