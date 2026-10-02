import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, waitFor } from "@testing-library/react";

const tauri = vi.hoisted(() => ({ invoke: vi.fn(), listen: vi.fn(async () => () => {}) }));
vi.mock("@tauri-apps/api/core", () => ({ invoke: tauri.invoke }));
vi.mock("@tauri-apps/api/event", () => ({ listen: tauri.listen }));

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
  tauri.listen.mockClear();
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
      expect(tauri.invoke).toHaveBeenCalledWith("notch_fit", {
        width: 0,
        height: 0,
        rest: 0,
        // Nobody has moved it: the middle of the right edge, where it has always hung.
        along: 0.5,
        edge: "right",
      }),
    );
  });

  /**
   * Where the owner dragged it travels with every fit, and moving it asks for a fit of its own —
   * a drag moves the window without changing the size the `ResizeObserver` watches, so without
   * this the Rust side would never hear that the notch had moved.
   */
  it("hangs where the owner dragged it, and asks again when it is moved", async () => {
    window.localStorage.setItem("nucleos.notch-along", "0.25");
    const { container } = renderWithQuery(<NotchWindow />);
    await waitFor(() =>
      expect(tauri.invoke).toHaveBeenCalledWith("notch_fit", expect.objectContaining({ along: 0.25 })),
    );
    await waitFor(() => expect(container.querySelector(".quota-notch-rail")).not.toBeNull());

    const rail = container.querySelector(".quota-notch-rail")!;
    Object.defineProperty(window.screen, "availHeight", { value: 1000, configurable: true });
    Object.defineProperty(window.screen, "availWidth", { value: 1920, configurable: true });
    fireEvent.pointerDown(rail, { button: 0, screenX: 1900, screenY: 400, pointerId: 1 });
    fireEvent.pointerMove(rail, { screenX: 1900, screenY: 650, pointerId: 1 });
    fireEvent.pointerUp(rail, { screenX: 1900, screenY: 650, pointerId: 1 });

    // 250 screen pixels of a 1000-pixel work area is a quarter of the way further down.
    await waitFor(() =>
      expect(tauri.invoke).toHaveBeenCalledWith("notch_fit", expect.objectContaining({ along: 0.5 })),
    );
    expect(window.localStorage.getItem("nucleos.notch-along")).toBe("0.5");

    // Carried to the bottom of the screen, it hangs from the bottom edge, and the fit says so.
    fireEvent.pointerDown(rail, { button: 0, screenX: 1900, screenY: 650, pointerId: 2 });
    fireEvent.pointerMove(rail, { screenX: 960, screenY: 995, pointerId: 2 });
    fireEvent.pointerUp(rail, { screenX: 960, screenY: 995, pointerId: 2 });
    await waitFor(() =>
      expect(tauri.invoke).toHaveBeenCalledWith(
        "notch_fit",
        expect.objectContaining({ along: 0.5, edge: "bottom" }),
      ),
    );
    expect(window.localStorage.getItem("nucleos.notch-edge")).toBe("bottom");
    window.localStorage.removeItem("nucleos.notch-along");
    window.localStorage.removeItem("nucleos.notch-edge");
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

  /**
   * The floating window holds no capability of its own (`src-tauri/src/notch.rs`), which buys it
   * the app's own commands and nothing else: every plugin command, window API and event
   * subscription is refused there. `listen` is the one that bites, because it is mocked in this
   * file and in `notch-mode.test.tsx` — a subscription added to this page passes CI and is dead on
   * the owner's screen, silently, since the page catches what it cannot do. So it is asserted
   * outright rather than left to the mock: nothing here listens, and every command it does invoke
   * is one of the app's own.
   */
  const OWN_COMMANDS = ["notch_fit", "notch_set_mode", "get_daemon_token"];

  it("reaches for nothing the notch window is refused", async () => {
    const { container, findByRole } = renderWithQuery(<NotchWindow />);
    await waitFor(() => expect(container.querySelector(".quota-notch")).not.toBeNull());
    fireEvent.pointerEnter(container.querySelector(".quota-notch")!);
    fireEvent.click(await findByRole("button", { name: "Put the notch back inside NucleOS" }));
    await waitFor(() => expect(tauri.invoke).toHaveBeenCalled());

    expect(tauri.listen).not.toHaveBeenCalled();
    for (const [command] of tauri.invoke.mock.calls) expect(OWN_COMMANDS).toContain(command);
  });

  /**
   * The same rule where a mock cannot reach it: a plugin import anywhere in this window's own
   * modules. `@tauri-apps/api/event` is in `notch-mode.ts` for the main window's `useNotchMode`,
   * which this page never calls — the import is inert, the call would not be — so the assertion is
   * about the page's own files.
   */
  it("imports no plugin API into the notch window's own page", () => {
    // The same normalisation `MapCanvas.test.tsx` and `one-waiting-phrase.test.ts` do: this runner
    // hands `import.meta.url` over as a bare path, and `new URL` then resolves it against nothing.
    const here = import.meta.url.startsWith("file:") ? import.meta.url : `file://${import.meta.url}`;
    for (const name of ["NotchWindow.tsx", "QuotaNotch.tsx"]) {
      const source = readFileSync(fileURLToPath(new URL(`./${name}`, here)), "utf8");
      expect(source, name).not.toMatch(/@tauri-apps\/plugin-/);
      expect(source, name).not.toMatch(/@tauri-apps\/api\/(event|window|webviewWindow)/);
    }
  });
});
