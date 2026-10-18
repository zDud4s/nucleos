// The harness builder needs a DOM, so this one runs in the default (jsdom) environment.
import { describe, expect, it } from "vitest";
import { pipelineMetrics } from "./cards";
import { pipelineReport } from "../../test/harness";
import type { Gate, Jobs, Landing, Postgate } from "../../data/analytics-pipeline";

const byKey = (report: ReturnType<typeof pipelineReport>) =>
  Object.fromEntries(pipelineMetrics(report).map((metric) => [metric.key, metric]));

const red = (over: Partial<Postgate> = {}): Postgate => ({
  ...(pipelineReport().postgate as Postgate),
  red_now: true,
  red_since: "2026-10-18T08:11:00Z",
  red_phase: "bisect",
  ...over,
});

describe("pipelineMetrics", () => {
  it("gives one card per block, in the spec's order", () => {
    expect(pipelineMetrics(pipelineReport()).map((metric) => metric.key)).toEqual([
      "postgate",
      "landing",
      "gate",
      "jobs",
      "autopilot",
    ]);
  });

  it("says red now in words, with how long and in which phase", () => {
    const report = pipelineReport({ now: "2026-10-18T09:41:00Z", postgate: red() });
    const card = byKey(report).postgate;
    expect(card.alert).toBe(true);
    expect(card.value).toBe("Red");
    expect(card.sub).toBe("Red for 1 h 30 (phase: bisect)");
  });

  it("does not invent a start for a red that began before the record", () => {
    const card = byKey(pipelineReport({ postgate: red({ red_since: null, red_phase: null }) })).postgate;
    expect(card.sub).toBe("Red since before the record");
  });

  it("reads green with the red time of the window, and the reverts", () => {
    const base = pipelineReport().postgate as Postgate;
    const quiet = byKey(pipelineReport({ postgate: base })).postgate;
    expect(quiet.value).toBe("Green");
    expect(quiet.alert).toBe(false);
    expect(quiet.sub).toBe("no red in 30 days");
    const had = byKey(pipelineReport({ postgate: { ...base, red_s_in_window: 5400, reverts: 2 } })).postgate;
    expect(had.sub).toBe("1 h 30 red in 30 days · 2 reverts");
  });

  it("turns a block that is an error into an error card and leaves the rest", () => {
    const cards = byKey(pipelineReport({ gate: { error: "query_failed" } }));
    expect(cards.gate.error).toBe(true);
    expect(cards.gate.value).toBeUndefined();
    expect(cards.landing.error).toBeUndefined();
  });

  it("writes null ratios as no data, never 0% or NaN", () => {
    const cards = Object.values(byKey(pipelineReport()));
    const text = JSON.stringify(cards.map((card) => [card.value, card.sub]));
    expect(text).not.toContain("0%");
    expect(text).not.toContain("NaN");
    expect(byKey(pipelineReport()).gate.value).toBe("no data");
    expect(byKey(pipelineReport()).jobs.value).toBe("no data");
  });

  it("names the landing wait as the last attempt's", () => {
    const base = pipelineReport().landing as Landing;
    const landing: Landing = {
      ...base,
      total: 8,
      bad_share: 0.25,
      wait_s: { p50: 600, p95: null, n: 8 },
      wait_s_machine: { p50: 12, p95: null, n: 8 },
    };
    expect(byKey(pipelineReport({ landing })).landing.sub).toBe("wait p50 12 s (all 10 min) · last attempt · 25% went wrong");
  });

  it("counts the stuck jobs and the human verdicts", () => {
    const jobs = pipelineReport().jobs as Jobs;
    const stuck = { ...jobs.stuck, age_over_lifetime: 1, item_stuck: 2 };
    const gate = pipelineReport().gate as Gate;
    const report = pipelineReport({
      jobs: { ...jobs, total: 4, completed: 3, completion_rate: 0.75, stuck },
      gate: { ...gate, terminal: 10, pass_rate: 0.9, first_pass_rate: 0.7 },
    });
    const cards = byKey(report);
    expect(cards.jobs.sub).toBe("3 of 4 completed · 3 stuck");
    expect(cards.gate.sub).toBe("first try 70%");
    expect(cards.autopilot.spark).toBeUndefined();
  });
});
