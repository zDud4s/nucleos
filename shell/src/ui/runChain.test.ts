import { describe, expect, it } from "vitest";
import { COLLAPSE_AT, chainLegs, partOfText, reasonLabel, type ChainLeg } from "./runChain";
import { runStages } from "./runStages";
import { readState } from "./state-map";

function leg(id: number, over: Partial<ChainLeg> = {}): ChainLeg {
  return {
    id,
    status: "completed",
    num_turns: null,
    started_at: null,
    ended_at: null,
    ended_because: null,
    gate_status: null,
    gate_exit_code: null,
    exit_code: null,
    ...over,
  };
}
const run = (lineage: ChainLeg[], id: number, truncated = false) => ({
  id,
  lineage,
  lineage_truncated: truncated,
});
const many = (n: number) => Array.from({ length: n }, (_, i) => leg(i + 1));
const kinds = (c: ReturnType<typeof chainLegs>) => c.items.map((i) => i.kind);
const ids = (c: ReturnType<typeof chainLegs>) =>
  c.items.map((i) => (i.kind === "leg" ? i.id : "gap"));

describe("chainLegs", () => {
  it("draws a single leg as current, with the same gate and result as runStages", () => {
    const l = leg(5, { gate_status: "passed", gate_exit_code: 0, exit_code: 0 });
    const chain = chainLegs(run([l], 5));
    expect(chain.items).toHaveLength(1);
    expect(chain.items[0]).toMatchObject({ kind: "leg", id: 5, current: true });
    const shape = {
      ...l,
      project_id: null,
      steerable: false,
      successor_run_id: null,
    };
    const stages = runStages(shape);
    expect(chain.gate).toEqual(stages.find((s) => s.key === "gate"));
    expect(chain.result).toEqual(stages.find((s) => s.key === "result"));
  });

  it("labels the reason each leg ended and marks only the current one", () => {
    const legs = [
      leg(1, { status: "superseded", ended_because: "context_full", num_turns: 3 }),
      leg(2, { status: "superseded", ended_because: "approval_approved" }),
      leg(3),
    ];
    const chain = chainLegs(run(legs, 2));
    expect(ids(chain)).toEqual([1, 2, 3]);
    const items = chain.items as Extract<(typeof chain.items)[number], { kind: "leg" }>[];
    expect(items[0].sub).toContain("context full");
    expect(items[1].sub).toContain("approved");
    expect(items.map((i) => i.current)).toEqual([false, true, false]);
    expect(items[0].tone).toBe(readState("run", "superseded")?.tone);
    expect(items[0].tone).toBe("off");
  });

  it("invents no reason for a superseded leg without a known one", () => {
    const chain = chainLegs(run([leg(1, { status: "superseded" }), leg(2)], 1));
    const first = chain.items[0];
    if (first.kind !== "leg") throw new Error("leg expected");
    expect(first.title).toBe("superseded");
    expect(first.sub).not.toMatch(/context full|approved|declined/);
  });

  it("takes gate and result from the last leg", () => {
    const legs = [
      leg(1, { status: "superseded" }),
      leg(2, { gate_status: "passed", gate_exit_code: 0, exit_code: 0 }),
    ];
    const chain = chainLegs(run(legs, 1));
    expect(chain.gate.title).toBe(readState("gate", "passed")?.label);
    expect(chain.gate.reached).toBe(true);
    expect(chain.result.title).toBe("exit 0");
  });

  it("reads a live last leg as not yet at the gate, from any viewpoint", () => {
    const legs = [leg(1, { status: "superseded" }), leg(2, { status: "running" })];
    const a = chainLegs(run(legs, 1));
    const b = chainLegs(run(legs, 2));
    for (const c of [a, b]) {
      expect(c.gate.title).toBe("not reached");
      expect(c.gate.reached).toBe(false);
      expect(c.result.reached).toBe(false);
    }
    expect(a.items[1]).toMatchObject({ kind: "leg", alive: true });
    expect(a.items[0]).toMatchObject({ kind: "leg", alive: false });
  });

  it("collapses a long chain to first, current and last", () => {
    const c = chainLegs(run(many(12), 6));
    expect(kinds(c)).toEqual(["leg", "gap", "leg", "gap", "leg"]);
    expect(ids(c)).toEqual([1, "gap", 6, "gap", 12]);
    const gaps = c.items.filter((i) => i.kind === "gap");
    expect(gaps[0]).toEqual({ kind: "gap", hiddenIds: [2, 3, 4, 5] });
    expect(gaps[1]).toEqual({ kind: "gap", hiddenIds: [7, 8, 9, 10, 11] });
  });

  it("merges current into the first leg and never draws an empty gap", () => {
    expect(ids(chainLegs(run(many(12), 1)))).toEqual([1, "gap", 12]);
    expect(ids(chainLegs(run(many(12), 2)))).toEqual([1, 2, "gap", 12]);
    expect(ids(chainLegs(run(many(12), 11)))).toEqual([1, "gap", 11, 12]);
  });

  it("draws a chain of COLLAPSE_AT legs in full, and an expanded one too", () => {
    expect(kinds(chainLegs(run(many(COLLAPSE_AT), 3)))).not.toContain("gap");
    const full = chainLegs(run(many(12), 6), true);
    expect(full.items.filter((i) => i.kind === "leg")).toHaveLength(12);
    expect(kinds(full)).not.toContain("gap");
  });

  it("mirrors truncation and returns nothing for an empty lineage", () => {
    expect(chainLegs(run(many(2), 1, true)).truncated).toBe(true);
    expect(chainLegs(run(many(2), 1)).truncated).toBe(false);
    expect(chainLegs({ id: 1 }).items).toEqual([]);
    expect(chainLegs(run([], 1)).items).toEqual([]);
  });
});

describe("partOfText", () => {
  it("names a tree node", () => {
    expect(
      partOfText({
        kind: "tree",
        tree_id: "t",
        tree_title: "Tree A",
        node_id: "n",
        node_title: "Leaf B",
      }),
    ).toBe('Part of tree "Tree A" > node "Leaf B"');
  });
  it("names a job item, or the job alone when there is no item", () => {
    const job = { kind: "job" as const, job_id: 1, job_title: "nightly" };
    expect(partOfText({ ...job, item_id: 9, item_ordinal: 3 })).toBe(
      'Part of job "nightly" > item 3',
    );
    expect(partOfText({ ...job, item_id: null, item_ordinal: null })).toBe('Part of job "nightly"');
  });
  it("is null without a parent", () => {
    expect(partOfText(null)).toBeNull();
  });
});

describe("reasonLabel", () => {
  it("maps the known reasons and nothing else", () => {
    expect(reasonLabel("context_full")).toBe("context full");
    expect(reasonLabel("approval_approved")).toBe("approved");
    expect(reasonLabel("approval_declined")).toBe("declined");
    expect(reasonLabel("from_the_future")).toBeNull();
    expect(reasonLabel(null)).toBeNull();
  });
});
