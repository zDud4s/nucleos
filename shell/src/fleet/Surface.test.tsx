import type { ReactNode } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";

const flow = vi.hoisted(() => ({
  fitView: vi.fn(),
  setCenter: vi.fn(),
  zoom: 1,
  initialized: true,
  viewportInitialized: true,
  flowNodes: [] as Array<{
    id: string;
    position: { x: number; y: number };
    width?: number;
    height?: number;
    measured?: { width?: number; height?: number };
    handles?: Array<{ type: string }>;
  }>,
  props: {} as Record<string, unknown>,
  renders: [] as Array<{ nodeTypes: unknown; edgeTypes: unknown }>,
}));
vi.mock("@xyflow/react", async (original) =>
  (await import("./xyflow-stand-in")).standIn(await original<typeof import("@xyflow/react")>(), flow),
);

const layoutSpy = vi.hoisted(() => ({ calls: [] as Array<{ size: string }> }));
vi.mock("./layout", async (original) => {
  const real = await original<typeof import("./layout")>();
  return {
    ...real,
    layoutForest: (nodes: Parameters<typeof real.layoutForest>[0], options: Parameters<typeof real.layoutForest>[1]) => {
      layoutSpy.calls.push({ size: options.size });
      return real.layoutForest(nodes, options);
    },
  };
});

import { FLEET_EDGE_TYPES, FLEET_NODE_TYPES, FleetCanvas, type FleetCanvasProps } from "./Surface";
import { FleetStoreContext } from "./hooks";
import { NODE_W, NODE_H, BAND_HEAD } from "./layout";
import { FleetStore } from "./store";
import { FleetUiContext } from "./ui-context";
import { child, line, metricsOf, node, snapshot } from "./test-fixtures";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

const props: FleetCanvasProps = { project: null, onlyActive: false, expanded: new Set(), follow: false, fitSignal: 0 };
const select = vi.fn();

function ui(children: ReactNode, store: FleetStore) {
  return (
    <FleetStoreContext.Provider value={store}>
      <FleetUiContext.Provider value={{ path: new Set(), openPanel: vi.fn(), toggleGroup: vi.fn(), select }}>
        {children}
      </FleetUiContext.Provider>
    </FleetStoreContext.Provider>
  );
}

function seeded() {
  const store = new FleetStore();
  const root = node("r", { label: "root" });
  const a = child("a", root, { label: "alpha", state: "idle" });
  const done = child("d", root, { label: "old", state: "done", started_at: "2026-10-20T10:03:00Z" });
  store.apply(snapshot([root, a, done]));
  return { store, root, a, done };
}

beforeEach(() => {
  flow.fitView.mockReset();
  flow.setCenter.mockReset();
  flow.zoom = 1;
  flow.initialized = true;
  flow.viewportInitialized = true;
  flow.renders.length = 0;
  layoutSpy.calls.length = 0;
  select.mockReset();
});
afterEach(cleanup);

describe("FleetCanvas", () => {
  it("hands xyflow fixed-size nodes at the layout's positions, and draws a band per project", () => {
    const { store } = seeded();
    render(ui(<FleetCanvas {...props} />, store));
    const root = flow.flowNodes.find((n) => n.id === "r")!;
    expect(root.position.y).toBe(BAND_HEAD);
    expect([root.width, root.height]).toEqual([NODE_W, NODE_H]);
    // `measured` is what xyflow's initialized check reads and what survives a re-layout.
    expect(flow.flowNodes.every((n) => n.measured?.width === n.width && n.measured?.height === n.height)).toBe(true);
    // Edge anchors are declared from the fixed size: in and out on a terminal node.
    expect(root.handles?.map((h) => h.type)).toEqual(["target", "source"]);
    expect(screen.getByText("nucleos")).toBeDefined(); // the band label
    expect(screen.getByRole("article", { name: /^root/ })).toBeDefined();
  });

  it("frames the live nodes once on first open and never again by itself", () => {
    const { store, root } = seeded();
    render(ui(<FleetCanvas {...props} />, store));
    expect(flow.fitView).toHaveBeenCalledTimes(1);
    const ids = (flow.fitView.mock.calls[0][0].nodes as Array<{ id: string }>).map((n) => n.id).sort();
    expect(ids).toEqual(["a", "r"]); // busy + idle, not the done one
    act(() => store.apply({ t: "node", node: child("z", root, { label: "new" }) }));
    expect(flow.fitView).toHaveBeenCalledTimes(1);
  });

  it("waits for the viewport as well as for the nodes before the first frame", () => {
    flow.viewportInitialized = false;
    const { store } = seeded();
    const view = render(ui(<FleetCanvas {...props} />, store));
    expect(flow.fitView).not.toHaveBeenCalled();
    flow.viewportInitialized = true;
    view.rerender(ui(<FleetCanvas {...props} />, store));
    expect(flow.fitView).toHaveBeenCalledTimes(1);
  });

  it("the Fit signal frames everything", () => {
    const { store } = seeded();
    const view = render(ui(<FleetCanvas {...props} fitSignal={0} />, store));
    flow.fitView.mockClear();
    view.rerender(ui(<FleetCanvas {...props} fitSignal={1} />, store));
    expect(flow.fitView).toHaveBeenCalledTimes(1);
    expect(flow.fitView.mock.calls[0][0].nodes).toBeUndefined();
  });

  it("a canvas that remounts with a Fit signal already pressed does not Fit again by itself", () => {
    const { store } = seeded();
    render(ui(<FleetCanvas {...props} fitSignal={3} />, store));
    expect(flow.fitView).toHaveBeenCalledTimes(1); // the first-open frame, nothing else
    expect(flow.fitView.mock.calls[0][0].nodes).toBeDefined();
  });

  it("hands xyflow stable node and edge types, no double-click zoom, and a no-op onNodesChange", () => {
    const { store } = seeded();
    const view = render(ui(<FleetCanvas {...props} />, store));
    view.rerender(ui(<FleetCanvas {...props} fitSignal={1} />, store));
    expect(flow.renders.length).toBeGreaterThanOrEqual(2);
    for (const seen of flow.renders) {
      expect(seen.nodeTypes).toBe(FLEET_NODE_TYPES);
      expect(seen.edgeTypes).toBe(FLEET_EDGE_TYPES);
    }
    expect(flow.props.zoomOnDoubleClick).toBe(false);
    expect(typeof flow.props.onNodesChange).toBe("function");
  });

  it("Follow off never moves the viewport; Follow on centers the node that spoke last", () => {
    const { store } = seeded();
    const view = render(ui(<FleetCanvas {...props} />, store));
    act(() => store.apply({ t: "lines", node_id: "a", lines: [line("text", "hi")] }));
    expect(flow.setCenter).not.toHaveBeenCalled();

    view.rerender(ui(<FleetCanvas {...props} follow />, store));
    const spoke = flow.flowNodes.find((n) => n.id === "a")!;
    expect(flow.setCenter).toHaveBeenLastCalledWith(spoke.position.x + NODE_W / 2, spoke.position.y + NODE_H / 2, expect.objectContaining({ zoom: 1 }));
    flow.setCenter.mockClear();
    act(() => store.apply({ t: "lines", node_id: "r", lines: [line("text", "yo")] }));
    expect(flow.setCenter).toHaveBeenCalledTimes(1);
  });

  it("Follow tolerates a last-active node that a new snapshot removed", () => {
    const { store, root } = seeded();
    render(ui(<FleetCanvas {...props} follow />, store));
    act(() => store.apply({ t: "lines", node_id: "a", lines: [line("text", "hi")] }));
    flow.setCenter.mockClear();
    act(() => store.apply(snapshot([root])));
    expect(store.getActive()).toBeNull();
    expect(flow.setCenter).not.toHaveBeenCalled();
  });

  it("lines, metrics and state-only events never run the layout; a new node does", () => {
    const { store, root, a } = seeded();
    render(ui(<FleetCanvas {...props} />, store));
    layoutSpy.calls.length = 0;
    act(() => {
      store.apply({ t: "lines", node_id: "a", lines: [line("text", "x")] });
      store.apply({ t: "metrics", metrics: [metricsOf("a")] });
      store.apply({ t: "node", node: { ...a, state: "busy" } });
    });
    expect(layoutSpy.calls).toHaveLength(0);
    act(() => store.apply({ t: "node", node: child("n", root) }));
    expect(layoutSpy.calls).toHaveLength(1);
  });

  it("lays out for cards below 60% zoom", () => {
    flow.zoom = 0.4;
    const { store } = seeded();
    render(ui(<FleetCanvas {...props} />, store));
    expect(layoutSpy.calls[layoutSpy.calls.length - 1]?.size).toBe("card");
  });

  it("clicking empty canvas clears the selection", () => {
    const { store } = seeded();
    render(ui(<FleetCanvas {...props} />, store));
    fireEvent.click(screen.getByTestId("flow"));
    expect(select).toHaveBeenCalledWith(null);
  });
});
