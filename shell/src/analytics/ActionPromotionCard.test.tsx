import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, screen, waitFor, within } from "@testing-library/react";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
const daemon = vi.hoisted(() => ({ apiFetch: vi.fn(), apiText: vi.fn(), probeHealth: vi.fn() }));
vi.mock("../data/client", async (original) => ({ ...(await original<typeof import("../data/client")>()), ...daemon }));

import { ApiRefusal } from "../data/client";
import type { ActionPair, PromotionReport } from "../data/devtime";
import { renderWithRouter } from "../test/harness";
import { ActionPromotionCard } from "./ActionPromotionCard";

function report(over: Partial<PromotionReport> = {}): PromotionReport {
  return {
    action_eligible: true, wilson_lower: 0.7, n_resolved: 12, n_sessions: 6, days_covered: 9, sample_ok: true,
    hit_rate: 0.8, mean_hit_cost_ms: 4000, shadow_approved: true, avoidable_now: 0.1, avoidable_prev: 0.2,
    regression_ok: true, human_marks: 5, human_rate: 0.9, human_ok: true, ready: true,
    median_hit_cost_ms: 3500, suppress_rate: 0.1, ignored: 0, ...over,
  };
}

function pair(over: Partial<PromotionReport> = {}, rest: Partial<ActionPair> = {}): ActionPair {
  return { action: "inject", rule_id: "A1", mode: "shadow", since: null, changed_by: null, report: report(over), ...rest };
}

const PROMOTE = "/analytics/devtime/actions/inject/A1/promote";
const promoteCalls = () => daemon.apiFetch.mock.calls.filter((call) => call[0] === PROMOTE);
const pause = (ms: number) => new Promise((resolve) => setTimeout(resolve, ms));

/** The house interlock's keyboard-less path: a first click arms, a second one past the dwell confirms. */
async function confirmPromotion() {
  fireEvent.click(screen.getByRole("button", { name: "Promote" }));
  await pause(350);
  fireEvent.click(screen.getByRole("button", { name: /Really promote/ }));
}

beforeEach(() => {
  daemon.apiFetch.mockReset();
  daemon.apiFetch.mockResolvedValue(pair({}, { mode: "live", changed_by: "owner" }));
});

describe("ActionPromotionCard", () => {
  it("lists exactly the five criteria", async () => {
    await renderWithRouter(<ActionPromotionCard pair={pair()} />);
    const list = await screen.findByRole("list", { name: "Promotion criteria" });
    expect(within(list).getAllByRole("listitem")).toHaveLength(5);
    for (const name of [/Rule eligible/, /Enough resolved cases/, /Shadow approved/, /No regression/, /Human review/]) {
      expect(within(list).getByText(name)).toBeTruthy();
    }
  });

  it("marks each criterion as met or not met from the report", async () => {
    await renderWithRouter(<ActionPromotionCard pair={pair({ human_ok: false, sample_ok: false, ready: false })} />);
    const list = await screen.findByRole("list", { name: "Promotion criteria" });
    const rows = within(list).getAllByRole("listitem");
    const notMet = rows.map((row) => /not met/.test(row.textContent ?? ""));
    expect(notMet).toEqual([false, true, false, false, true]);
  });

  it("shows the figures behind the criteria", async () => {
    await renderWithRouter(<ActionPromotionCard pair={pair({ n_resolved: 12, human_marks: 5 })} />);
    expect(await screen.findByText(/12 resolved/)).toBeTruthy();
    expect(screen.getByText(/5 marks/)).toBeTruthy();
  });

  it("disables Promote while the pair is not ready, and never calls the daemon", async () => {
    await renderWithRouter(<ActionPromotionCard pair={pair({ human_ok: false, ready: false })} />);
    const button = await screen.findByRole("button", { name: "Promote" });
    expect((button as HTMLButtonElement).disabled || button.getAttribute("aria-disabled") === "true").toBe(true);
    fireEvent.click(button);
    await pause(350);
    fireEvent.click(button);
    expect(promoteCalls()).toHaveLength(0);
  });

  it("enables Promote when the pair is ready", async () => {
    await renderWithRouter(<ActionPromotionCard pair={pair()} />);
    const button = await screen.findByRole("button", { name: "Promote" });
    expect((button as HTMLButtonElement).disabled).toBe(false);
    expect(button.getAttribute("aria-disabled")).toBeNull();
  });

  it("asks for confirmation before it promotes: one click calls nothing", async () => {
    await renderWithRouter(<ActionPromotionCard pair={pair()} />);
    fireEvent.click(await screen.findByRole("button", { name: "Promote" }));
    expect(await screen.findByRole("button", { name: /Really promote/ })).toBeTruthy();
    expect(promoteCalls()).toHaveLength(0);
  });

  it("promotes with one POST once confirmed", async () => {
    await renderWithRouter(<ActionPromotionCard pair={pair()} />);
    await screen.findByRole("button", { name: "Promote" });
    await confirmPromotion();
    await waitFor(() => expect(promoteCalls()).toEqual([[PROMOTE, { method: "POST" }]]));
  });

  it("says the criteria no longer hold when the daemon answers not_ready", async () => {
    daemon.apiFetch.mockRejectedValue(new ApiRefusal(409, "not_ready", ""));
    await renderWithRouter(<ActionPromotionCard pair={pair()} />);
    await screen.findByRole("button", { name: "Promote" });
    await confirmPromotion();
    expect(await screen.findByText("Not promoted: the criteria no longer hold.")).toBeTruthy();
  });

  it("says live and since when once the pair is live, with no Promote button", async () => {
    await renderWithRouter(<ActionPromotionCard pair={pair({}, { mode: "live", changed_by: "owner", since: "2026-10-30T09:00:00Z" })} />);
    expect(await screen.findByText(/Live since/)).toBeTruthy();
    expect(screen.queryByRole("button", { name: "Promote" })).toBeNull();
  });
});
