import { describe, expect, it, vi } from "vitest";
import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { QueryClientProvider } from "@tanstack/react-query";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
const daemon = vi.hoisted(() => ({ apiFetch: vi.fn(), apiText: vi.fn(), probeHealth: vi.fn() }));
vi.mock("../data/client", async (original) => ({
  ...(await original<typeof import("../data/client")>()),
  ...daemon,
}));

import { RemoteDevices } from "./RemoteDevices";
import { createAppQueryClient } from "../app/queryClient";
import { ApiRefusal } from "../data/client";
import type { RemoteDevice } from "../data/remote";

/** Past `ConfirmButton`'s dwell, so the second press is a decision and not a double-click. */
async function pastTheDwell(): Promise<void> {
  await act(async () => {
    await new Promise((resolve) => setTimeout(resolve, 350));
  });
}

function renderPanel() {
  const queryClient = createAppQueryClient();
  return render(
    <QueryClientProvider client={queryClient}>
      <RemoteDevices />
    </QueryClientProvider>,
  );
}

describe("RemoteDevices", () => {
  it("creates a code, shows it once, lists devices and revokes one", async () => {
    let devices: RemoteDevice[] = [];
    const posted: unknown[] = [];
    const deleted: string[] = [];
    daemon.apiFetch.mockImplementation(async (path: string, init?: RequestInit) => {
      if (path === "/remote/pairing-codes" && init?.method === "POST") {
        const body = JSON.parse(String(init.body)) as { device_name: string; access_level: string };
        posted.push(body);
        devices = [
          ...devices,
          {
            id: 7,
            name: body.device_name,
            access_level: "admin",
            created_at: "2026-10-29T09:00:00Z",
            last_seen_at: null,
            revoked_at: null,
          },
        ];
        return {
          code: "ABCDE-FGHJK",
          access_level: body.access_level,
          device_name: body.device_name,
          expires_at: "2026-10-29T09:05:00Z",
        };
      }
      if (path === "/remote/devices" && init?.method === undefined) return devices;
      if (path === "/remote/devices/7" && init?.method === "DELETE") {
        deleted.push(path);
        devices = devices.map((d) => ({ ...d, revoked_at: "2026-10-29T09:10:00Z" }));
        return undefined;
      }
      throw new Error(`unexpected ${path}`);
    });

    renderPanel();
    expect(await screen.findByText("no device has been paired.")).toBeDefined();

    fireEvent.change(screen.getByLabelText("Device name"), { target: { value: "Phone" } });
    fireEvent.click(screen.getByRole("button", { name: "Create code" }));

    const field = await screen.findByLabelText("the pairing code for Phone");
    expect((field as HTMLInputElement).value).toBe("ABCDE-FGHJK");
    expect(posted).toEqual([{ device_name: "Phone", access_level: "admin" }]);
    expect(screen.getByText(/Valid 5 minutes/)).toBeDefined();

    // The listing shows the device, never the code.
    const row = await screen.findByText("Phone", { selector: ".sy-token-name" });
    expect(row.closest("li, [role=listitem]")?.textContent ?? "").not.toMatch(/ABCDE/);

    fireEvent.click(screen.getByRole("button", { name: "Dismiss" }));
    expect(screen.queryByLabelText("the pairing code for Phone")).toBeNull();

    fireEvent.click(screen.getByRole("button", { name: /Revoke/ }));
    await pastTheDwell();
    fireEvent.click(screen.getByRole("button", { name: /really revoke/i }));
    await waitFor(() => {
      expect(deleted).toEqual(["/remote/devices/7"]);
    });
    expect(await screen.findByText(/revoked/)).toBeDefined();
  });

  it("says remote access is switched off on a 409", async () => {
    daemon.apiFetch.mockImplementation(async (path: string, init?: RequestInit) => {
      if (init?.method === "POST") throw new ApiRefusal(409, "remote_disabled", "");
      if (path === "/remote/devices") return [];
      throw new Error(`unexpected ${path}`);
    });
    renderPanel();
    fireEvent.change(await screen.findByLabelText("Device name"), { target: { value: "Phone" } });
    fireEvent.click(screen.getByRole("button", { name: "Create code" }));
    expect(
      await screen.findByText("Remote access is switched off in ~/.nucleos/remote.yaml"),
    ).toBeDefined();
  });
});
