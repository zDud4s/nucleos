import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, screen, waitFor, within } from "@testing-library/react";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
const daemon = vi.hoisted(() => ({ apiFetch: vi.fn(), apiText: vi.fn(), probeHealth: vi.fn() }));
vi.mock("../data/client", async (original) => ({
  ...(await original<typeof import("../data/client")>()),
  ...daemon,
}));

import { daemonFetch, daemonState, renderWithRouter, type DaemonState } from "../test/harness";
import type { ContextFitReport } from "../data/context-fit";
import { AnalyticsView } from "./AnalyticsView";

beforeEach(() => {
  daemon.apiFetch.mockReset();
});

/** Mount the whole view and pick the Context fit section; paths under a `failing` prefix throw. */
async function mount(state: DaemonState, failing: string[] = []) {
  const answer = daemonFetch(state);
  daemon.apiFetch.mockImplementation(async (path: string, init?: RequestInit) => {
    if (failing.some((prefix) => path.startsWith(prefix))) throw new Error("down");
    return answer(path, init);
  });
  await renderWithRouter(<AnalyticsView />);
  fireEvent.click(await screen.findByRole("button", { name: "Context fit" }));
}

const FIXTURE: ContextFitReport = {
  days: 7,
  nodes: 42,
  by_verdict: { under: 3, over: 5, split: 2, fit: 32 },
  by_layer: [{ layer: "core", under: 1, over: 2, split: 0, fit: 10 }],
  recent: [
    {
      node_id: "node-a",
      tree_id: "tree-1",
      layer: "core",
      verdicts: ["over"],
      evidence: [{ signal: "o2", value: 0.55, threshold: 0.4 }],
      given: {},
      used: {},
      rules_version: 1,
      computed_at: "2026-11-12T10:00:00Z",
    },
  ],
};

describe("ContextFitSection", () => {
  it("shows the verdict totals, the layer table and the evidence of a non-fit node", async () => {
    await mount(daemonState({ contextFit: FIXTURE }));
    const region = await screen.findByRole("region", { name: "Context fit" });
    await waitFor(() => expect(within(region).queryByText("Loading context fit")).toBeNull());
    expect(within(region).getByText("too little context")).toBeTruthy();
    expect(within(region).getByText("32")).toBeTruthy();
    const layers = within(region).getByRole("table", { name: "Context fit by layer" });
    expect(within(layers).getByText("core")).toBeTruthy();
    const recent = within(region).getByRole("table", { name: "Recent nodes that did not fit" });
    expect(within(recent).getByText("node-a")).toBeTruthy();
    expect(within(recent).getByText("Over")).toBeTruthy();
    expect(within(recent).getByText("o2 0.55 vs 0.4")).toBeTruthy();
  });

  it("says so when no node was measured", async () => {
    await mount(daemonState());
    expect(await screen.findByText("No tree nodes measured yet")).toBeTruthy();
  });

  it("is an error note with a retry when the route is down", async () => {
    await mount(daemonState(), ["/analytics/context-fit"]);
    expect(await screen.findByText(/Could not read context fit/)).toBeTruthy();
    expect(screen.getByRole("button", { name: "Try again" })).toBeTruthy();
  });
});
