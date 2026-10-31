import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, screen, waitFor, within } from "@testing-library/react";
import type { ReactElement } from "react";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
const daemon = vi.hoisted(() => ({ apiFetch: vi.fn(), apiText: vi.fn(), probeHealth: vi.fn() }));
vi.mock("../data/client", async (original) => ({ ...(await original<typeof import("../data/client")>()), ...daemon }));

import { ApiRefusal } from "../data/client";
import type { ActionPair, ActionPairs, PromotionReport, ShadowItem, ShadowPage } from "../data/devtime";
import { daemonFetch, daemonState, devtimeOverview, renderWithRouter } from "../test/harness";
import { ActionsShadowPanel } from "./ActionsShadowPanel";
import { DevtimeSection } from "./DevtimeSection";

function report(over: Partial<PromotionReport> = {}): PromotionReport {
  return {
    action_eligible: true, wilson_lower: 0.7, n_resolved: 12, n_sessions: 6, days_covered: 9, sample_ok: true,
    hit_rate: 0.8, mean_hit_cost_ms: 4000, shadow_approved: true, avoidable_now: 0.1, avoidable_prev: 0.2,
    regression_ok: true, human_marks: 3, human_rate: 0.9, human_ok: false, ready: false,
    median_hit_cost_ms: 3500, suppress_rate: 0.1, ignored: 0, ...over,
  };
}

function pair(over: Partial<ActionPair> = {}): ActionPair {
  return { action: "inject", rule_id: "A1", mode: "shadow", since: null, changed_by: null, report: report(), ...over };
}

function item(over: Partial<ShadowItem> = {}): ShadowItem {
  return {
    id: 7, action: "inject", rule_id: "A1", project_id: "alpha", decision: "fire", suppress_reason: null,
    template_id: "tpl_reread", params: { limit: 3, kind: "x" }, decided_at: "2026-10-29T10:00:00Z",
    outcome_state: "resolved", outcome: "hit", human_verdict: null, ...over,
  };
}

interface Answers { pairs?: ActionPairs; shadow?: ShadowPage; verdict?: () => Promise<ShadowItem> }

function mount(answers: Answers, ui: ReactElement = <ActionsShadowPanel />) {
  daemon.apiFetch.mockImplementation(async (path: string, init?: RequestInit) => {
    if (path === "/analytics/devtime/actions") return answers.pairs ?? { pairs: [pair()] };
    if (path.startsWith("/analytics/devtime/actions/shadow/") && path.endsWith("/verdict")) {
      return answers.verdict === undefined ? item({ human_verdict: JSON.parse(String(init?.body)).verdict }) : answers.verdict();
    }
    if (path.startsWith("/analytics/devtime/actions/shadow")) return answers.shadow ?? { items: [item()], next_cursor: null };
    return daemonFetch(daemonState({ devtime: devtimeOverview() }))(path, init);
  });
  return renderWithRouter(ui);
}

beforeEach(() => {
  daemon.apiFetch.mockReset();
});

describe("ActionsShadowPanel", () => {
  it("shows each shadow decision with its template id and params", async () => {
    await mount({ shadow: { items: [item(), item({ id: 8, template_id: "tpl_other", params: {} })], next_cursor: null } });
    expect(await screen.findByText("tpl_reread")).toBeTruthy();
    expect(screen.getByText("limit: 3, kind: x")).toBeTruthy();
    expect(screen.getByText("tpl_other")).toBeTruthy();
  });

  it("asks for the shadow decisions of each pair by action and rule", async () => {
    await mount({ pairs: { pairs: [pair(), pair({ action: "propose", rule_id: "B2" })] } });
    await screen.findAllByText("tpl_reread");
    const paths = daemon.apiFetch.mock.calls.map((call) => call[0] as string);
    expect(paths).toContain("/analytics/devtime/actions/shadow?limit=50&action=inject&rule=A1");
    expect(paths).toContain("/analytics/devtime/actions/shadow?limit=50&action=propose&rule=B2");
  });

  it("counts the criteria met as N/5", async () => {
    // eligible, sample, approved and regression hold; the human review does not: 4 of 5.
    await mount({});
    expect(await screen.findByText("4/5")).toBeTruthy();
    expect(screen.getByText(/criteria met/)).toBeTruthy();
  });

  it("counts 5/5 when every criterion holds", async () => {
    await mount({ pairs: { pairs: [pair({ report: report({ human_ok: true, ready: true }) })] } });
    expect(await screen.findByText("5/5")).toBeTruthy();
  });

  it("marks a decision useful with one POST of the verdict", async () => {
    await mount({});
    const row = (await screen.findByText("tpl_reread")).closest("li") as HTMLElement;
    fireEvent.click(within(row).getByRole("button", { name: "useful" }));
    await waitFor(() =>
      expect(daemon.apiFetch).toHaveBeenCalledWith("/analytics/devtime/actions/shadow/7/verdict", {
        method: "POST",
        body: JSON.stringify({ verdict: "useful" }),
      }),
    );
  });

  it("marks a decision as noise", async () => {
    await mount({});
    const row = (await screen.findByText("tpl_reread")).closest("li") as HTMLElement;
    fireEvent.click(within(row).getByRole("button", { name: "noise" }));
    await waitFor(() =>
      expect(daemon.apiFetch).toHaveBeenCalledWith("/analytics/devtime/actions/shadow/7/verdict", {
        method: "POST",
        body: JSON.stringify({ verdict: "noise" }),
      }),
    );
  });

  it("shows the stored verdict as the pressed button", async () => {
    await mount({ shadow: { items: [item({ human_verdict: "noise" })], next_cursor: null } });
    const row = (await screen.findByText("tpl_reread")).closest("li") as HTMLElement;
    expect(within(row).getByRole("button", { name: "noise" }).getAttribute("aria-pressed")).toBe("true");
    expect(within(row).getByRole("button", { name: "useful" }).getAttribute("aria-pressed")).toBe("false");
  });

  it("offers no mark on a suppressed decision, and says why it was suppressed", async () => {
    await mount({ shadow: { items: [item({ decision: "suppress", suppress_reason: "budget" })], next_cursor: null } });
    const row = (await screen.findByText("tpl_reread")).closest("li") as HTMLElement;
    expect(within(row).queryByRole("button", { name: "useful" })).toBeNull();
    expect(within(row).getByText(/budget/)).toBeTruthy();
  });

  it("says the mark was not saved when the daemon refuses, and reverts the pressed state", async () => {
    await mount({ verdict: () => Promise.reject(new ApiRefusal(404, "unknown_shadow", "")) });
    const row = (await screen.findByText("tpl_reread")).closest("li") as HTMLElement;
    fireEvent.click(within(row).getByRole("button", { name: "useful" }));
    expect(await within(row).findByText("The mark was not saved.")).toBeTruthy();
    expect(within(row).getByRole("button", { name: "useful" }).getAttribute("aria-pressed")).toBe("false");
  });

  it("says there is nothing yet when a pair has no shadow decisions", async () => {
    await mount({ shadow: { items: [], next_cursor: null } });
    expect(await screen.findByText("No shadow decisions yet.")).toBeTruthy();
  });

  it("asks for the next page with the opaque cursor", async () => {
    await mount({ shadow: { items: [item()], next_cursor: "0a1b" } });
    fireEvent.click(await screen.findByRole("button", { name: "Next page" }));
    await waitFor(() =>
      expect(daemon.apiFetch).toHaveBeenCalledWith("/analytics/devtime/actions/shadow?limit=50&action=inject&rule=A1&cursor=0a1b"),
    );
  });

  it("fails on its own: an error note with a retry", async () => {
    daemon.apiFetch.mockRejectedValue(new Error("down"));
    await renderWithRouter(<ActionsShadowPanel />);
    expect(await screen.findByText(/Could not read automatic actions/)).toBeTruthy();
    expect(screen.getByRole("button", { name: "Try again" })).toBeTruthy();
  });

  it("sits in the development time section as its own card", async () => {
    await mount({}, <DevtimeSection days={30} />);
    expect(await screen.findByRole("region", { name: "Automatic actions" })).toBeTruthy();
    expect(await screen.findByText("tpl_reread")).toBeTruthy();
  });
});
