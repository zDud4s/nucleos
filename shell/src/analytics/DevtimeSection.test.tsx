import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, screen, waitFor, within } from "@testing-library/react";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
const daemon = vi.hoisted(() => ({ apiFetch: vi.fn(), apiText: vi.fn(), probeHealth: vi.fn() }));
vi.mock("../data/client", async (original) => ({ ...(await original<typeof import("../data/client")>()), ...daemon }));

import { daemonFetch, daemonState, devtimeFindings, devtimeOverview, project, renderWithRouter, type DaemonState } from "../test/harness";
import { DevtimeSection } from "./DevtimeSection";

const MIN = 60_000;

function mount(state: DaemonState, projectId?: string, failing: Set<string> = new Set()) {
  const answer = daemonFetch(state);
  daemon.apiFetch.mockImplementation(async (path: string, init?: RequestInit) => {
    for (const prefix of failing) if (path.startsWith(prefix)) throw new Error(`${prefix} is down`);
    return answer(path, init);
  });
  return renderWithRouter(<DevtimeSection projectId={projectId} days={30} />);
}

/** A month with active time, waste, turns, priced and unpriced tokens, and a fresh ingest. */
function busy(): DaemonState {
  const report = devtimeOverview();
  const last = report.days_series.length - 1;
  report.days_series[last] = { ...report.days_series[last], active_ms: 100 * MIN, wasted_ms: 20 * MIN,
    by_waste: { useful: 80 * MIN, rework: 15 * MIN, avoidable: 5 * MIN },
    by_kind: { model: 40 * MIN, tool: 60 * MIN, subagent: 0, wait_background: 0, wait_machine: 0 },
    turns: 10, interrupted: 2, corrections: 1, cost_usd: 4.2 };
  report.totals = { ...report.totals, active_ms: 100 * MIN, wasted_ms: 20 * MIN, waste_ratio: 0.2, turns: 10,
    interruption_rate: 0.2, correction_rate: 0.25, correction_coverage: 0.4,
    previous: { active_ms: 50 * MIN, wasted_ms: 12 * MIN, waste_ratio: 0.24, interruption_rate: 0.3, correction_rate: null } };
  report.quality = { ...report.quality, ingest_enabled: true, last_cycle_at: new Date(Date.now() - 60_000).toISOString(), sessions: 5 };
  report.tokens = { ...report.tokens, cost_usd: 4.2, prices_as_of: "2026-10-17", unpriced_models: ["sonnet"],
    dropped_prices: ["claude-haiku-5-5: input is not a finite, non-negative number"],
    by_model: [
      { model: "claude-opus-4-7", effort: "high", input: 1000, cache_read: 9000, cache_creation: 0, output: 500, messages: 3, cost_usd: 4.2 },
      { model: "sonnet", effort: "unknown", input: 10, cache_read: 0, cache_creation: 0, output: 5, messages: 1, cost_usd: null },
    ] };
  return daemonState({ devtime: report, devtimeFindings: devtimeFindings() });
}

beforeEach(() => {
  daemon.apiFetch.mockReset();
});

describe("DevtimeSection", () => {
  it("shows skeleton cards while reading", async () => {
    daemon.apiFetch.mockImplementation(() => new Promise(() => {}));
    await renderWithRouter(<DevtimeSection days={30} />);
    expect(screen.getByRole("region", { name: "Development time" })).toBeTruthy();
    expect(screen.getAllByText("reading…").length).toBeGreaterThan(0);
  });

  it("says there is nothing yet, and why, when the window is empty", async () => {
    await mount(daemonState());
    expect(await screen.findByText("No sessions read in this window yet")).toBeTruthy();
    expect(screen.getByText(/Never read/)).toBeTruthy();
  });

  it("fails on its own: an error note with a retry, and F1's section is not its business", async () => {
    await mount(busy(), undefined, new Set(["/analytics/devtime"]));
    expect(await screen.findByText(/The núcleo did not answer for development time\./)).toBeTruthy();
    expect(screen.getByRole("button", { name: "Try again" })).toBeTruthy();
  });

  it("leads with four cards, the cost one badged estimated, with deltas in words", async () => {
    await mount(busy());
    const cost = await screen.findByRole("button", { name: /Estimated cost/ });
    expect(within(cost).getByText("estimated")).toBeTruthy();
    expect(screen.getByRole("button", { name: /Active time/ })).toBeTruthy();
    expect(screen.getByRole("button", { name: /Waste/ }).textContent).toContain("4 points less than the previous 30 days");
    expect(screen.getByRole("button", { name: /Interruptions/ })).toBeTruthy();
  });

  it("draws the daily cost as the cost card's sparkline", async () => {
    await mount(busy());
    const card = (await screen.findByRole("button", { name: /Estimated cost/ })).closest(".an-metric");
    expect(card?.querySelector("svg.ui-chart-cost")).toBeTruthy(); // the sparkline sits beside the button
  });

  it("draws no cost sparkline when no day is priced", async () => {
    const unpriced = busy();
    unpriced.devtime.days_series = unpriced.devtime.days_series.map((day) => ({ ...day, cost_usd: null }));
    await mount(unpriced);
    const bare = (await screen.findByRole("button", { name: /Estimated cost/ })).closest(".an-metric");
    expect(bare?.querySelector("svg")).toBeNull();
  });

  it("turns the quality strip amber when the last read is older than five cycles", async () => {
    const state = busy();
    state.devtime.quality.last_cycle_at = new Date(Date.now() - 10 * 60_000).toISOString();
    await mount(state);
    const strip = await screen.findByText(/Last read 10 min ago/);
    expect(strip.closest(".an-dt-quality")?.classList.contains("an-dt-quality-amber")).toBe(true);
  });

  it("opens active time by waste and switches it to kind", async () => {
    await mount(busy());
    const panel = await screen.findByRole("region", { name: "Active time per day" });
    expect(within(panel).getByRole("img", { name: /per day by waste/ })).toBeTruthy();
    fireEvent.click(within(panel).getByRole("button", { name: "By kind" }));
    expect(within(panel).getByRole("img", { name: /per day by kind/ })).toBeTruthy();
  });

  it("lists waste by rule and by lever, top eight each, with reserved keys named", async () => {
    const state = busy();
    state.devtime.waste_by_rule = [{ rule_id: "__none__", level: null, waste: null, lever: null, ms: 5 * MIN, findings: 0 }];
    state.devtime.waste_by_lever = [{ lever: "spec_gap", ms: 5 * MIN }];
    await mount(state);
    fireEvent.click(await screen.findByRole("button", { name: /Waste/ }));
    const panel = screen.getByRole("region", { name: "Waste per day" });
    expect(within(panel).getByText("No rule")).toBeTruthy();
    expect(within(panel).getByText("spec_gap")).toBeTruthy();
  });

  it("tabulates tokens per model, marks the unpriced one, and footnotes the price date", async () => {
    await mount(busy());
    fireEvent.click(await screen.findByRole("button", { name: /Estimated cost/ }));
    const table = screen.getByRole("table", { name: "Tokens by model" });
    const sonnet = within(table).getByRole("row", { name: /sonnet/ });
    expect(within(sonnet).getByText("no price")).toBeTruthy();
    expect(screen.getByText("Cost does not include 1 model without a price.")).toBeTruthy();
    expect(screen.getByText(
      "Unreadable prices ignored in nucleos-models.yaml: claude-haiku-5-5: input is not a finite, non-negative number.",
    )).toBeTruthy();
    expect(screen.getByText("Estimated, prices of 2026-10-17. Never compared with an invoice.")).toBeTruthy();
  });

  it("keeps today's segment partial in what it hands the chart", async () => {
    await mount(busy());
    const panel = await screen.findByRole("region", { name: "Active time per day" });
    await waitFor(() => expect(panel.querySelector(".ui-area-partial")).not.toBeNull());
  });

  it("asks for findings only once the disclosure opens", async () => {
    await mount(busy());
    await screen.findByRole("button", { name: /Active time/ });
    expect(daemon.apiFetch.mock.calls.some(([path]) => String(path).includes("/findings"))).toBe(false);
    fireEvent.click(screen.getByText("Findings"));
    expect(await screen.findByText("few cases (3)")).toBeTruthy();
  });

  it("badges a low-precision rule and notes global precision in a project's view", async () => {
    const state = busy();
    state.projects = [project({ project_id: "nucleos" })];
    const rule = { ...state.devtimeFindings.items[0].rule!, n_cases: 25, precision: 0.6, low_precision: true };
    state.devtimeFindings.items[0].rule = rule;
    await mount(state, "nucleos");
    fireEvent.click(await screen.findByText("Findings"));
    expect(await screen.findByText("unreliable rule")).toBeTruthy();
    expect(screen.getByText("60% in 25 cases · global, all projects")).toBeTruthy();
  });

  it("marks optimistically and reverts with an error when the POST fails", async () => {
    const state = busy();
    state.devtimeMarkRefusal = { status: 500, code: "failed", detail: "analytics failed" };
    await mount(state);
    fireEvent.click(await screen.findByText("Findings"));
    const row = await screen.findByRole("listitem", { name: /A1/ });
    fireEvent.click(within(row).getByRole("button", { name: "Not rework" }));
    expect(within(row).getByRole("button", { name: "Not rework" }).getAttribute("aria-pressed")).toBe("true");
    await waitFor(() =>
      expect(within(row).getByRole("button", { name: "Not rework" }).getAttribute("aria-pressed")).toBe("false"),
    );
    expect(within(row).getByText("The mark was not saved.")).toBeTruthy();
  });

  it("keeps a successful mark and sends it once", async () => {
    const state = busy();
    await mount(state);
    fireEvent.click(await screen.findByText("Findings"));
    const row = await screen.findByRole("listitem", { name: /A1/ });
    fireEvent.click(within(row).getByRole("button", { name: "Confirm" }));
    await waitFor(() => expect(state.devtimeMarks).toEqual([{ findingKey: "f-1", verdict: "confirmed", cause: null }]));
    expect(within(row).getByRole("button", { name: "Confirm" }).getAttribute("aria-pressed")).toBe("true");
  });
});
