// §spec motor-de-workflows
import { describe, expect, it } from "vitest";
import { graphNode } from "../test/harness";
import type { GraphEdge } from "../data/workflow-graph";
import { buildWorkflow, inSequence, layerOf, nodeMeaning, nodeShape, placed, SHAPES } from "./workflow-model";

/**
 * The layout, without a canvas.
 *
 * §13 names the overlay's resolution as one of the pure things worth testing without a render; this
 * is its neighbour. A workflow's layout IS its content — the sequence is the whole point — so a
 * layout bug is a picture that says something false, which is worse than one that looks untidy.
 */

const NODES = [
  graphNode({ id: "triage", type: "decision" }),
  graphNode({ id: "plan" }),
  graphNode({ id: "gate", type: "command", role: "gate" }),
  graphNode({ id: "rescue" }),
  graphNode({ id: "land", type: "command" }),
];

const EDGES: GraphEdge[] = [
  { from: "triage", to: "plan", when: "size != trivial" },
  { from: "plan", to: "gate" },
  { from: "gate", to: "land", verdict: "pass" },
  { from: "gate", to: "rescue", verdict: "fail" },
];

describe("layerOf", () => {
  it("puts a node after everything that can reach it", () => {
    const layer = layerOf(NODES, EDGES);
    expect(layer).toEqual({ triage: 0, plan: 1, gate: 2, rescue: 3, land: 3 });
  });

  /**
   * The longest path and not the first one found. A node reached both directly and through a long
   * chain belongs at the end of the long one — the short answer draws an edge running backwards,
   * which reads as a loop that is not there.
   */
  it("uses the longest path, so no edge is drawn running backwards", () => {
    const nodes = ["a", "b", "c", "d"].map((id) => graphNode({ id }));
    const layer = layerOf(nodes, [
      { from: "a", to: "d" },
      { from: "a", to: "b" },
      { from: "b", to: "c" },
      { from: "c", to: "d" },
    ]);
    expect(layer.d).toBe(3);
  });

  /**
   * A loop is a real workflow — a rescue that returns to the gate is the obvious one — so this has
   * to terminate rather than refuse. A layout that would not draw a looping workflow would be
   * refusing to draw the interesting ones.
   */
  it("terminates on a cycle instead of hanging or throwing", () => {
    const nodes = ["a", "b"].map((id) => graphNode({ id }));
    const layer = layerOf(nodes, [
      { from: "a", to: "b" },
      { from: "b", to: "a" },
    ]);
    expect(Number.isFinite(layer.a)).toBe(true);
    expect(Number.isFinite(layer.b)).toBe(true);
  });
});

describe("placed", () => {
  it("reads left to right along the sequence, and stacks what shares a column", () => {
    const at = placed(NODES, EDGES);
    expect(at.triage.x).toBeLessThan(at.plan.x);
    expect(at.plan.x).toBeLessThan(at.gate.x);
    // `rescue` and `land` are both one step past the gate, so they share a column and not a cell.
    expect(at.rescue.x).toBe(at.land.x);
    expect(at.rescue.y).not.toBe(at.land.y);
  });

  /**
   * The bundle's own order decides who is on top, because somebody chose it when they wrote the
   * file. Sorting by id would throw that away for nothing.
   */
  it("keeps the bundle's order within a column", () => {
    const at = placed(NODES, EDGES);
    expect(at.rescue.y).toBeLessThan(at.land.y);
  });
});

describe("inSequence", () => {
  it("reads the graph in the order the miniature draws it", () => {
    expect(inSequence(NODES, EDGES).map((node) => node.id)).toEqual([
      "triage",
      "plan",
      "gate",
      "rescue",
      "land",
    ]);
  });
});

describe("nodeShape", () => {
  /**
   * §6.4's two questions — where the money goes, where the pipeline stops — answered by shape, and
   * no longer by colour: every hue this system has is a state, and a gate drawn permanently amber
   * spent the one tone that means "this needs you" on something that never does. Five kinds, five
   * shapes, and the role beats the kind.
   */
  it("gives every kind its own shape, and draws a gate as a gate whatever it runs", () => {
    const shapes = [
      nodeShape("agent", "plain"),
      nodeShape("command", "plain"),
      nodeShape("command", "gate"),
      nodeShape("decision", "plain"),
      nodeShape("fan", "plain"),
    ];
    expect(new Set(shapes).size).toBe(5);
    expect(nodeShape("agent", "gate")).toBe("gate");
    // The key lists every shape the canvas can draw, so none of them is a vocabulary without a key.
    expect([...SHAPES].sort()).toEqual([...new Set(shapes)].sort());
  });

  it("says what each kind is in words, for the ones who read the label", () => {
    const said = (["agent", "command", "decision", "fan"] as const).map((kind) =>
      nodeMeaning(kind, "plain"),
    );
    expect(new Set(said).size).toBe(4);
    expect(nodeMeaning("command", "gate")).not.toBe(nodeMeaning("command", "plain"));
  });
});

describe("buildWorkflow", () => {
  it("lights exactly the node a run is on, and nothing when nothing knows", () => {
    const lit = buildWorkflow(NODES, EDGES, "gate");
    expect(lit.nodes.filter((node) => node.data.running).map((node) => node.id)).toEqual(["gate"]);
    expect(buildWorkflow(NODES, EDGES, null).nodes.every((node) => !node.data.running)).toBe(true);
  });

  /**
   * §6.2: a node switched off here stays in the graph, and so do its edges — dimmed rather than
   * removed. Dropping them would make the picture lie about what the workflow is.
   */
  it("keeps the edges of a node this project switched off, and marks them", () => {
    const nodes = [...NODES.slice(0, 3), graphNode({ id: "rescue", disabled: true }), NODES[4]];
    const model = buildWorkflow(nodes, EDGES, null);
    expect(model.edges.length).toBe(EDGES.length);
    const toRescue = model.edges.find((edge) => edge.target === "rescue");
    expect(toRescue?.data?.dimmed).toBe(true);
    expect(model.edges.find((edge) => edge.target === "land")?.data?.dimmed).toBe(false);
  });

  /**
   * xyflow focuses its own wrapper and names it from the node object, so the name lives there — with
   * the overlay and the run in it, because those are drawn and a screen reader must hear them too.
   */
  it("names each node where keyboard focus lands, with what the picture marks on it", () => {
    const nodes = [graphNode({ id: "plan", label: "Plan", disabled: true, overridden: true })];
    const [plan] = buildWorkflow(nodes, [], "plan").nodes;
    expect(plan.ariaLabel).toBe(
      "Plan, a model is asked to do this, off in this project, changed by this project, running",
    );
  });

  /**
   * A gate's `pass` and `fail` landing on one node is unusual and legal, and an edge id built from
   * the two ends alone would silently draw one of them.
   */
  it("keeps two edges between the same pair as two edges", () => {
    const model = buildWorkflow(NODES, [
      { from: "gate", to: "land", verdict: "pass" },
      { from: "gate", to: "land", verdict: "fail" },
    ], null);
    expect(new Set(model.edges.map((edge) => edge.id)).size).toBe(2);
  });
});
