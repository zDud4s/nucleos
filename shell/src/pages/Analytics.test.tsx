import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, screen, waitFor } from "@testing-library/react";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
const daemon = vi.hoisted(() => ({ apiFetch: vi.fn(), apiText: vi.fn(), probeHealth: vi.fn() }));
vi.mock("../data/client", async (original) => ({
  ...(await original<typeof import("../data/client")>()),
  ...daemon,
}));

import { daemonFetch, daemonState, daemonText, renderApp } from "../test/harness";

beforeEach(() => {
  const state = daemonState();
  daemon.apiFetch.mockReset();
  daemon.apiFetch.mockImplementation(daemonFetch(state));
  daemon.apiText.mockImplementation(daemonText(state));
  daemon.probeHealth.mockResolvedValue(true);
});

describe("the Analytics page", () => {
  it("opens on 30 days and keeps the chosen window in the address", async () => {
    const { router } = await renderApp({ initialPath: "/analytics" });

    expect(await screen.findByRole("heading", { level: 1, name: "Analytics" })).toBeTruthy();
    expect((await screen.findByRole("button", { name: "30 days" })).getAttribute("aria-pressed")).toBe("true");

    fireEvent.click(screen.getByRole("button", { name: "90 days" }));

    await waitFor(() => expect((router.state.location.search as { days?: number }).days).toBe(90));
    await waitFor(() =>
      expect(daemon.apiFetch.mock.calls.map(([path]) => String(path))).toContain("/analytics?days=90"),
    );
    expect(screen.getByRole("button", { name: "90 days" }).getAttribute("aria-pressed")).toBe("true");
  });

  it("is the global view: it has the quota section", async () => {
    await renderApp({ initialPath: "/analytics" });
    expect(await screen.findByRole("region", { name: "Quota" })).toBeTruthy();
  });
});
