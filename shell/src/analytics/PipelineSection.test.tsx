import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, screen, waitFor, within } from "@testing-library/react";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
const daemon = vi.hoisted(() => ({ apiFetch: vi.fn(), apiText: vi.fn(), probeHealth: vi.fn() }));
vi.mock("../data/client", async (original) => ({
  ...(await original<typeof import("../data/client")>()),
  ...daemon,
}));

import { daemonFetch, daemonState, pipelineReport, project, renderWithRouter, type DaemonState } from "../test/harness";
import type { Gate, Jobs, Landing, Postgate, Pressure } from "../data/analytics-pipeline";
import { AnalyticsView } from "./AnalyticsView";

beforeEach(() => {
  daemon.apiFetch.mockReset();
});

/** Mount the whole view (the section is mounted in it); any path starting with a `failing` prefix throws. */
function mount(state: DaemonState, failing: Set<string> = new Set()) {
  const answer = daemonFetch(state);
  daemon.apiFetch.mockImplementation(async (path: string, init?: RequestInit) => {
    for (const prefix of failing) {
      if (path.startsWith(prefix)) throw new Error(`${prefix} is down`);
    }
    return answer(path, init);
  });
  return renderWithRouter(<AnalyticsView />);
}

const withPipeline = (analyticsPipeline: ReturnType<typeof pipelineReport>) =>
  daemonState({ projects: [project({ project_id: "nucleos" })], analyticsPipeline });

/** The section, once its report has been read. */
async function section(): Promise<HTMLElement> {
  const region = await screen.findByRole("region", { name: "Pipeline" });
  await waitFor(() => expect(within(region).queryByText("reading…")).toBeNull());
  return region;
}

function card(label: string): HTMLElement {
  return within(screen.getByRole("region", { name: "Pipeline" })).getByRole("button", { name: new RegExp(`^${label}`) });
}

const redReport = () =>
  pipelineReport({
    now: "2026-10-18T09:41:00Z",
    postgate: {
      ...(pipelineReport().postgate as Postgate),
      red_now: true,
      red_since: "2026-10-18T08:11:00Z",
      red_phase: "bisect",
      red_groups: ["core"],
      per_project: [{ project_id: "nucleos", red_now: true, red_s: 5400 }],
    },
  });

/** A month with something in every block. */
function busyReport() {
  const base = pipelineReport();
  const landing = base.landing as Landing;
  const gate = base.gate as Gate;
  const jobs = base.jobs as Jobs;
  return pipelineReport({
    landing: {
      ...landing,
      total: 8,
      bad_share: 0.25,
      status_mix: { ...landing.status_mix, succeeded: 6, failed: 2 },
      failure_buckets: { ...landing.failure_buckets, conflict: 1, gate: 1 },
      wait_s: { p50: 600, p95: null, n: 8 },
      wait_s_machine: { p50: 12, p95: null, n: 8 },
      daily: landing.daily.map((day, index) => ({ ...day, total: index % 3 })),
    },
    gate: {
      ...gate,
      terminal: 10,
      pass_rate: 0.9,
      first_pass_rate: 0.7,
      most_red: [{ group: "core", red: 3, terminal: 10, rate: 0.3 }],
      flaky: [{ project_id: "nucleos", group: "shell", observed: 10, flaky: 3, rate: 0.3, suspect: true }],
    },
    jobs: {
      ...jobs,
      total: 4,
      completed: 3,
      completion_rate: 0.75,
      stuck: { ...jobs.stuck, item_stuck: 1, jobs: [{ id: 21, project_id: "nucleos" }] },
    },
  });
}

describe("PipelineSection", () => {
  it("renders each empty state without zeros", async () => {
    const { container } = await mount(withPipeline(pipelineReport()));
    const pipeline = await section();
    expect(within(pipeline).getAllByText("No activity in this window").length).toBeGreaterThan(0);
    expect(within(pipeline).getAllByText("no data").length).toBeGreaterThan(0);
    expect(screen.queryByText("0%")).toBeNull();
    expect(container.textContent).not.toMatch(/NaN/);
  });

  it("says since when the post-merge gate is recorded, in its panel", async () => {
    const report = pipelineReport();
    const state = withPipeline({
      ...report,
      history: { postgate_since: `${report.window.to}T03:00:00Z`, slots_since: null },
    });
    await mount(state);
    await section();
    fireEvent.click(card("Post-merge gate"));
    expect(screen.getByText(`Recording since ${report.window.to}. No data for earlier periods yet.`)).toBeTruthy();
  });

  it("shows red now with text and not colour only", async () => {
    await mount(withPipeline(redReport()));
    await section();
    const red = card("Post-merge gate");
    expect(within(red).getByText("Red")).toBeTruthy();
    expect(within(red).getByText("Red for 1 h 30 (phase: bisect)")).toBeTruthy();
    expect(within(red).getByRole("img", { name: "Red" })).toBeTruthy();
    expect(red.closest(".an-metric")?.classList.contains("an-metric-alert")).toBe(true);
    // Red opens first, and its project is a way into the state view.
    expect(red.getAttribute("aria-expanded")).toBe("true");
    const link = screen.getByRole("link", { name: "nucleos" });
    expect(link.getAttribute("href")).toBe("/projects/nucleos/state");
  });

  it("a block error shows the card error and leaves the other blocks", async () => {
    await mount(withPipeline({ ...busyReport(), gate: { error: "query_failed" } }));
    await section();
    const gate = card("Gate");
    expect(within(gate).getByText("Could not compute")).toBeTruthy();
    expect(within(gate).queryByText(/first try|no data/)).toBeNull();
    expect(card("Landing queue").textContent).toContain("8");
    expect(card("Jobs").textContent).toContain("75%");
    fireEvent.click(gate);
    expect(screen.getByText("Could not compute this block.")).toBeTruthy();
  });

  it("null ratios read no data", async () => {
    await mount(withPipeline(pipelineReport()));
    await section();
    expect(within(card("Gate")).getByText("no data")).toBeTruthy();
    expect(within(card("Jobs")).getByText("no data")).toBeTruthy();
  });

  it("says once that the núcleo did not answer for the pipeline", async () => {
    await mount(withPipeline(busyReport()), new Set(["/analytics/pipeline"]));
    expect(await screen.findByText("The núcleo did not answer for the pipeline.")).toBeTruthy();
    expect(screen.getAllByText("The núcleo did not answer for the pipeline.")).toHaveLength(1);
    // F1's readout above it is not taken down.
    expect(await screen.findByRole("region", { name: "Spend per day" })).toBeTruthy();
  });

  it("opens the panel of the card clicked, one at a time, and lists what each holds", async () => {
    await mount(withPipeline(busyReport()));
    await section();
    expect(card("Landing queue").getAttribute("aria-expanded")).toBe("true");
    expect(card("Landing queue").textContent).toContain("last attempt");
    expect(screen.getByRole("list", { name: "Landing times" })).toBeTruthy();

    fireEvent.click(card("Gate"));
    expect(card("Landing queue").getAttribute("aria-expanded")).toBe("false");
    const gate = screen.getByRole("region", { name: "Gate duration per day" });
    expect(within(gate).getByText("core")).toBeTruthy();
    expect(within(gate).getByText("suspect")).toBeTruthy();

    fireEvent.click(card("Jobs"));
    expect(screen.getByRole("link", { name: "Job 21" }).getAttribute("href")).toBe("/projects/nucleos/state");

    fireEvent.click(card("Autopilot"));
    expect(screen.getByRole("region", { name: "Autopilot agreement" })).toBeTruthy();
  });

  it("writes no inline style, whichever panel is open", async () => {
    const { container } = await mount(withPipeline(busyReport()));
    await section();
    expect(container.querySelector("[style]")).toBeNull();
    for (const label of ["Post-merge gate", "Landing queue", "Gate", "Jobs", "Autopilot", "Slot pressure"]) {
      fireEvent.click(card(label));
      expect(container.querySelector("[style]")).toBeNull();
    }
  });

  it("shows conflicts in the landing panel and the owner-blocked line on its card", async () => {
    const report = busyReport();
    const landing = report.landing as Landing;
    await mount(
      withPipeline({
        ...report,
        landing: {
          ...landing,
          conflicts: { count: 3, rate: 0.12, auto_resolved: 2, work_lost: { nothing: 2, lost: 1 } },
          owner_blocked: { count: 2, oldest_age_s: 7200, median_age_s: 600 },
        },
      }),
    );
    await section();
    expect(card("Landing queue").textContent).toContain("2 waiting on you · oldest 2 h");
    const conflicts = screen.getByRole("list", { name: "Conflicts" });
    expect(within(conflicts).getByText("12% · 3 conflicts")).toBeTruthy();
    expect(within(conflicts).getByText("Work lost: lost")).toBeTruthy();
  });

  it("leaves the conflicts, cache and bypass pairs out when the block has none", async () => {
    await mount(withPipeline(busyReport()));
    await section();
    expect(screen.queryByRole("list", { name: "Conflicts" })).toBeNull();
    fireEvent.click(card("Gate"));
    expect(screen.queryByRole("list", { name: "Cache and bypass" })).toBeNull();
  });

  it("shows cache and bypass in the gate panel", async () => {
    const report = busyReport();
    const gate = report.gate as Gate;
    await mount(
      withPipeline({
        ...report,
        gate: {
          ...gate,
          cache: { hit_rate: 0.5, prev_hit_rate: 0.25 },
          bypass: { observed: 1, verify_units: 10, ratio: 0.1 },
        },
      }),
    );
    await section();
    fireEvent.click(card("Gate"));
    const pairs = screen.getByRole("list", { name: "Cache and bypass" });
    expect(within(pairs).getByText("50% · before 25%")).toBeTruthy();
    expect(within(pairs).getByText("1 of 10 verify units · 10%")).toBeTruthy();
  });

  it("opens the slot pressure panel with the at-least wait and the recording note", async () => {
    const report = busyReport();
    const pressure = report.pressure as Pressure;
    await mount(
      withPipeline({
        ...report,
        history: { postgate_since: null, slots_since: `${report.window.to}T03:00:00Z` },
        pressure: {
          ...pressure,
          at_limit_share: 0.22,
          peak_in_use: 4,
          limit: 4,
          slot_wait_share: 0.18,
          slot_wait_share_is_lower_bound: true,
        },
      }),
    );
    await section();
    expect(card("Slot pressure").textContent).toContain("22%");
    expect(card("Slot pressure").textContent).toContain("peak 4 of 4");
    fireEvent.click(card("Slot pressure"));
    expect(screen.getByRole("region", { name: "Slots in use per day" })).toBeTruthy();
    expect(screen.getByText("Waiting for a slot, at least")).toBeTruthy();
    expect(screen.getByText("18%")).toBeTruthy();
    expect(screen.getByText(`Recording since ${report.window.to}. No data for earlier periods yet.`)).toBeTruthy();
  });

  it("reads empty pressure as no data and a failed pressure block as an error card", async () => {
    await mount(withPipeline(pipelineReport()));
    await section();
    expect(within(card("Slot pressure")).getByText("No samples in this window")).toBeTruthy();
    fireEvent.click(card("Slot pressure"));
    expect(screen.queryByText(/Recording since/)).toBeNull();
  });

  it("a pressure block error leaves the other cards", async () => {
    await mount(withPipeline({ ...busyReport(), pressure: { error: "query_failed" } }));
    await section();
    expect(within(card("Slot pressure")).getByText("Could not compute")).toBeTruthy();
    fireEvent.click(card("Slot pressure"));
    expect(screen.getByText("Could not compute this block.")).toBeTruthy();
    expect(card("Jobs").textContent).toContain("75%");
  });
});
