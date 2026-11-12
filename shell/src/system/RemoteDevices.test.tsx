import { describe, expect, it, vi } from "vitest";
import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { QueryClientProvider } from "@tanstack/react-query";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
const opener = vi.hoisted(() => ({ openUrl: vi.fn() }));
vi.mock("@tauri-apps/plugin-opener", () => opener);
const daemon = vi.hoisted(() => ({ apiFetch: vi.fn(), apiText: vi.fn(), probeHealth: vi.fn() }));
vi.mock("../data/client", async (original) => ({
  ...(await original<typeof import("../data/client")>()),
  ...daemon,
}));

import { RemoteDevices } from "./RemoteDevices";
import { createAppQueryClient } from "../app/queryClient";
import { ApiRefusal } from "../data/client";
import type { RemoteDevice, RemoteSetup } from "../data/remote";

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
            owner: false,
          },
        ];
        return {
          code: "ABCDE-FGHJK",
          access_level: body.access_level,
          device_name: body.device_name,
          expires_at: "2026-10-29T09:05:00Z",
          owner: false,
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

  it("pairs an Owner device as an admin code with the owner flag", async () => {
    const posted: unknown[] = [];
    daemon.apiFetch.mockImplementation(async (path: string, init?: RequestInit) => {
      if (path === "/remote/pairing-codes" && init?.method === "POST") {
        const body = JSON.parse(String(init.body)) as { device_name: string };
        posted.push(body);
        return {
          code: "ABCDE-FGHJK",
          access_level: "admin",
          device_name: body.device_name,
          expires_at: "2026-10-29T09:05:00Z",
          owner: true,
        };
      }
      if (path === "/remote/devices") return [];
      throw new Error(`unexpected ${path}`);
    });
    renderPanel();
    fireEvent.change(await screen.findByLabelText("Device name"), { target: { value: "Laptop" } });
    fireEvent.change(screen.getByLabelText("Access level"), { target: { value: "owner" } });
    expect(screen.getByRole("option", { name: "Owner" })).toBeDefined();
    fireEvent.click(screen.getByRole("button", { name: "Create code" }));
    await screen.findByLabelText("the pairing code for Laptop");
    expect(posted).toEqual([{ device_name: "Laptop", access_level: "admin", owner: true }]);
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
      await screen.findByText(/Remote access is switched off\. Turn it on above/),
    ).toBeDefined();
  });
  function setupOf(
    over: Partial<Omit<RemoteSetup, "tailscale">> & { tailscale?: Partial<RemoteSetup["tailscale"]> },
  ): RemoteSetup {
    return {
      enabled: false,
      public_origin: "",
      serving: false,
      funnel: false,
      port_taken: false,
      ...over,
      tailscale: {
        installed: true,
        signed_in: true,
        dns_name: "mybox.tail1234.ts.net",
        https_ok: true,
        ...over.tailscale,
      },
    };
  }

  it("the_wizard_points_at_the_admin_console_when_https_is_off", async () => {
    daemon.apiFetch.mockImplementation(async (path: string) => {
      if (path === "/remote/setup") return setupOf({ tailscale: { https_ok: false } });
      if (path === "/remote/devices") return [];
      throw new Error(`unexpected ${path}`);
    });
    renderPanel();
    expect(await screen.findByText(/HTTPS\s+certificates switched off/)).toBeDefined();
    expect(screen.getByRole("button", { name: "Open the Tailscale admin console" })).toBeDefined();
    // Nothing to turn on yet, and no platform is named as a prerequisite.
    expect(screen.queryByRole("button", { name: "Turn on remote access" })).toBeNull();
    expect(document.body.textContent ?? "").not.toMatch(/Windows 11/);
  });

  it("the_wizard_shows_a_code_and_qr_when_ready", async () => {
    let setup = setupOf({});
    const posts: unknown[] = [];
    daemon.apiFetch.mockImplementation(async (path: string, init?: RequestInit) => {
      if (path === "/remote/setup" && init?.method === "POST") {
        posts.push(JSON.parse(String(init.body)));
        setup = setupOf({
          enabled: true,
          public_origin: "https://mybox.tail1234.ts.net",
          serving: true,
        });
        return setup;
      }
      if (path === "/remote/setup") return setup;
      if (path === "/remote/devices") return [];
      if (path === "/remote/pairing-codes") {
        return {
          code: "ABCDE-FGHJK",
          access_level: "admin",
          device_name: "Phone",
          expires_at: "2026-10-29T09:05:00Z",
          owner: false,
        };
      }
      throw new Error(`unexpected ${path}`);
    });
    renderPanel();
    fireEvent.click(await screen.findByRole("button", { name: "Turn on remote access" }));
    await screen.findByText(/Remote access is on at/);
    expect(posts).toEqual([{}]);

    fireEvent.change(screen.getByLabelText("Device name"), { target: { value: "Phone" } });
    fireEvent.click(screen.getByRole("button", { name: "Create code" }));
    const field = await screen.findByLabelText("the pairing code for Phone");
    expect((field as HTMLInputElement).value).toBe("ABCDE-FGHJK");
    const qr = screen.getByRole("img", { name: "QR code for https://mybox.tail1234.ts.net" });
    expect(qr.querySelector("path")?.getAttribute("d")?.length ?? 0).toBeGreaterThan(100);
  });

  it("shows_the_tailscale_approval_link_and_lets_the_person_retry", async () => {
    const link = "https://login.tailscale.com/f/serve?node=n123";
    let attempts = 0;
    daemon.apiFetch.mockImplementation(async (path: string, init?: RequestInit) => {
      if (path === "/remote/setup" && init?.method === "POST") {
        attempts += 1;
        throw new ApiRefusal(409, "serve_needs_approval", link);
      }
      if (path === "/remote/setup") return setupOf({});
      if (path === "/remote/devices") return [];
      throw new Error(`unexpected ${path}`);
    });
    renderPanel();
    fireEvent.click(await screen.findByRole("button", { name: "Turn on remote access" }));
    expect(await screen.findByText(/needs you to approve Serve/)).toBeDefined();
    fireEvent.click(screen.getByRole("button", { name: "Open the Tailscale approval page" }));
    await waitFor(() => {
      expect(opener.openUrl).toHaveBeenCalledWith(link);
    });
    // The same button is still there to retry once it is approved.
    fireEvent.click(screen.getByRole("button", { name: "Turn on remote access" }));
    await waitFor(() => {
      expect(attempts).toBe(2);
    });
  });

  it("refuses_to_offer_turning_on_while_the_port_is_public_or_taken", async () => {
    daemon.apiFetch.mockImplementation(async (path: string) => {
      if (path === "/remote/setup") return setupOf({ funnel: true });
      if (path === "/remote/devices") return [];
      throw new Error(`unexpected ${path}`);
    });
    const { unmount } = renderPanel();
    expect(await screen.findByText(/open to the public internet/)).toBeDefined();
    const button = screen.getByRole("button", { name: "Turn on remote access" });
    expect((button as HTMLButtonElement).disabled).toBe(true);
    unmount();

    daemon.apiFetch.mockImplementation(async (path: string) => {
      if (path === "/remote/setup") return setupOf({ port_taken: true });
      if (path === "/remote/devices") return [];
      throw new Error(`unexpected ${path}`);
    });
    renderPanel();
    expect(await screen.findByText(/already serving something else/)).toBeDefined();
    const taken = screen.getByRole("button", { name: "Turn on remote access" });
    expect((taken as HTMLButtonElement).disabled).toBe(true);
  });
});
