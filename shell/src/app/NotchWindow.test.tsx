import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, waitFor } from "@testing-library/react";

const tauri = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke: tauri.invoke }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn(async () => () => {}) }));

const daemon = vi.hoisted(() => ({ apiFetch: vi.fn() }));
vi.mock("../data/client", async (original) => ({
  ...(await original<typeof import("../data/client")>()),
  ...daemon,
}));

import { NotchWindow } from "./NotchWindow";
import { renderWithQuery } from "../test/harness";

/** jsdom has no layout, so no ResizeObserver either; the fit under test is the one on mount. */
class StillObserver {
  observe() {}
  disconnect() {}
}

beforeEach(() => {
  vi.stubGlobal("ResizeObserver", StillObserver);
  tauri.invoke.mockReset();
  tauri.invoke.mockResolvedValue(undefined);
  daemon.apiFetch.mockReset();
  daemon.apiFetch.mockResolvedValue({
    providers: [
      {
        provider: "claude",
        fidelity: "official",
        read_at: "2026-09-19T05:00:00Z",
        windows: [{ window: "5h", used_fraction: 0.5, resets_at: null, stale: false, state: "ok" }],
        detail: "",
        severity: "normal",
      },
    ],
    source: "sidecar",
    cached: false,
  });
});

describe("NotchWindow", () => {
  /**
   * The window is sized by what it draws, in CSS pixels. jsdom measures everything as zero, which
   * is also exactly the "nothing to draw" answer that hides the window.
   */
  it("asks to be fitted to its drawing", async () => {
    renderWithQuery(<NotchWindow />);
    await waitFor(() =>
      expect(tauri.invoke).toHaveBeenCalledWith("notch_fit", { width: 0, height: 0 }),
    );
  });

  it("docks the notch back inside the app from its own control", async () => {
    const { container, findByRole } = renderWithQuery(<NotchWindow />);
    await waitFor(() => expect(container.querySelector(".quota-notch")).not.toBeNull());
    fireEvent.pointerEnter(container.querySelector(".quota-notch")!);
    fireEvent.click(await findByRole("button", { name: "Put the notch back inside NucleOS" }));
    await waitFor(() =>
      expect(tauri.invoke).toHaveBeenCalledWith("notch_set_mode", { mode: "contained" }),
    );
  });
});
