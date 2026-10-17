import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, screen, waitFor, within } from "@testing-library/react";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
const daemon = vi.hoisted(() => ({ apiFetch: vi.fn(), apiText: vi.fn(), probeHealth: vi.fn() }));
vi.mock("../data/client", async (original) => ({
  ...(await original<typeof import("../data/client")>()),
  ...daemon,
}));

import {
  analyticsReport,
  daemonFetch,
  daemonState,
  project,
  renderWithRouter,
  type DaemonState,
} from "../test/harness";
import { AnalyticsView } from "./AnalyticsView";

/** Mount the view over a daemon holding these facts; any path starting with a `failing` prefix throws. */
function mount(state: DaemonState, projectId?: string, failing: Set<string> = new Set()) {
  const answer = daemonFetch(state);
  daemon.apiFetch.mockImplementation(async (path: string, init?: RequestInit) => {
    for (const prefix of failing) {
      if (path.startsWith(prefix)) throw new Error(`${prefix} is down`);
    }
    return answer(path, init);
  });
  return renderWithRouter(<AnalyticsView projectId={projectId} />);
}

const asked = () => daemon.apiFetch.mock.calls.map(([path]) => String(path));

/** A month with something in every block — and every absence the page must say out loud. */
function busy(scope: "global" | "project" = "global"): DaemonState {
  const base = analyticsReport();
  const last = base.cost.series.length - 1;
  const keys =
    scope === "global"
      ? [
          { key: "nucleos", label: "nucleos" },
          { key: "__other__", label: "Other projects" },
          { key: "judge", label: "Judge" },
        ]
      : [
          { key: "worktree", label: "worktree" },
          { key: "relay_v9", label: "relay_v9" },
          { key: "judge", label: "Judge" },
        ];
  // The daemon's contract: one part per `keys[]` entry, in order, zeros included, on every day.
  const parts = (today: boolean) =>
    keys.map(({ key }) => ({
      key,
      measured_usd: !today ? 0 : key === keys[0].key ? 2 : key === "judge" ? 0.35 : 0,
      estimated_usd: today && key === keys[0].key ? 0.5 : 0,
    }));
  const tokens = {
    input: 1_000,
    cache_read: 3_000,
    cache_creation: 0,
    output: 500,
    measured_sessions: 38,
    unmeasured_runs: 3,
    cache_hit_ratio: null,
  };
  return daemonState({
    projects: [project({ project_id: "nucleos", mode: "shadow" })],
    analytics: analyticsReport({
      cost: {
        ...base.cost,
        by_label: scope === "global" ? "project" : "mode",
        keys,
        total: { measured_usd: 12.34, estimated_usd: 1.2, judge_usd: 0.35, runs: 41, estimated_runs: 3 },
        previous: { measured_usd: 6, estimated_usd: 0, judge_usd: 0, runs: 20, estimated_runs: 0 },
        series: base.cost.series.map((point, index) => ({ ...point, by: parts(index === last) })),
      },
      tokens: { ...base.tokens, total: tokens, previous: { ...tokens, unmeasured_runs: 0 } },
      throughput: {
        ...base.throughput,
        total: { runs_completed: 52, passed: 40, failed: 5, errored: 2, no_gate: 4, other: 1 },
        by_mode: [
          { mode: "worktree", runs_completed: 40 },
          { mode: "relay_v9", runs_completed: 12 },
        ],
        in_flight: 2,
      },
      delivered: { ...base.delivered, total: { landed: 9, timed: 0, median_minutes: null } },
      top_runs: [
        {
          run_id: 412,
          project_id: "nucleos",
          mode: "worktree",
          completed_at: "2026-08-23T10:00:00Z",
          cost_usd: 3.2,
          cost_is_estimated: true,
        },
        // A run with no project (`project_id: null`, the daemon's `__none__`).
        {
          run_id: 398,
          project_id: null,
          mode: "worktree",
          completed_at: "2026-08-22T10:00:00Z",
          cost_usd: 1.1,
          cost_is_estimated: false,
        },
      ],
    }),
  });
}

beforeEach(() => {
  daemon.apiFetch.mockReset();
  daemon.apiText.mockReset();
  daemon.probeHealth.mockReset();
  daemon.probeHealth.mockResolvedValue(true);
});

describe("AnalyticsView, global", () => {
  it("asks the global routes for the default window and draws the five sections", async () => {
    await mount(busy());

    expect(await screen.findByRole("region", { name: "Cost and tokens" })).toBeTruthy();
    for (const name of ["Throughput and gate", "Delivered", "Most expensive sessions", "Quota"]) {
      expect(screen.getByRole("region", { name })).toBeTruthy();
    }
    expect(asked()).toContain("/analytics?days=30");
    expect(asked()).toContain("/analytics/quota?days=30");
    expect(screen.getByRole("button", { name: "30 days" }).getAttribute("aria-pressed")).toBe("true");
  });

  it("says that every mode is counted and that the ceiling counts fewer", async () => {
    await mount(busy());
    expect(await screen.findByText(/Every run counts here, in every mode/)).toBeTruthy();
    expect(screen.getByText(/days in UTC/)).toBeTruthy();
  });

  it("stacks cost by project, with the judge and the estimate as parts of their own", async () => {
    await mount(busy());
    const legend = await screen.findByRole("list", { name: /^Cost per day by project, last 30 days/ });
    const items = within(legend).getAllByRole("listitem").map((item) => item.textContent);
    expect(items).toEqual(["nucleos", "Other projects", "Judge", "Estimated from run time"]);
  });

  it("explains an empty quota history instead of drawing a blank chart", async () => {
    await mount(busy());
    const quota = await screen.findByRole("region", { name: "Quota" });
    expect(await within(quota).findByText("No quota history yet")).toBeTruthy();
  });
});

describe("AnalyticsView, one project", () => {
  it("asks the project's route, stacks by mode, and never asks for quota", async () => {
    await mount(busy("project"), "nucleos");

    const legend = await screen.findByRole("list", { name: /^Cost per day by mode, last 30 days/ });
    expect(within(legend).getByText("relay_v9")).toBeTruthy();
    expect(asked()).toContain("/projects/nucleos/analytics?days=30");
    expect(asked().some((path) => path.startsWith("/analytics/quota"))).toBe(false);
    expect(screen.queryByRole("region", { name: "Quota" })).toBeNull();
    expect(screen.getByRole("region", { name: "Most expensive sessions" })).toBeTruthy();
  });

  it("keeps its window in local state", async () => {
    await mount(busy("project"), "nucleos");
    fireEvent.click(await screen.findByRole("button", { name: "7 days" }));
    await waitFor(() => expect(asked()).toContain("/projects/nucleos/analytics?days=7"));
    expect(screen.getByRole("button", { name: "7 days" }).getAttribute("aria-pressed")).toBe("true");
  });

  it("answers a project the roster does not have with an absence, not an error", async () => {
    await mount(busy("project"), "ghost");
    // The card first reads "reading…"; wait for the answer, then check it sits in the Analytics card.
    const reason = await screen.findByText("No project by that name.");
    expect(within(screen.getByRole("article", { name: "Analytics" })).getByText(reason.textContent ?? "")).toBeTruthy();
  });
});

describe("what the tiles say", () => {
  it("draws absent as an em dash with a reason, never as zero", async () => {
    await mount(busy());
    const ratio = await screen.findByRole("article", { name: "Cache hit ratio" });
    expect(within(ratio).getByText("—")).toBeTruthy();
    expect(within(ratio).getByText("nothing measured")).toBeTruthy();

    const merge = screen.getByRole("article", { name: "Run to merge" });
    expect(within(merge).getByText("—")).toBeTruthy();
    expect(within(merge).getByText("no merge with an associated run")).toBeTruthy();
  });

  it("names the runs nobody measured, and the estimate apart from the measured cost", async () => {
    await mount(busy());
    expect(await screen.findByText("3 runs not measured")).toBeTruthy();
    const estimated = screen.getByRole("article", { name: "Estimated cost" });
    expect(within(estimated).getByText("$1.20")).toBeTruthy();
    const measured = screen.getByRole("article", { name: "Cost" });
    expect(within(measured).getByText("$12.34")).toBeTruthy();
    expect(within(measured).getByText(/\$0\.35 judge, counted apart/)).toBeTruthy();
  });

  it("reads the estimate apart from the measured total in the cost summary", async () => {
    await mount(busy());
    await screen.findByRole("region", { name: "Cost and tokens" });
    const text = screen.getByText(/^Total \$.*measured/).textContent ?? "";
    expect(text).toContain("Total $2.35 measured");
    expect(text).toContain("$0.50 estimated from run time");
    expect(text).toContain("peak $2.35");
  });

  it("says the change in words", async () => {
    await mount(busy());
    const measured = await screen.findByRole("article", { name: "Cost" });
    expect(within(measured).getByText(/more than the previous 30 days/)).toBeTruthy();
    const tokens = screen.getByRole("article", { name: "Tokens" });
    expect(within(tokens).getByText(/level with the previous 30 days/)).toBeTruthy();
  });

  it("keeps failed and could-not-run apart, lists every mode raw, and links the costliest run", async () => {
    await mount(busy());
    const failing = await screen.findByRole("article", { name: "Failed or could not run" });
    expect(within(failing).getByText("5 failed · 2 could not run")).toBeTruthy();
    expect(screen.getByRole("article", { name: "Pass rate" }).textContent).toContain("85%");
    const modes = screen.getByRole("list", { name: "Runs by mode" });
    expect(within(modes).getByText("relay_v9")).toBeTruthy();
    const link = screen.getByRole("link", { name: /nucleos.*\$3\.20 \(estimated\)/ });
    expect(link.getAttribute("href")).toBe("/runs/412");
    const orphan = screen.getByRole("link", { name: /No project.*\$1\.10/ });
    expect(orphan.getAttribute("href")).toBe("/runs/398");
  });

  it("draws an empty window as charts with axes and a sentence", async () => {
    await mount(daemonState({ projects: [project({ project_id: "nucleos" })] }));
    expect((await screen.findAllByText("No runs in this window")).length).toBeGreaterThanOrEqual(2);
    expect(screen.getByText("No run finished in this window.")).toBeTruthy();
  });
});

describe("refused and stale", () => {
  it("says once that the núcleo did not answer", async () => {
    await mount(busy(), undefined, new Set(["/analytics"]));
    expect(await screen.findByText("The núcleo did not answer for analytics.")).toBeTruthy();
  });

  it("keeps the last good read and dates it when a refetch fails", async () => {
    const failing = new Set<string>();
    const { queryClient } = await mount(busy(), undefined, failing);
    expect(await screen.findByText("$12.34")).toBeTruthy();

    failing.add("/analytics?");
    await queryClient.refetchQueries({ predicate: (query) => JSON.stringify(query.queryKey).includes("global") });

    expect(await screen.findByText(/view is stale — last good read/)).toBeTruthy();
    expect(screen.getByText("$12.34")).toBeTruthy();
  });
});
