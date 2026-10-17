import { QueryClientProvider } from "@tanstack/react-query";
import { renderHook, waitFor } from "@testing-library/react";
import type { ReactNode } from "react";
import { describe, expect, it, vi } from "vitest";

const stream = vi.hoisted(() => ({ streamLiveTurn: vi.fn() }));
vi.mock("./liveStream", () => stream);
const daemon = vi.hoisted(() => ({ apiFetch: vi.fn() }));
vi.mock("./client", async (original) => ({
  ...(await original<typeof import("./client")>()),
  ...daemon,
}));

import { createAppQueryClient } from "../app/queryClient";
import { useLiveTurn } from "./chats";

describe("useLiveTurn", () => {
  it("retries the stream after a drop instead of polling for good", async () => {
    daemon.apiFetch.mockResolvedValue(undefined);
    stream.streamLiveTurn
      .mockResolvedValueOnce("failed")
      .mockImplementation(() => new Promise(() => {}));
    const queryClient = createAppQueryClient();
    const wrapper = ({ children }: { children: ReactNode }) => (
      <QueryClientProvider client={queryClient}>{children}</QueryClientProvider>
    );

    const view = renderHook(() => useLiveTurn(3, true, { stream: true }), { wrapper });

    await waitFor(() => expect(stream.streamLiveTurn).toHaveBeenCalledTimes(2), { timeout: 5000 });
    view.unmount();
  });
});
