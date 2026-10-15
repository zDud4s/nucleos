import { describe, expect, it } from "vitest";
import { layoutMachine } from "./states-model";
import type { StateMachine, StateTransition } from "../data/project-states";

function transition(from: string, to: string, fn: string | null, line = 1): StateTransition {
  return { from, to, function: fn, path: "core/src/q.rs", line };
}

function machine(transitions: StateTransition[]): StateMachine {
  const states = [...new Set(transitions.flatMap((t) => [t.from, t.to]))].filter(
    (s) => s !== "*" && s !== "?",
  );
  return { table: "t", column: "status", states, transitions };
}

describe("the layout of a state machine", () => {
  it("groups two transitions between the same states into one edge with both functions", () => {
    const { edges } = layoutMachine(
      machine([transition("a", "b", "zeta", 3), transition("a", "b", "alpha", 9)]),
    );
    expect(edges).toHaveLength(1);
    expect(edges[0].from).toBe("a");
    expect(edges[0].to).toBe("b");
    expect(edges[0].functions).toEqual(["alpha", "zeta"]);
    expect(edges[0].anchors).toHaveLength(2);
  });

  it("puts the source in column 0 and its successor one column further", () => {
    const { nodes, columns } = layoutMachine(
      machine([transition("a", "b", "f"), transition("b", "c", "g")]),
    );
    const column = (id: string) => nodes.find((n) => n.id === id)?.column;
    expect(column("a")).toBe(0);
    expect(column("b")).toBe(1);
    expect(column("c")).toBe(2);
    expect(columns).toBe(3);
  });

  it("labels the any-state as any and the runtime-bound target as computed", () => {
    const { nodes, edges } = layoutMachine(
      machine([transition("*", "a", "f"), transition("a", "?", null)]),
    );
    expect(nodes.find((n) => n.id === "*")?.label).toBe("any");
    expect(nodes.find((n) => n.id === "?")?.label).toBe("computed");
    expect(nodes.find((n) => n.id === "a")?.label).toBe("a");
    expect(nodes.find((n) => n.id === "*")?.column).toBe(0);
    expect(edges.find((e) => e.to === "?")?.functions).toEqual(["top level"]);
  });

  it("places every state of a cycle that has no source", () => {
    const { nodes } = layoutMachine(machine([transition("a", "b", "f"), transition("b", "a", "g")]));
    expect(nodes.map((n) => n.id).sort()).toEqual(["a", "b"]);
    expect(nodes.find((n) => n.id === "a")?.column).toBe(0);
    expect(nodes.find((n) => n.id === "b")?.column).toBe(1);
  });
});
