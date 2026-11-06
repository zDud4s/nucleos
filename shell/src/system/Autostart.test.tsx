import { describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { QueryClientProvider } from "@tanstack/react-query";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
const daemon = vi.hoisted(() => ({ apiFetch: vi.fn(), apiText: vi.fn(), probeHealth: vi.fn() }));
vi.mock("../data/client", async (original) => ({
  ...(await original<typeof import("../data/client")>()),
  ...daemon,
}));

import { Autostart } from "./Autostart";
import { createAppQueryClient } from "../app/queryClient";
import { ApiRefusal } from "../data/client";
import type { AutostartView } from "../data/system";

function renderPanel() {
  return render(
    <QueryClientProvider client={createAppQueryClient()}>
      <Autostart />
    </QueryClientProvider>,
  );
}

const windowsOff: AutostartView = {
  mode: "at-login",
  at_boot: true,
  needs_password: true,
  why_not: null,
};

describe("Autostart", () => {
  it("asks for the password once, sends it in the PUT, and clears the field", async () => {
    let current = windowsOff;
    const puts: unknown[] = [];
    daemon.apiFetch.mockImplementation(async (path: string, init?: RequestInit) => {
      if (path !== "/system/autostart") throw new Error(`unexpected ${path}`);
      if (init?.method === "PUT") {
        puts.push(JSON.parse(String(init.body)));
        current = { ...current, mode: "at-boot" };
        return current;
      }
      return current;
    });
    renderPanel();

    const turnOn = await screen.findByRole("button", { name: "Turn on" });
    expect((turnOn as HTMLButtonElement).disabled).toBe(true);
    fireEvent.change(screen.getByLabelText("Windows password"), { target: { value: "s3cret" } });
    fireEvent.click(turnOn);

    await waitFor(() => {
      expect(puts).toEqual([{ mode: "at-boot", password: "s3cret" }]);
    });
    expect(await screen.findByRole("button", { name: "Turn off" })).toBeDefined();
    expect(screen.queryByDisplayValue("s3cret")).toBeNull();

    fireEvent.click(screen.getByRole("button", { name: "Turn off" }));
    await waitFor(() => {
      expect(puts[1]).toEqual({ mode: "at-login" });
    });
  });

  it("shows the system's own words when Windows refuses the account", async () => {
    daemon.apiFetch.mockImplementation(async (_path: string, init?: RequestInit) => {
      if (init?.method === "PUT") {
        throw new ApiRefusal(
          422,
          "system_refused",
          "ERROR: No mapping between account names and security IDs was done.",
        );
      }
      return windowsOff;
    });
    renderPanel();
    fireEvent.change(await screen.findByLabelText("Windows password"), {
      target: { value: "wrong" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Turn on" }));
    expect(await screen.findByText(/No mapping between account names/)).toBeDefined();
    expect(screen.queryByDisplayValue("wrong")).toBeNull();
  });

  it("explains macOS instead of offering a switch it cannot honour", async () => {
    daemon.apiFetch.mockResolvedValue({
      mode: "at-login",
      at_boot: false,
      needs_password: false,
      why_not: "macOS only starts a LaunchAgent at login. Turn on automatic login.",
    } satisfies AutostartView);
    renderPanel();
    expect(await screen.findByText(/Turn on automatic login/)).toBeDefined();
    expect(screen.queryByRole("button", { name: "Turn on" })).toBeNull();
  });

  it("turns on at once where no password is needed", async () => {
    const puts: unknown[] = [];
    daemon.apiFetch.mockImplementation(async (_path: string, init?: RequestInit) => {
      if (init?.method === "PUT") {
        puts.push(JSON.parse(String(init.body)));
        return { mode: "at-boot", at_boot: true, needs_password: false, why_not: null };
      }
      return { mode: "at-login", at_boot: true, needs_password: false, why_not: null };
    });
    renderPanel();
    fireEvent.click(await screen.findByRole("button", { name: "Turn on" }));
    await waitFor(() => {
      expect(puts).toEqual([{ mode: "at-boot" }]);
    });
    expect(screen.queryByLabelText("Windows password")).toBeNull();
  });
});
