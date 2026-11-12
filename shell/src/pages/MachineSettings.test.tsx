import { beforeEach, describe, expect, it, vi } from "vitest";
import { screen, within } from "@testing-library/react";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

const daemon = vi.hoisted(() => ({ apiFetch: vi.fn(), apiText: vi.fn(), probeHealth: vi.fn() }));
vi.mock("../data/client", async (original) => ({
  ...(await original<typeof import("../data/client")>()),
  ...daemon,
}));

import { MachineSettings } from "./MachineSettings";
import { daemonFetch, daemonState, renderWithRouter } from "../test/harness";
import { isSettingsPath, settingsDouble, type SeedDoc } from "../test/settings-double";

function machineDoc(name: string): SeedDoc {
  return {
    scope: "machine",
    name,
    schema: { type: "object", properties: { host: { type: "string" } } },
    takes_effect: "when the daemon restarts",
  };
}

/** The settings routes over the foundation's responder, as `System.test.tsx`'s `systemFetch` does. */
function machineFetch(names: string[]): (path: string, init?: RequestInit) => Promise<unknown> {
  const settings = settingsDouble(names.map(machineDoc));
  const shared = daemonFetch(daemonState());
  return async (path, init) => {
    if (isSettingsPath(path)) return settings.handle(path, init);
    if (path === "/config/secrets") return { secrets: [] };
    if (path === "/engines/agy") {
      return { enabled: false, acknowledged: false, warning: "Fixture warning", warning_version: 1, installed: true };
    }
    return shared(path, init);
  };
}

beforeEach(() => {
  daemon.apiFetch.mockReset();
  daemon.apiText.mockReset();
  daemon.probeHealth.mockReset();
});

describe("MachineSettings - the agy document", () => {
  it("leaves the agy document out of the settings list, its own panel holds the warning", async () => {
    // agy is served first on purpose: the first listed document is the one open from the start.
    daemon.apiFetch.mockImplementation(machineFetch(["agy", "email"]));

    await renderWithRouter(<MachineSettings />);

    const list = await screen.findByRole("list", { name: "Settings documents" });
    expect(within(list).getByRole("button", { name: /^email\b/ })).toBeDefined();
    expect(within(list).queryByRole("button", { name: /^agy\b/ })).toBeNull();
    expect(await screen.findByRole("heading", { name: "email" })).toBeDefined();
    expect(screen.queryByRole("heading", { name: "agy" })).toBeNull();

    // The one way left to enable it: the switch whose dialog shows the daemon's warning.
    const engines = screen.getByRole("heading", { name: "Engines" }).closest("section") as HTMLElement;
    expect(await within(engines).findByRole("switch")).toBeDefined();
  });
});
