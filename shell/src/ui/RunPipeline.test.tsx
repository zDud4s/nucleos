import { describe, expect, it } from "vitest";
import { render } from "@testing-library/react";

import { RunPipeline, runStages, type RunShape } from "./RunPipeline";
import { readState } from "./state-map";

/**
 * The two ways a picture of a run lies, and the one thing on it that moves.
 *
 * A diagram is where §7's distinctions are easiest to lose: a stage has one box whatever it
 * means, so an absent gate and a failed gate are one rectangle apart, and a run that was killed
 * before it reached the gate looks exactly like a run whose project has no suite. Both of those
 * are decided in `runStages`, which is why it is pure and asserted here without rendering.
 */

const RUN: RunShape = {
  status: "running",
  project_id: "nucleos",
  steerable: true,
  num_turns: 14,
  gate_status: null,
  gate_exit_code: null,
  exit_code: null,
  successor_run_id: null,
};

const run = (over: Partial<RunShape> = {}): RunShape => ({ ...RUN, ...over });
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
  const drawn = (props: { run?: RunShape; alive: boolean }) =>
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
    for (const box of boxes) {
      const x = Number(box.getAttribute("x"));
      const width = Number(box.getAttribute("width"));
      const y = Number(box.getAttribute("y"));
      const height = Number(box.getAttribute("height"));
      expect(x).toBeGreaterThanOrEqual(0);
      expect(x + width).toBeLessThanOrEqual(720);
      expect(y).toBeGreaterThanOrEqual(0);
      expect(y + height).toBeLessThanOrEqual(96);
    }
  });

  it("names itself for a reader who cannot see it", () => {
    const container = drawn({ alive: true });
    const label = container.querySelector("svg")!.getAttribute("aria-label")!;

    expect(label).toContain("agent running");
    expect(label).toContain("prompt nucleos");
  });
});
