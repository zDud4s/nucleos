import { describe, expect, it, vi } from "vitest";
import { fireEvent, render, within } from "@testing-library/react";

import { RunPipeline } from "./RunPipeline";
import type { ChainLeg, ChainRun } from "./runChain";
import { runStages, type RunShape } from "./runStages";
import { readState } from "./state-map";

/**
 * The two ways a picture of a run lies, and the one thing on it that moves.
 *
 * A diagram is where §7's distinctions are easiest to lose: a stage has one box whatever it
 * means, so an absent gate and a failed gate are one rectangle apart, and a run that was killed
 * before it reached the gate looks exactly like a run whose project has no suite. Both of those
 * are decided in `runStages`, which is why it is pure and asserted here without rendering.
 */

const RUN: RunShape & ChainRun = {
  id: 1,
  lineage: [],
  status: "running",
  project_id: "nucleos",
  steerable: true,
  num_turns: 14,
  gate_status: null,
  gate_exit_code: null,
  exit_code: null,
  successor_run_id: null,
};

const run = (over: Partial<RunShape> = {}): RunShape & ChainRun => ({ ...RUN, ...over });
const stage = (over: Partial<RunShape>, key: string) =>
  runStages(run(over)).find((found) => found.key === key)!;

describe("runStages", () => {
  it("draws a completed run with no gate as no gate configured, and never as a failure", () => {
    // The distinction the whole map exists for: a project with no suite is not a broken project,
    // and a red box here tells somebody their code is broken on the evidence of nothing at all.
    const gate = stage({ status: "completed", gate_status: null, exit_code: 0 }, "gate");

    expect(gate.title).toBe("no gate configured");
    expect(gate.tone).toBe("off");
    expect(gate.reached).toBe(true);
  });

  it("draws the gate of an interrupted run as not reached, which is a different absence", () => {
    // Same NULL on the wire, opposite meaning: nothing measured this run because it never got
    // there, so the reading about the project's configuration must not be printed on it.
    const gate = stage({ status: "interrupted", gate_status: null, exit_code: null }, "gate");

    expect(gate.title).toBe("not reached");
    expect(gate.reached).toBe(false);
    expect(gate.tone).toBeNull();
  });

  it("draws the gate of a run that is still going as not reached", () => {
    // The state this page is read in most: a run mid-flight has not been measured yet, and
    // "no gate configured" on it answers a question about the project nobody asked.
    const gate = stage({ status: "running", gate_status: null, exit_code: null }, "gate");

    expect(gate.title).toBe("not reached");
    expect(gate.reached).toBe(false);
  });

  it("takes every tone from the map rather than choosing one", () => {
    for (const status of ["running", "awaiting_approval", "failed", "cancelled", "timed_out"]) {
      const agent = stage({ status }, "agent");
      expect(agent.tone).toBe(readState("run", status)!.tone);
      expect(agent.title).toBe(readState("run", status)!.label);
    }
    const failed = stage({ status: "completed", gate_status: "failed", gate_exit_code: 101 }, "gate");
    expect(failed.tone).toBe(readState("gate", "failed")!.tone);
  });

  it("shows a status the map has no reading for as itself, with no tone borrowed", () => {
    const agent = stage({ status: "quiescing" }, "agent");

    expect(agent.title).toBe("quiescing");
    expect(agent.tone).toBeNull();
  });

  it("says none recorded for a missing exit code instead of exit 0", () => {
    // Absent is not zero. A run killed before it reported has no exit code, and "exit 0" here
    // would be the page inventing the one number somebody opened it to check.
    expect(stage({ exit_code: null }, "result").title).toBe("none recorded");
    expect(stage({ status: "completed", exit_code: 0 }, "result").title).toBe("exit 0");
  });

  it("names the successor of a run that handed its context on", () => {
    expect(stage({ status: "completed", exit_code: 0, successor_run_id: 1483 }, "result").sub).toBe(
      "continued as 1483",
    );
  });

  it("says no project rather than nothing for a run without one", () => {
    expect(stage({ project_id: null }, "prompt").title).toBe("no project");
  });
});

describe("RunPipeline", () => {
  const drawn = (props: { run?: RunShape & ChainRun; alive: boolean }) =>
    render(<RunPipeline run={props.run ?? RUN} alive={props.alive} />).container;

  it("moves only while the run is going", () => {
    // The one argument for motion here: a stage that is HAPPENING and a stage that finished are
    // the same picture when nothing moves. A finished run that still animates is a lie.
    const live = drawn({ alive: true });
    expect(live.querySelectorAll(".ui-runpipe-flow").length).toBeGreaterThan(0);
    expect(live.querySelectorAll(".ui-runpipe-workdot").length).toBeGreaterThan(0);

    const done = drawn({ run: run({ status: "completed", exit_code: 0 }), alive: false });
    expect(done.querySelectorAll(".ui-runpipe-flow")).toHaveLength(0);
    expect(done.querySelectorAll(".ui-runpipe-workdot")).toHaveLength(0);
  });

  it("wears the tone class the map chose, on the stage and on the marks flowing into it", () => {
    const container = drawn({ alive: true });

    expect(container.querySelectorAll(".ui-runpipe-stage.ui-runpipe-active")).toHaveLength(1);
    // The marks are not a stage, so they carry the tone class without the stage class.
    expect(container.querySelector(".ui-runpipe-active .ui-runpipe-flow")).not.toBeNull();
  });

  it("draws every stage inside the box, so nothing is clipped at either end", () => {
    const container = drawn({ alive: false });
    const boxes = [...container.querySelectorAll("rect.ui-runpipe-box")];

    expect(boxes).toHaveLength(4);
    const [, , viewW, viewH] = container.querySelector("svg")!.getAttribute("viewBox")!.split(" ").map(Number);
    for (const box of boxes) {
      const x = Number(box.getAttribute("x"));
      const width = Number(box.getAttribute("width"));
      const y = Number(box.getAttribute("y"));
      const height = Number(box.getAttribute("height"));
      expect(x).toBeGreaterThanOrEqual(0);
      expect(x + width).toBeLessThanOrEqual(viewW);
      expect(y).toBeGreaterThanOrEqual(0);
      expect(y + height).toBeLessThanOrEqual(viewH);
    }
  });

  it("names itself for a reader who cannot see it", () => {
    const container = drawn({ alive: true });
    const label = container.querySelector("svg")!.getAttribute("aria-label")!;

    expect(label).toContain("agent running");
    expect(label).toContain("prompt nucleos");
  });
});

describe("RunPipeline chain", () => {
  const leg = (id: number, over: Partial<ChainLeg> = {}): ChainLeg => ({
    id,
    status: "completed",
    num_turns: 7,
    started_at: null,
    ended_at: null,
    ended_because: null,
    gate_status: null,
    gate_exit_code: null,
    exit_code: 0,
    ...over,
  });
  const chain = (
    legs: ChainLeg[],
    currentId: number,
    extra: Partial<ChainRun> = {},
    over: Partial<RunShape> = {},
  ): RunShape & ChainRun => ({
    ...RUN,
    status: "completed",
    exit_code: 0,
    ...over,
    id: currentId,
    lineage: legs,
    ...extra,
  });
  const many = (n: number) => Array.from({ length: n }, (_, i) => leg(i + 1));
  const drawn = (run: RunShape & ChainRun, onOpenRun?: (id: number) => void, alive = false) =>
    render(<RunPipeline run={run} alive={alive} onOpenRun={onOpenRun} />).container;
  const legFor = (container: HTMLElement, id: number) =>
    [...container.querySelectorAll(".ui-runpipe-leg")].find((g) =>
      g.querySelector(".ui-runpipe-kind")?.textContent === `RUN ${id}`,
    )!;
  const kinds = (container: HTMLElement) =>
    [...container.querySelectorAll(".ui-runpipe-kind")].map((t) => t.textContent);

  it("a single-leg chain renders the four classic stages", () => {
    for (const lineage of [[leg(1)], []]) {
      const container = drawn(chain(lineage, 1));
      expect(container.querySelectorAll(".ui-runpipe-stage")).toHaveLength(4);
      expect(kinds(container)).toEqual(["PROMPT", "AGENT", "GATE", "RESULT"]);
      expect(container.querySelector('[role="link"]')).toBeNull();
    }
  });

  it("draws every leg of a short chain, highlights this run and ends with GATE and RESULT", () => {
    const legs = [leg(1, { status: "superseded", ended_because: "context_full" }), leg(2), leg(3)];
    const container = drawn(chain(legs, 2));

    expect(container.querySelectorAll(".ui-runpipe-leg")).toHaveLength(3);
    const current = legFor(container, 2);
    expect(current.getAttribute("class")).toContain("ui-runpipe-current");
    expect(current.getAttribute("aria-current")).toBe("step");
    expect(legFor(container, 1).getAttribute("aria-current")).toBeNull();
    expect(kinds(container).slice(-2)).toEqual(["GATE", "RESULT"]);
    expect(container.textContent!.match(/context full/g)).toHaveLength(1);
  });

  it("legs are links except the current one", () => {
    const onOpenRun = vi.fn();
    const container = drawn(chain(many(3), 2), onOpenRun);

    expect(container.querySelectorAll('[role="link"]')).toHaveLength(2);
    fireEvent.click(legFor(container, 3));
    expect(onOpenRun).toHaveBeenLastCalledWith(3);
    fireEvent.keyDown(legFor(container, 1), { key: "Enter" });
    expect(onOpenRun).toHaveBeenLastCalledWith(1);
    onOpenRun.mockClear();
    fireEvent.click(legFor(container, 2));
    expect(onOpenRun).not.toHaveBeenCalled();
  });

  it("a leg wears its tone from the map", () => {
    const container = drawn(chain([leg(1, { status: "superseded" }), leg(2)], 2));

    expect(legFor(container, 1).getAttribute("class")).toContain("ui-runpipe-off");
    expect(container.querySelector('[fill^="#"], [stroke^="#"]')).toBeNull();
  });

  it("collapses the middle of a long chain and expands on click", () => {
    const container = drawn(chain(many(12), 6));

    expect(container.querySelectorAll(".ui-runpipe-leg")).toHaveLength(3);
    expect(container.textContent).toContain("… 4 runs");
    expect(container.textContent).toContain("… 5 runs");
    fireEvent.click(container.querySelector(".ui-runpipe-gap")!);
    expect(container.querySelectorAll(".ui-runpipe-leg")).toHaveLength(12);
    expect(container.querySelector(".ui-runpipe-gap")).toBeNull();
  });

  it("the last leg alive draws GATE and RESULT as outlines saying not reached", () => {
    const legs = [leg(1, { ended_because: "context_full", status: "superseded" }), leg(2, { status: "running", exit_code: null })];
    const container = drawn(chain(legs, 2, {}, { status: "running", exit_code: null }), undefined, true);
    const gate = [...container.querySelectorAll(".ui-runpipe-stage")].find(
      (g) => g.querySelector(".ui-runpipe-kind")?.textContent === "GATE",
    )!;

    expect(gate.getAttribute("class")).toContain("ui-runpipe-unreached");
    expect(gate.textContent).toContain("not reached");
    expect(container.querySelectorAll(".ui-runpipe-workdot").length).toBeGreaterThan(0);
  });

  it("a long title is cut and the full text stays in the aria-label", () => {
    const long = "x".repeat(80);
    const container = drawn(chain([leg(1, { status: long }), leg(2)], 2));
    const titles = [...container.querySelectorAll(".ui-runpipe-title")].map((t) => t.textContent!);

    expect(titles.some((t) => t.endsWith("…"))).toBe(true);
    const label = container.querySelector("svg")!.getAttribute("aria-label")!;
    expect(label).toContain("run 1");
    expect(label).toContain("run 2");
  });

  it("a truncated chain says so", () => {
    const container = drawn(chain([leg(1), leg(2)], 2, { lineage_truncated: true }));

    expect(container.textContent).toContain("chain truncated");
  });

  it("the chain svg is a group, so its legs stay reachable as links", () => {
    const container = drawn(chain(many(3), 2), vi.fn());
    const group = within(container).getByRole("group", { name: /^Chain of runs/ });

    expect(group.tagName.toLowerCase()).toBe("svg");
    expect(within(group).getAllByRole("link").map((l) => l.getAttribute("aria-label"))).toEqual([
      "Open run 1",
      "Open run 3",
    ]);
    expect(container.querySelector('svg[role="img"]')).toBeNull();
  });

  it("Space activates a leg link", () => {
    const onOpenRun = vi.fn();
    const container = drawn(chain(many(3), 2), onOpenRun);

    fireEvent.keyDown(legFor(container, 3), { key: " " });
    expect(onOpenRun).toHaveBeenCalledWith(3);
  });

  it("Enter and Space on the gap expand it", () => {
    for (const key of ["Enter", " "]) {
      const { container, unmount } = render(<RunPipeline run={chain(many(12), 6)} alive={false} />);
      fireEvent.keyDown(container.querySelector(".ui-runpipe-gap")!, { key });
      expect(container.querySelectorAll(".ui-runpipe-leg")).toHaveLength(12);
      unmount();
    }
  });

  it("the flow mark leads only into a leg that is alive", () => {
    const still = drawn(chain([leg(1), leg(2)], 2));
    expect(still.querySelectorAll(".ui-runpipe-flow")).toHaveLength(0);

    const legs = [leg(1, { status: "superseded" }), leg(2, { status: "running", exit_code: null })];
    const going = drawn(chain(legs, 2, {}, { status: "running", exit_code: null }), undefined, true);
    const marks = going.querySelectorAll(".ui-runpipe-flow");
    expect(marks).toHaveLength(3);
    expect(marks[0].parentElement!.getAttribute("class")).toContain(
      `ui-runpipe-${readState("run", "running")!.tone}`,
    );
  });
});
