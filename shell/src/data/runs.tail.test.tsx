import { QueryClientProvider } from "@tanstack/react-query";
import { renderHook, waitFor } from "@testing-library/react";
import type { ReactNode } from "react";
import { describe, expect, it, vi } from "vitest";

const daemon = vi.hoisted(() => ({ apiFetch: vi.fn() }));
vi.mock("./client", async (original) => ({
  ...(await original<typeof import("./client")>()),
  ...daemon,
}));

import { createAppQueryClient } from "../app/queryClient";
import { keys } from "./keys";
import { useRunTail } from "./runs";

describe("useRunTail", () => {
  it("does not outlive its reader, whose cursor and text are component state", async () => {
    daemon.apiFetch.mockResolvedValue({ text: "line\n", next: 5 });
    const queryClient = createAppQueryClient();
    const wrapper = ({ children }: { children: ReactNode }) => (
      <QueryClientProvider client={queryClient}>{children}</QueryClientProvider>
    );

    const view = renderHook(() => useRunTail(7, 0, false), { wrapper });
    await waitFor(() => expect(view.result.current.data).toBeDefined());
    view.unmount();

    // A cached chunk handed to a remounted reader whose cursor is back at 0 is
    // what duplicated and reordered the run's output.
    await waitFor(() => expect(queryClient.getQueryData(keys.runs.tail(7))).toBeUndefined());
  });
});
