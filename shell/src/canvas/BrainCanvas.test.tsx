import { createElement } from "react";
import { render, screen, within } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

const seen = vi.hoisted(() => ({
  nodeTypes: [] as unknown[],
  fitView: vi.fn(),
}));

vi.mock("@xyflow/react", async (original) => {
  const real = await original<typeof import("@xyflow/react")>();
  return {
    ...real,
    ReactFlow: (props: Record<string, unknown>) => {
      seen.nodeTypes.push(props.nodeTypes);
      return createElement("div", { "data-testid": "flow" });
    },
    useNodesInitialized: () => true,
    useReactFlow: () => ({ fitView: seen.fitView }),
  };
});

import { BrainCanvas, BRAIN_NODE_TYPES } from "./BrainCanvas";
import type { BrainFilters } from "../data/brain-graph";

const filters: BrainFilters = {
  knowledge: "linked",
  linkTypes: new Set(["relates"]),
  kinds: new Set(["note"]),
  showArchived: false,
};

function canvas() {
  return (
    <BrainCanvas
      nodes={[]}
      edges={[]}
      filters={filters}
      onFilters={() => {}}
      meta={{ notes: 0, knowledge: 0, entities: 0, edges: 0 }}
      selectedId={null}
      onSelect={() => {}}
    />
  );
}

describe("BrainCanvas", () => {
  it("the first view is framed with a zoom floor of 0.8", () => {
    render(canvas());
    expect(seen.fitView).toHaveBeenCalledTimes(1);
    expect(seen.fitView).toHaveBeenCalledWith({ minZoom: 0.8, maxZoom: 1, padding: 0.08 });
  });

  it("the legend is outside the canvas", () => {
    render(canvas());
    const legend = screen.getByRole("list", { name: "Legend" });
    expect(within(screen.getByTestId("brain-canvas")).queryByRole("list", { name: "Legend" })).toBeNull();
    expect(screen.getByTestId("brain-bar").contains(legend)).toBe(true);
  });

  it("nodeTypes keep their identity across renders", () => {
    seen.nodeTypes.length = 0;
    const { rerender } = render(canvas());
    rerender(canvas());
    expect(seen.nodeTypes.length).toBeGreaterThan(1);
    expect(new Set(seen.nodeTypes).size).toBe(1);
    expect(seen.nodeTypes[0]).toBe(BRAIN_NODE_TYPES);
  });
});
