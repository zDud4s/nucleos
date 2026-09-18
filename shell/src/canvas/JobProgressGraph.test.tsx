import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";
import type { Job, JobItem } from "../data/fleet";
import { JobProgressGraph, JobProgressLine } from "./JobProgressGraph";

afterEach(cleanup);

function job(over: Partial<Job> = {}): Job {
  return {
    id: 7,
    project_id: "nucleos",
    rule_name: null,
    status: "running",
    wait_reason: null,
    max_items: 8,
    created_at: "2026-09-01T00:00:00Z",
    completed_at: null,
    slot: 0,
    round: 0,
    max_rounds: 1,
    team_id: null,
    team_name: null,
    team_max_parallel: null,
    ...over,
  } as Job;
}

function item(over: Partial<JobItem> = {}): JobItem {
  return {
    ordinal: 0,
    description: "do the thing",
    status: "pending",
    round: 0,
    run_id: null,
    gate_status: null,
    agent_id: null,
    agent_name: null,
    depends_on: [],
    files: [],
    ...over,
  };
}

/** The `<title>` of the group whose label contains `text`. jsdom computes no layout, so the
 *  drawing cannot be asserted on visually -- what it CAN assert is that every fact reached the
 *  node, which is the half that goes wrong silently. */
function titleNear(text: string): string {
  const label = screen.getByText(text);
  const group = label.closest("g");
  return group?.querySelector("title")?.textContent ?? "";
}

describe("JobProgressGraph", () => {
  it("draws a box per item plus the plan node", () => {
    const { container } = render(
      <JobProgressGraph
        job={job()}
        items={[item({ ordinal: 0 }), item({ ordinal: 1, depends_on: [0] })]}
      />,
    );
    expect(container.querySelectorAll(".jp-node")).toHaveLength(3);
  });

  it("draws an edge for a dependency", () => {
    const { container } = render(
      <JobProgressGraph
        job={job()}
        items={[item({ ordinal: 0 }), item({ ordinal: 1, depends_on: [0] })]}
      />,
    );
    // Two items, one depending on the other: plan→0 and 0→1. A list can show neither.
    expect(container.querySelectorAll(".jp-edge").length).toBeGreaterThanOrEqual(2);
  });

  it("paints each lifecycle with its own class", () => {
    const { container } = render(
      <JobProgressGraph
        job={job()}
        items={[
          item({ ordinal: 0, status: "passed" }),
          item({ ordinal: 1, status: "running" }),
          item({ ordinal: 2, status: "skipped" }),
          item({ ordinal: 3, status: "cancelled" }),
          item({ ordinal: 4, status: "failed" }),
        ]}
      />,
    );
    // Scoped to the NODES. The legend renders all seven classes by design, so an unscoped query
    // here would pass against boxes that carried no class at all — the assertion would be about
    // the key rather than about the drawing.
    for (const life of ["done", "running", "waiting", "withdrawn", "stopped"]) {
      expect(container.querySelectorAll(`.jp-nodes .jp-${life}`).length).toBeGreaterThan(0);
    }
  });

  it("never gives withdrawn work the failure class", () => {
    // The distinction `state-map.ts` and `job.rs` both insist on, asserted where it is actually
    // rendered rather than only where it is decided.
    const { container } = render(
      <JobProgressGraph job={job()} items={[item({ status: "cancelled" })]} />,
    );
    expect(container.querySelectorAll(".jp-nodes .jp-stopped")).toHaveLength(0);
    expect(container.querySelectorAll(".jp-nodes .jp-withdrawn")).toHaveLength(1);
  });

  it("marks an undecided gate verdict rather than picking a side", () => {
    const { container } = render(
      <JobProgressGraph job={job()} items={[item({ status: "gate_failed" })]} />,
    );
    expect(container.querySelectorAll(".jp-nodes .jp-undecided")).toHaveLength(1);
    expect(titleNear("do the thing")).toContain("may still owe it another attempt");
  });

  it("puts the facts a box has no room for into its title", () => {
    render(
      <JobProgressGraph
        job={job()}
        items={[
          item({
            description: "widen the gate",
            status: "running",
            agent_name: "Ana",
            run_id: 42,
            gate_status: "passed",
            files: ["core/src/gate.rs"],
          }),
        ]}
      />,
    );
    const title = titleNear("widen the gate");
    expect(title).toContain("given to Ana");
    expect(title).toContain("run 42");
    expect(title).toContain("gate: passed");
    expect(title).toContain("core/src/gate.rs");
  });

  it("labels the whole drawing for a reader who cannot see it", () => {
    render(<JobProgressGraph job={job()} items={[item({ status: "passed" })]} />);
    expect(screen.getByRole("img").getAttribute("aria-label")).toContain("job 7");
    expect(screen.getByRole("img").getAttribute("aria-label")).toContain("1/1 done");
  });
});

describe("JobProgressLine", () => {
  it("says how far along without opening anything", () => {
    render(
      <JobProgressLine
        job={job()}
        items={[item({ ordinal: 0, status: "passed" }), item({ ordinal: 1, status: "running" })]}
      />,
    );
    expect(screen.getByText(/1\/2 done/)).toBeTruthy();
  });

  it("draws no bar for a job whose planner found nothing", () => {
    const { container } = render(<JobProgressLine job={job({ status: "completed" })} items={[]} />);
    expect(container.querySelectorAll(".jp-bar")).toHaveLength(0);
    expect(screen.getByText(/found no work/)).toBeTruthy();
  });

  it("gives a segment only to a bucket that has something in it", () => {
    const { container } = render(
      <JobProgressLine job={job()} items={[item({ status: "passed" })]} />,
    );
    // One item, all done: one segment. An empty bucket with a zero-width segment is a sliver
    // that reads as a real one at small sizes.
    expect(container.querySelectorAll(".jp-seg")).toHaveLength(1);
  });
});
