import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, screen, waitFor } from "@testing-library/react";
import { KillSwitchControl } from "./KillSwitchControl";
import { daemonFetch, daemonState, renderWithQuery } from "../test/harness";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

const daemon = vi.hoisted(() => ({ apiFetch: vi.fn(), apiText: vi.fn(), probeHealth: vi.fn() }));
vi.mock("../data/client", async (original) => ({
  ...(await original<typeof import("../data/client")>()),
  ...daemon,
}));

beforeEach(() => {
  daemon.apiFetch.mockReset();
});

/** Every write the control made, in order, with the body it sent. */
function writes(): unknown[] {
  return daemon.apiFetch.mock.calls
    .filter((call: unknown[]) => (call[1] as RequestInit | undefined)?.method === "POST")
    .map((call: unknown[]) => JSON.parse(String((call[1] as RequestInit).body)) as unknown);
}

/**
 * Real timers, and one real wait.
 *
 * The interlock's 300 ms dwell is the only clock in this test, and faking time
 * here would also freeze react-query's — which is what actually delivers the
 * daemon's answers. A third of a second of suite time is the cheaper trade.
 */
function afterDwell(): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, 350));
}

describe("KillSwitchControl", () => {
  it("reads the state the daemon has, not one of its own", async () => {
    daemon.apiFetch.mockImplementation(daemonFetch(daemonState({ kill: { engaged: true } })));

    renderWithQuery(<KillSwitchControl />);

    expect(await screen.findByText(/kill switch engaged/i)).toBeDefined();
    expect(screen.getByRole("button", { name: /release kill switch/i })).toBeDefined();
  });

  it("engages on a single click, because panic is fast", async () => {
    daemon.apiFetch.mockImplementation(daemonFetch(daemonState({ kill: { engaged: false } })));

    renderWithQuery(<KillSwitchControl />);
    fireEvent.click(await screen.findByRole("button", { name: "Kill switch" }));

    // One click, one write. An interlock in front of *stopping* would be a UI
    // arguing with somebody who has just watched an agent do something wrong.
    await waitFor(() => {
      expect(writes()).toEqual([{ engaged: true }]);
    });
  });

  it("needs two clicks to release, because releasing restarts the machine", async () => {
    daemon.apiFetch.mockImplementation(daemonFetch(daemonState({ kill: { engaged: true } })));

    renderWithQuery(<KillSwitchControl />);
    fireEvent.click(await screen.findByRole("button", { name: /release kill switch/i }));

    // Armed, and nothing has moved yet.
    expect(writes()).toEqual([]);
    expect(screen.getByRole("button", { name: /really release/i })).toBeDefined();

    await afterDwell();
    fireEvent.click(screen.getByRole("button", { name: /really release/i }));

    await waitFor(() => {
      expect(writes()).toEqual([{ engaged: false }]);
    });
  });

  it("stays on screen while the state is unread", async () => {
    // The daemon is not answering this route. The control that stops everything
    // is exactly the control that must not disappear when things look wrong.
    daemon.apiFetch.mockRejectedValue(new Error("no answer"));

    renderWithQuery(<KillSwitchControl />);

    expect(screen.getByRole("button", { name: "Kill switch" })).toBeDefined();
    expect(await screen.findByText(/state unread/i)).toBeDefined();
  });

  it("says a refusal next to the button rather than anywhere else", async () => {
    const { ApiRefusal } = await import("../data/client");
    daemon.apiFetch.mockImplementation(async (path: string, init?: RequestInit) => {
      if (init?.method === "POST") throw new ApiRefusal(423, "kill_switch", "kill_switch");
      if (path === "/autopilot/kill") return { engaged: false };
      return undefined;
    });

    renderWithQuery(<KillSwitchControl />);
    fireEvent.click(await screen.findByRole("button", { name: "Kill switch" }));

    // Inline, under the control that caused it. There are no toasts in this app.
    expect(await screen.findByText(/kill switch is engaged/i)).toBeDefined();
  });
});
