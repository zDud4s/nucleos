import { createElement, type ComponentType, type ReactNode } from "react";
import type { Mock } from "vitest";

type Xy = typeof import("@xyflow/react");

export interface FlowProbe {
  fitView: Mock;
  setCenter: Mock;
  /** The zoom the stand-in reports to `useStore` (card mode is below 0.6). */
  zoom: number;
  initialized: boolean;
  /** What `useReactFlow().viewportInitialized` reports (xyflow: false until the pane has mounted). */
  viewportInitialized: boolean;
  /** The nodes the surface last handed to <ReactFlow>. */
  flowNodes: Array<{
    id: string;
    position: { x: number; y: number };
    width?: number;
    height?: number;
    measured?: { width?: number; height?: number };
    handles?: Array<{ type: string }>;
  }>;
  /** The props of the last <ReactFlow> render. */
  props: Record<string, unknown>;
  /** `nodeTypes`/`edgeTypes` of every <ReactFlow> render, to prove they are one stable object. */
  renders: Array<{ nodeTypes: unknown; edgeTypes: unknown }>;
}

interface StandInProps {
  nodes: Array<{ id: string; type: string; data: unknown; position: { x: number; y: number }; width?: number; height?: number }>;
  nodeTypes: Record<string, ComponentType<never>>;
  edgeTypes?: unknown;
  onPaneClick?: () => void;
  children?: ReactNode;
}

/**
 * xyflow without its layout engine, for tests: <ReactFlow> renders each node through the given
 * `nodeTypes` (so the real node components run) inside `data-testid="flow"`, and the viewport hooks
 * read a probe the test controls. Used by `Surface.test.tsx` and `pages/Fleet.test.tsx`.
 */
export function standIn(real: Xy, probe: FlowProbe): Xy {
  const Flow = (props: StandInProps) => {
    probe.flowNodes = props.nodes;
    probe.props = props as unknown as Record<string, unknown>;
    probe.renders.push({ nodeTypes: props.nodeTypes, edgeTypes: props.edgeTypes });
    return createElement(
      "div",
      {
        "data-testid": "flow",
        // A click on empty canvas only: xyflow does not fire onPaneClick for a click on a node.
        onClick: (event: { target: unknown; currentTarget: unknown }) => {
          if (event.target === event.currentTarget) props.onPaneClick?.();
        },
      },
      props.nodes.map((n) =>
        createElement(props.nodeTypes[n.type] as ComponentType<Record<string, unknown>>, { key: n.id, id: n.id, data: n.data }),
      ),
      props.children,
    );
  };
  const wrap = ({ children }: { children?: ReactNode }) => createElement("div", null, children);
  // One object for the whole run, like xyflow's memoized instance: a fresh object per render would
  // make every effect that lists it as a dependency fire on every render.
  const instance = {
    fitView: probe.fitView,
    setCenter: probe.setCenter,
    getZoom: () => probe.zoom,
    get viewportInitialized() {
      return probe.viewportInitialized;
    },
  };
  return {
    ...real,
    ReactFlow: Flow,
    ReactFlowProvider: wrap,
    ViewportPortal: wrap,
    Background: () => null,
    Controls: () => null,
    Handle: () => null,
    useStore: (selector: (s: { transform: [number, number, number] }) => unknown) =>
      selector({ transform: [0, 0, probe.zoom] }),
    useReactFlow: () => instance,
    useNodesInitialized: () => probe.initialized,
  } as unknown as Xy;
}
