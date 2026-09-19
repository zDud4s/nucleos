import { beforeEach, describe, expect, it, vi } from "vitest";
import { act, renderHook, waitFor } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import type { ReactNode } from "react";

const tauri = vi.hoisted(() => ({
  invoke: vi.fn(),
  listen: vi.fn(),
  heard: [] as Array<(event: { payload: unknown }) => void>,
}));
vi.mock("@tauri-apps/api/core", () => ({ invoke: tauri.invoke }));
vi.mock("@tauri-apps/api/event", () => ({ listen: tauri.listen }));

import { useNotchMode, useSetNotchMode, windowKind } from "./notch-mode";

beforeEach(() => {
  tauri.invoke.mockReset();
  tauri.listen.mockReset();
  tauri.heard.length = 0;
  tauri.listen.mockImplementation(
    async (_event: string, handler: (event: { payload: unknown }) => void) => {
      tauri.heard.push(handler);
      return () => {};
    },
  );
});

/** One cache per test. A wrapper that built its own on every render would forget each answer. */
let client: QueryClient;
beforeEach(() => {
  client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
});

function wrapper({ children }: { children: ReactNode }) {
  return <QueryClientProvider client={client}>{children}</QueryClientProvider>;
}

describe("notch mode", () => {
  /** The floating window loads the app's own bundle; only the query tells it which face to render. */
  it("tells the notch window from the app by its query", () => {
    expect(windowKind("?window=notch")).toBe("notch");
    expect(windowKind("")).toBe("app");
    expect(windowKind("?window=main")).toBe("app");
  });

  it("reads the mode the Rust side stored", async () => {
    tauri.invoke.mockResolvedValue("global");
    const { result } = renderHook(() => useNotchMode(), { wrapper });
    await waitFor(() => expect(result.current).toBe("global"));
    expect(tauri.invoke).toHaveBeenCalledWith("notch_mode");
  });

  /**
   * No Rust side — the browser preview — or one that refuses: contained, the host that cannot fail
   * to draw. A notch drawn nowhere is the failure the contained host exists to prevent.
   */
  it("reads a refusal as contained", async () => {
    tauri.invoke.mockRejectedValue(new Error("no such command"));
    const { result } = renderHook(() => useNotchMode(), { wrapper });
    await waitFor(() => expect(result.current).toBe("contained"));
  });

  /** A notch docked by closing its own window is news the main window only hears by the broadcast. */
  it("follows the broadcast when another window moves the notch", async () => {
    tauri.invoke.mockResolvedValue("global");
    const { result } = renderHook(() => useNotchMode(), { wrapper });
    await waitFor(() => expect(result.current).toBe("global"));
    await waitFor(() => expect(tauri.heard).toHaveLength(1));
    expect(tauri.listen).toHaveBeenCalledWith("notch://mode", expect.any(Function));
    act(() => tauri.heard[0]({ payload: "contained" }));
    await waitFor(() => expect(result.current).toBe("contained"));
  });

  /**
   * The command answers before the move is made, so the mode waits for the broadcast rather than
   * trusting the request: a float that failed to open a window must not put the contained notch away.
   */
  it("asks the Rust side to move the notch, and waits for the broadcast", async () => {
    tauri.invoke.mockImplementation(async (command: string) =>
      command === "notch_mode" ? "contained" : undefined,
    );
    const { result } = renderHook(() => ({ mode: useNotchMode(), set: useSetNotchMode() }), {
      wrapper,
    });
    await waitFor(() => expect(result.current.mode).toBe("contained"));
    await waitFor(() => expect(tauri.heard).toHaveLength(1));
    await act(() => result.current.set("global"));
    expect(tauri.invoke).toHaveBeenCalledWith("notch_set_mode", { mode: "global" });
    expect(result.current.mode).toBe("contained");

    act(() => tauri.heard[0]({ payload: "global" }));
    await waitFor(() => expect(result.current.mode).toBe("global"));
  });
});
