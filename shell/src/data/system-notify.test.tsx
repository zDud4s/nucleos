import { describe, expect, it, vi } from "vitest";
import type { ReactNode } from "react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { renderHook, waitFor } from "@testing-library/react";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("./client", async (original) => ({
  ...(await original<typeof import("./client")>()),
  apiFetch: vi.fn(async () => ({ families: [], kinds: [] })),
  probeHealth: vi.fn(async () => true),
}));

import { useNotifyPolicy, useObservedKinds } from "./system";

/**
 * Neither notification read polls, and that is worth pinning because in this
 * layer NOT polling is the exception: almost every neighbour runs at
 * `POLL.fast`, so an edit that "fixes the inconsistency" is exactly the
 * regression to catch.
 *
 * The reasons differ. `useNotifyPolicy` is the initial state of a form —
 * refetching it every three seconds would walk over the switches somebody is
 * in the middle of flipping. `useObservedKinds` is a ninety-day `DISTINCT` over
 * the feed, and reading it once per open is what makes the absence of an index
 * on `feed (kind)` the right trade rather than a shortcut.
 *
 * Read off the query cache rather than off the source, so this observes the
 * configuration React Query actually received.
 */
describe("the notification queries", () => {
  it("do not poll", async () => {
    const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    const wrapper = ({ children }: { children: ReactNode }) => (
      <QueryClientProvider client={queryClient}>{children}</QueryClientProvider>
    );

    renderHook(
      () => {
        useNotifyPolicy();
        useObservedKinds();
      },
      { wrapper },
    );

    await waitFor(() => expect(queryClient.getQueryCache().getAll()).toHaveLength(2));
    for (const query of queryClient.getQueryCache().getAll()) {
      // `refetchInterval` is a `useQuery` option that the cache entry carries at
      // run time but that `QueryOptions` does not declare, so it is read through
      // a cast rather than off the typed shape.
      const options = query.options as { refetchInterval?: unknown };
      expect(options.refetchInterval).toBeUndefined();
    }
  });
});
