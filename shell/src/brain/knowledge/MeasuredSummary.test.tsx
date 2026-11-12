import { beforeEach, describe, expect, it, vi } from "vitest";
import { within } from "@testing-library/react";
import { daemonWith, known, panelFor, renderMeasured } from "./test-helpers";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

const daemon = vi.hoisted(() => ({ apiFetch: vi.fn(), apiText: vi.fn(), probeHealth: vi.fn() }));
vi.mock("../../data/client", async (original) => ({
  ...(await original<typeof import("../../data/client")>()),
  ...daemon,
}));

beforeEach(() => {
  daemon.apiFetch.mockReset();
});

describe("MeasuredSummary", () => {
  it("counts measured rows per project and per generator", async () => {
    daemon.apiFetch.mockImplementation(
      daemonWith([
        known({ id: 1, layer: "episodic", source: "consolidator", scope_id: "alpha", generator: "gate" }),
        known({ id: 2, layer: "episodic", source: "consolidator", scope_id: "alpha", generator: "gate" }),
        known({ id: 3, layer: "episodic", source: "consolidator", scope_id: "alpha", generator: "refused-action" }),
        known({ id: 4, layer: "episodic", source: "consolidator", scope_kind: "machine", scope_id: null, generator: null }),
      ]),
    );

    await renderMeasured();
    const panel = await panelFor("Measured, by generator");
    const project = within(panel).getByText("alpha").closest(".learned-measured-row");
    const machine = within(panel).getByText("this machine").closest(".learned-measured-row");

    expect(project).not.toBeNull();
    expect(within(project as HTMLElement).getByText("gate: 2")).toBeDefined();
    expect(within(project as HTMLElement).getByText("refused-action: 1")).toBeDefined();
    expect(machine).not.toBeNull();
    expect(within(machine as HTMLElement).getByText("unknown: 1")).toBeDefined();
  });
});
