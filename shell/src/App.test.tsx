import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, render, screen } from "@testing-library/react";

import App from "./App";
import { invoke } from "@tauri-apps/api/core";

// The credential-manager read is the one thing here that is not HTTP.
vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

const invokeMock = vi.mocked(invoke);
const fetchMock = vi.fn();
vi.stubGlobal("fetch", fetchMock);

/** A daemon that is up, healthy, and happy with the token. */
function healthyDaemon(overrides: Record<string, unknown> = {}) {
  return async (url: string) => {
    for (const [suffix, response] of Object.entries(overrides)) {
      if (url.endsWith(suffix)) return response;
    }
    if (url.endsWith("/health")) return { ok: true, status: 200, text: async () => "ok" };
    if (url.endsWith("/status")) return { ok: true, status: 200, text: async () => "idle" };
    if (url.endsWith("/autopilot/kill")) return { ok: true, status: 200, json: async () => ({ engaged: false }) };
    if (url.endsWith("/autopilot/budget")) {
      return {
        ok: true,
        status: 200,
        json: async () => ({
          limit_usd: null, period: "daily", hourly_limit_usd: null,
          per_run_reserve_usd: 0.5, time_cost_per_hour_usd: 0,
          window_spend_usd: 0, hourly_spend_usd: 0, paused: false, reason: null,
        }),
      };
    }
    return { ok: true, status: 200, json: async () => [] };
  };
}

/** Advances the poll clock and lets every promise it started settle. */
async function tick(ms = 3000) {
  await act(async () => {
    vi.advanceTimersByTime(ms);
  });
}

async function settle() {
  await act(async () => {});
}

describe("App connection handshake", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    fetchMock.mockImplementation(healthyDaemon());
    invokeMock.mockResolvedValue("daemon-token");
  });
  afterEach(() => {
    vi.useRealTimers();
    fetchMock.mockReset();
    invokeMock.mockReset();
  });

  it("retries a failed credential read instead of remembering the failure forever", async () => {
    // The keychain is commonly locked for a moment right after login. Caching
    // the promise made that one failure permanent for the whole session.
    invokeMock
      .mockRejectedValueOnce("No matching entry found in secure storage")
      .mockResolvedValue("daemon-token");

    render(<App />);
    await settle();

    expect(screen.getByText(/No matching entry found in secure storage/)).toBeTruthy();
    expect(screen.queryByText("daemon connected")).toBeNull();

    await tick();

    expect(invokeMock).toHaveBeenCalledTimes(2);
    expect(screen.getByText("daemon connected")).toBeTruthy();
    expect(screen.queryByText(/No matching entry found/)).toBeNull();
  });

  it("says the daemon refused the token rather than pretending it is connected", async () => {
    fetchMock.mockImplementation(
      healthyDaemon({ "/status": { ok: false, status: 401, text: async () => "" } }),
    );

    render(<App />);
    await settle();

    // Reachable but rejected: retrying forever cannot fix a stale token, so
    // the header must not keep claiming everything is fine.
    expect(screen.queryByText("daemon connected")).toBeNull();
    expect(screen.getByText(/rejected the stored token/)).toBeTruthy();
  });

  it("recovers once the daemon accepts the token again", async () => {
    let authorised = false;
    fetchMock.mockImplementation(async (url: string) => {
      if (url.endsWith("/status")) {
        return authorised
          ? { ok: true, status: 200, text: async () => "idle" }
          : { ok: false, status: 401, text: async () => "" };
      }
      return healthyDaemon()(url);
    });

    render(<App />);
    await settle();
    expect(screen.getByText(/rejected the stored token/)).toBeTruthy();

    authorised = true;
    await tick();

    expect(screen.getByText("daemon connected")).toBeTruthy();
    // A refusal drops the cached token so the next round re-reads the
    // credential manager: the daemon may have rotated it.
    expect(invokeMock).toHaveBeenCalledTimes(2);
  });

  it("never stacks a second poll on top of one still in flight", async () => {
    let releaseHealth: ((value: unknown) => void) | null = null;
    fetchMock.mockImplementation(async (url: string) => {
      if (url.endsWith("/health")) {
        return new Promise((resolve) => {
          releaseHealth = () => resolve({ ok: true, status: 200, text: async () => "ok" });
        });
      }
      return healthyDaemon()(url);
    });

    render(<App />);
    await settle();
    await tick(9000);

    // Three ticks passed with the first request unanswered; a slow daemon must
    // not accumulate rounds whose answers then land out of order.
    expect(fetchMock).toHaveBeenCalledTimes(1);

    await act(async () => {
      releaseHealth?.(undefined);
    });
    await tick();
    expect(fetchMock.mock.calls.filter(([url]) => String(url).endsWith("/health")).length).toBe(2);
  });
});
