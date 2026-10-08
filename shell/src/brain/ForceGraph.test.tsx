import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";

import { ForceGraph } from "./ForceGraph";
import type { GModel, GNode } from "./graph-types";
import type { Placed } from "./hit-test";

/**
 * Shallow on purpose: the geometry is `hit-test.test.ts`'s job. These only prove the wiring —
 * a canvas with a name, a click on a drawn node reaching `onSelect`, nothing ticking after unmount.
 */

/** jsdom has no 2D context; this one records the method names called on it and accepts any write. */
function stubContext() {
  const calls: string[] = [];
  const target: Record<string, unknown> = {};
  const ctx = new Proxy(target, {
    get(t, prop: string) {
      if (prop in t) return t[prop];
      const fn = (..._args: unknown[]) => {
        calls.push(prop);
      };
      t[prop] = fn;
      return fn;
    },
    set(t, prop: string, value) {
      t[`__${prop}`] = value;
      return true;
    },
  });
  return { ctx, calls };
}

function node(id: string, over: Partial<GNode> = {}): GNode {
  return { id, kind: "note", ref: id, label: id, bucket: "in_force", missing: false, degree: 0, ...over };
}

let recorded: string[];

beforeEach(() => {
  const { ctx, calls } = stubContext();
  recorded = calls;
  vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockImplementation(
    () => ctx as unknown as CanvasRenderingContext2D,
  );
});

afterEach(() => {
  vi.restoreAllMocks();
});

describe("ForceGraph", () => {
  it("renders a focusable canvas named by its node count, and paints it", async () => {
    const model: GModel = { nodes: [node("n:1"), node("n:2")], edges: [] };
    render(<ForceGraph model={model} selected={null} onSelect={() => {}} />);

    const canvas = screen.getByRole("img", { name: "Brain graph, 2 nodes" });
    expect(canvas.tagName).toBe("CANVAS");
    expect(canvas.getAttribute("tabindex")).toBe("0");
    await waitFor(() => expect(recorded).toContain("arc"));
  });

  it("calls onSelect with the node a click lands on, and not for empty canvas", async () => {
    // One node: the centring force parks it at the origin after one tick, so where it was drawn
    // is where it still is when the click lands.
    const model: GModel = { nodes: [node("n:solo")], edges: [] };
    const onSelect = vi.fn();
    let last: Placed[] = [];
    const layouts: Placed[][] = [];
    render(
      <ForceGraph
        model={model}
        selected={null}
        onSelect={onSelect}
        onLayout={(placed) => {
          last = placed;
          layouts.push(placed);
        }}
      />,
    );
    await waitFor(() => expect(layouts.length).toBeGreaterThan(3));

    const canvas = screen.getByRole("img", { name: "Brain graph, 1 nodes" });
    const [solo] = last;
    fireEvent.click(canvas, { clientX: solo.x + solo.r * 2 + 30, clientY: solo.y });
    expect(onSelect).not.toHaveBeenCalled();

    fireEvent.click(canvas, { clientX: solo.x, clientY: solo.y });
    expect(onSelect).toHaveBeenCalledWith("n:solo");
  });

  it("stops the simulation and its frames on unmount", async () => {
    const model: GModel = {
      nodes: [node("n:1", { degree: 1 }), node("k:2", { kind: "knowledge", layer: "semantic", degree: 1 })],
      edges: [{ id: "relates|n:1|k:2", source: "n:1", target: "k:2", type: "relates" }],
    };
    let frames = 0;
    const { unmount } = render(
      <ForceGraph model={model} selected={null} onSelect={() => {}} onLayout={() => (frames += 1)} />,
    );
    await waitFor(() => expect(frames).toBeGreaterThan(1));

    unmount();
    const atUnmount = frames;
    await new Promise((resolve) => setTimeout(resolve, 150));
    expect(frames).toBe(atUnmount);
  });
});
