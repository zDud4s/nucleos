import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, render } from "@testing-library/react";
import { AttentionHeartbeat } from "./AttentionHeartbeat";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

const daemon = vi.hoisted(() => ({ apiFetch: vi.fn(), apiText: vi.fn(), probeHealth: vi.fn() }));
vi.mock("../data/client", async (original) => ({
  ...(await original<typeof import("../data/client")>()),
  ...daemon,
}));

/**
 * jsdom has no window manager, so `visibilityState` is a fixed `visible`.
 * Redefining it is the only way to say "the window is in the tray".
 */
let visibility: DocumentVisibilityState = "visible";

function setVisibility(next: DocumentVisibilityState) {
  visibility = next;
  act(() => {
    document.dispatchEvent(new Event("visibilitychange"));
  });
}

beforeEach(() => {
  vi.useFakeTimers();
  daemon.apiFetch.mockReset();
  daemon.apiFetch.mockResolvedValue(undefined);
  visibility = "visible";
  Object.defineProperty(document, "visibilityState", {
    configurable: true,
    get: () => visibility,
  });
});

afterEach(() => {
  vi.useRealTimers();
});

/** How many heartbeats have reached the daemon. */
function beats(): number {
  return daemon.apiFetch.mock.calls.filter((call: unknown[]) => call[0] === "/autopilot/attention").length;
}

/**
 * The heartbeat is how the núcleo decides whether it may work unattended, so
 * the *silence* is the load-bearing half. A heartbeat that ignored visibility
 * would mark the owner present for as long as the app was running — which for
 * a tray app is always — and the night work this whole system exists to do
 * would never start again.
 */
describe("AttentionHeartbeat", () => {
  it("posts nothing at all while the window is hidden", () => {
    visibility = "hidden";
    render(<AttentionHeartbeat />);

    // Not on mount, and not on any tick after it. A window in the tray for eight
    // hours makes exactly zero requests.
    expect(beats()).toBe(0);
    act(() => {
      vi.advanceTimersByTime(120000);
    });
    expect(beats()).toBe(0);
  });

  it("posts the moment the window becomes visible again", () => {
    visibility = "hidden";
    render(<AttentionHeartbeat />);
    expect(beats()).toBe(0);

    setVisibility("visible");

    // Immediately, not up to thirty seconds later: the first thing somebody does
    // on coming back is expect the machine to have noticed.
    expect(beats()).toBe(1);
  });

  it("beats every thirty seconds while it can see someone", () => {
    render(<AttentionHeartbeat />);
    expect(beats()).toBe(1);

    act(() => {
      vi.advanceTimersByTime(30000);
    });
    expect(beats()).toBe(2);

    act(() => {
      vi.advanceTimersByTime(60000);
    });
    expect(beats()).toBe(4);
  });

  it("stops beating when the window is hidden again", () => {
    render(<AttentionHeartbeat />);
    expect(beats()).toBe(1);

    setVisibility("hidden");
    act(() => {
      vi.advanceTimersByTime(300000);
    });

    // The interval is torn down rather than skipped, so nothing is queued up to
    // fire in a burst when the window comes back.
    expect(beats()).toBe(1);
  });

  it("stops beating when it unmounts", () => {
    const { unmount } = render(<AttentionHeartbeat />);
    expect(beats()).toBe(1);

    unmount();
    act(() => {
      vi.advanceTimersByTime(120000);
    });

    expect(beats()).toBe(1);
  });

  it("posts to the attention route and nowhere else", () => {
    render(<AttentionHeartbeat />);

    const call = daemon.apiFetch.mock.calls[0] as [string, RequestInit];
    expect(call[0]).toBe("/autopilot/attention");
    expect(call[1].method).toBe("POST");
    // No project scope: this is the global "someone is here", and an empty
    // `project_id` is a 400 from the daemon.
    expect(call[1].body).toBe("{}");
  });
});
