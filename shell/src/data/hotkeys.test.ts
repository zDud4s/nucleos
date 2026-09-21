import { renderHook, waitFor } from "@testing-library/react";
import { createElement, type ReactNode } from "react";
import { QueryClientProvider } from "@tanstack/react-query";
import { beforeEach, describe, expect, it, vi } from "vitest";

const tauri = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => tauri);

const daemon = vi.hoisted(() => ({ apiFetch: vi.fn() }));
vi.mock("./client", async (original) => ({
  ...(await original<typeof import("./client")>()),
  apiFetch: daemon.apiFetch,
}));

import { createAppQueryClient } from "../app/queryClient";
import { useHotkeyRegistration } from "./hotkeys";

const CHORDS = { hotkey: "Ctrl+Alt+D", memo_hotkey: "Ctrl+Alt+M", conversation_hotkey: "Ctrl+Alt+C" };

function registrations(): unknown[] {
  return tauri.invoke.mock.calls.filter(([command]) => command === "voice_register_hotkeys").map((call) => call[1]);
}

function wrapper() {
  const client = createAppQueryClient();
  return ({ children }: { children: ReactNode }) => createElement(QueryClientProvider, { client }, children);
}

beforeEach(() => {
  tauri.invoke.mockReset();
  tauri.invoke.mockImplementation(async (command: string) =>
    command === "voice_register_hotkeys" ? [] : null,
  );
  daemon.apiFetch.mockReset();
  daemon.apiFetch.mockResolvedValue(CHORDS);
});

describe("useHotkeyRegistration", () => {
  /* The shell and the Voice page both ask, and the host must be asked once: every registration
     unregisters all three chords before it registers any, so a second one is a moment with none. */
  it("registers the three chords once, however many ask", async () => {
    const { result } = renderHook(() => [useHotkeyRegistration(), useHotkeyRegistration()], {
      wrapper: wrapper(),
    });

    await waitFor(() => expect(result.current[0]).toEqual({ unavailable: null, conflicts: [], failed: false }));
    expect(result.current[1]).toEqual(result.current[0]);
    expect(registrations()).toEqual([{ dictation: "Ctrl+Alt+D", memo: "Ctrl+Alt+M", conversation: "Ctrl+Alt+C" }]);
  });

  it("registers nothing on a desktop that gives out no global hotkeys", async () => {
    const sentence = "this desktop runs Wayland, which gives no application global hotkeys";
    tauri.invoke.mockImplementation(async (command: string) =>
      command === "voice_hotkeys_unavailable" ? sentence : [],
    );

    const { result } = renderHook(() => useHotkeyRegistration(), { wrapper: wrapper() });

    await waitFor(() => expect(result.current?.unavailable).toBe(sentence));
    expect(registrations()).toEqual([]);
  });

  it("says the registration failed rather than throwing", async () => {
    tauri.invoke.mockImplementation(async (command: string) => {
      if (command === "voice_register_hotkeys") throw new Error("host unavailable");
      return null;
    });

    const { result } = renderHook(() => useHotkeyRegistration(), { wrapper: wrapper() });

    await waitFor(() => expect(result.current).toEqual({ unavailable: null, conflicts: null, failed: true }));
  });
});
