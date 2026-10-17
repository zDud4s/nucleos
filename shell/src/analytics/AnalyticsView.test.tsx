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
      delivered: {
        ...base.delivered,
        total: { landed: 3, timed: 0, median_minutes: null },
        // One merge on day 3, two on day 10, none since: a running total of 3 on the last day, which
        // lands nothing of its own.
        series: base.delivered.series.map((point, index) => ({ ...point, landed: index === 3 ? 1 : index === 10 ? 2 : 0 })),
      },
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

/** The four card buttons, by the label their accessible name starts with. */
function card(label: "Spent" | "Runs" | "Delivered" | "Tokens"): HTMLElement {
  return screen.getByRole("button", { name: new RegExp(`^${label}`) });
}

/** The labels a share bar lists, in its order. */
function shareLabels(name: string): string[] {
  const list = screen.getByRole("list", { name: `${name}, by item` });
  return within(list)
    .getAllByRole("listitem")
    .map((item) => item.querySelector(".ui-share-label")?.textContent ?? "");
}

describe("AnalyticsView, global", () => {
  it("asks the global routes for the default window and draws the cards, the spend chart and the lower row", async () => {
    await mount(busy());

    expect(await screen.findByRole("region", { name: "Spend per day" })).toBeTruthy();
    for (const name of ["Most expensive sessions", "Quota"]) {
      expect(screen.getByRole("region", { name })).toBeTruthy();
    }
    expect(asked()).toContain("/analytics?days=30");
    expect(asked()).toContain("/analytics/quota?days=30");
    expect(screen.getByRole("button", { name: "30 days" }).getAttribute("aria-pressed")).toBe("true");
  });

  it("keeps the coverage note behind a disclosure beside the range", async () => {
    await mount(busy());
    const note = await screen.findByText(/Every run counts here, in every mode/);
    const about = note.closest("details");
    expect(about).not.toBeNull();
    expect(within(about as HTMLElement).getByText("About these numbers").tagName).toBe("SUMMARY");
    expect(screen.getByText(/days in UTC/)).toBeTruthy();
  });

  it("draws four cards, each a button for the one detail panel, with spend open by default", async () => {
    await mount(busy());
    await screen.findByRole("region", { name: "Spend per day" });

    const cards = (["Spent", "Runs", "Delivered", "Tokens"] as const).map(card);
    expect(cards.map((button) => button.getAttribute("aria-expanded"))).toEqual(["true", "false", "false", "false"]);
    const controls = new Set(cards.map((button) => button.getAttribute("aria-controls")));
    expect(controls.size).toBe(1);
    const panel = document.getElementById([...controls][0] ?? "");
    expect(panel).toBe(screen.getByRole("region", { name: "Spend per day" }));
  });

  it("opens the runs chart from its card, closes spend, and keeps it open on a second click", async () => {
    await mount(busy());
    await screen.findByRole("region", { name: "Spend per day" });

    fireEvent.click(card("Runs"));
    expect(card("Runs").getAttribute("aria-expanded")).toBe("true");
    expect(card("Spent").getAttribute("aria-expanded")).toBe("false");
    expect(screen.getByRole("region", { name: "Runs per day" })).toBeTruthy();
    expect(screen.queryByRole("region", { name: "Spend per day" })).toBeNull();

    // There is no "none": the open card stays open, so the page never loses its chart.
    fireEvent.click(card("Runs"));
    expect(card("Runs").getAttribute("aria-expanded")).toBe("true");
    expect(screen.getByRole("region", { name: "Runs per day" })).toBeTruthy();
  });

  it("breaks spend down by project in the share bar, the judge and the estimate as items of their own", async () => {
    await mount(busy());
    await screen.findByRole("region", { name: "Spend per day" });
    expect(screen.getByRole("img", { name: "Spend per day by project, last 30 days" })).toBeTruthy();
    // "Other projects" spent nothing, and a share bar leaves a zero out.
    expect(shareLabels("Spend by project, last 30 days")).toEqual(["nucleos", "Judge", "Estimated from run time"]);
    const list = screen.getByRole("list", { name: "Spend by project, last 30 days, by item" });
    const estimate = within(list).getByText("Estimated from run time").closest("li");
    expect(estimate?.querySelector(".ui-chart-estimated")).not.toBeNull();
  });

  it("charts delivered as a running total, with its median and timed count under it", async () => {
    await mount(busy());
    await screen.findByRole("region", { name: "Spend per day" });

    fireEvent.click(card("Delivered"));
    const panel = screen.getByRole("region", { name: "Delivered, running total" });
    expect(within(panel).getByText("3 landed")).toBeTruthy();
    // Focus lands on the last day: it landed nothing itself, and the running total still reads 3.
    fireEvent.focus(within(panel).getByLabelText(/^Delivered, running total, last 30 days: use the arrow keys/));
    expect(within(panel).getByText(/Landed so far 3$/)).toBeTruthy();
    // The screen-reader sentence reads the days themselves: three merges, two on the busiest day.
    expect(within(panel).getByText(/^Total 3; peak 2 on /)).toBeTruthy();

    expect(within(panel).getByText("Median, run to merge")).toBeTruthy();
    expect(within(panel).getByText("No merge with an associated run.")).toBeTruthy();
    expect(within(panel).getByText("With a measured time").nextElementSibling?.textContent).toBe("0");
  });

  it("explains an empty quota history instead of drawing a blank chart", async () => {
    await mount(busy());
    const quota = await screen.findByRole("region", { name: "Quota" });
    expect(await within(quota).findByText("No quota history yet")).toBeTruthy();
  });

  it("writes no inline style, whichever chart is open", async () => {
    const { container } = await mount(busy());
    await screen.findByRole("region", { name: "Spend per day" });
    expect(container.querySelectorAll("[style]")).toHaveLength(0);
    for (const label of ["Runs", "Delivered", "Tokens"] as const) {
      fireEvent.click(card(label));
      expect(container.querySelectorAll("[style]")).toHaveLength(0);
    }
  });
});

describe("AnalyticsView, one project", () => {
  it("asks the project's route, breaks spend down by mode, and never asks for quota", async () => {
    const { container } = await mount(busy("project"), "nucleos");

    expect(await screen.findByRole("img", { name: "Spend per day by mode, last 30 days" })).toBeTruthy();
    expect(shareLabels("Spend by mode, last 30 days")).toContain("worktree");
    expect(asked()).toContain("/projects/nucleos/analytics?days=30");
    expect(asked().some((path) => path.startsWith("/analytics/quota"))).toBe(false);
    expect(screen.queryByRole("region", { name: "Quota" })).toBeNull();
    expect(screen.getByRole("region", { name: "Most expensive sessions" })).toBeTruthy();
    // With no quota beside it, the sessions take the row.
    expect(container.querySelector(".an-lower-split")).toBeNull();
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

describe("what the cards say", () => {
  it("draws absent as an em dash with a reason, never as zero", async () => {
    await mount(daemonState({ projects: [project({ project_id: "nucleos" })] }));
    await screen.findByRole("region", { name: "Spend per day" });
    const tokens = card("Tokens");
    expect(within(tokens).getByText("—")).toBeTruthy();
    expect(within(tokens).getByText("no session reported usage")).toBeTruthy();
    expect(within(card("Delivered")).getByText("no merge with an associated run")).toBeTruthy();
    // Nothing spent and nothing run are real zeros, and say so.
    expect(within(card("Spent")).getByText("$0.00")).toBeTruthy();
    expect(within(card("Runs")).getByText("0")).toBeTruthy();
    expect(within(card("Runs")).getByText("nothing judged")).toBeTruthy();
  });

  it("names the runs nobody measured, and the estimate apart from the measured cost", async () => {
    await mount(busy());
    const panel = await screen.findByRole("region", { name: "Spend per day" });
    const spent = card("Spent");
    expect(within(spent).getByText("$12.34")).toBeTruthy();
    expect(spent.textContent).toContain("$1.20 estimated");
    expect(within(panel).getByText("$12.34 in 30 days · $1.20 estimated, counted apart · $0.35 judge")).toBeTruthy();

    // Tokens were measured (no em dash), but the cache ratio was not: no percentage is made up.
    expect(within(card("Tokens")).getByText("5k")).toBeTruthy();
    expect(card("Tokens").textContent).not.toContain("%");
    fireEvent.click(card("Tokens"));
    const tokens = screen.getByRole("region", { name: "Tokens per day" });
    expect(within(tokens).getByText("3 runs not measured")).toBeTruthy();
    expect(shareLabels("Tokens by kind, last 30 days")).toEqual(["Input", "Cache read", "Output"]);
  });

  it("reads the estimate apart from the measured total in the cost summary", async () => {
    await mount(busy());
    await screen.findByRole("region", { name: "Spend per day" });
    const text = screen.getByText(/^Total \$.*measured/).textContent ?? "";
    expect(text).toContain("Total $2.35 measured");
    expect(text).toContain("$0.50 estimated from run time");
    expect(text).toContain("peak $2.35");
  });

  it("says the change in words", async () => {
    await mount(busy());
    await screen.findByRole("region", { name: "Spend per day" });
    expect(card("Spent").textContent).toContain("more than the previous 30 days");
  });

  it("keeps failed and could-not-run apart, lists every mode raw, and links the costliest run", async () => {
    await mount(busy());
    await screen.findByRole("region", { name: "Spend per day" });
    expect(card("Runs").textContent).toContain("85% passed the gate · 2 running");

    fireEvent.click(card("Runs"));
    const runs = screen.getByRole("region", { name: "Runs per day" });
    expect(within(runs).getByText("52 completed · 2 still running")).toBeTruthy();
    expect(shareLabels("Runs by gate result, last 30 days")).toEqual([
      "Passed",
      "Failed",
      "Could not run",
      "No gate",
      "Other status",
    ]);
    const modes = screen.getByRole("list", { name: "Runs by mode" });
    expect(within(modes).getByText("relay_v9")).toBeTruthy();

    const link = screen.getByRole("link", { name: /nucleos.*\$3\.20 \(estimated\)/ });
    expect(link.getAttribute("href")).toBe("/runs/412");
    const orphan = screen.getByRole("link", { name: /No project.*\$1\.10/ });
    expect(orphan.getAttribute("href")).toBe("/runs/398");
  });

  it("draws an empty window as a chart with a sentence, and no share bar", async () => {
    await mount(daemonState({ projects: [project({ project_id: "nucleos" })] }));
    const panel = await screen.findByRole("region", { name: "Spend per day" });
    expect(within(panel).getByText("No runs in this window")).toBeTruthy();
    expect(within(panel).queryByRole("list")).toBeNull();
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
