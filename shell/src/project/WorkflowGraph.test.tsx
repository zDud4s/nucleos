// §spec motor-de-workflows
import { createElement } from "react";
import { QueryClientProvider } from "@tanstack/react-query";
import { describe, expect, it, vi } from "vitest";
import { fireEvent, screen, waitFor } from "@testing-library/react";

/**
 * jsdom has no layout and no CSS transforms, and xyflow constructs a `DOMMatrixReadOnly` on mount.
 * The same stub the fleet canvas's tests use, for the same reason: the absence is a fact about the
 * environment, not a fault in the component.
 */
vi.stubGlobal(
  "DOMMatrixReadOnly",
  class {
    m22 = 1;
    constructor(_transform?: string) {}
  },
);

/**
 * The real xyflow, with a tap on the two props that must never change identity.
 *
 * Wrapping rather than replacing: the canvas really mounts, so this also proves the library renders
 * the workflow's nodes under jsdom at all.
 */
const seen = vi.hoisted(() => ({ nodeTypes: [] as unknown[], edgeTypes: [] as unknown[] }));
vi.mock("@xyflow/react", async (original) => {
  const real = await original<typeof import("@xyflow/react")>();
  return {
    ...real,
    ReactFlow: (props: Record<string, unknown>) => {
      seen.nodeTypes.push(props.nodeTypes);
      seen.edgeTypes.push(props.edgeTypes);
      return createElement(real.ReactFlow as never, props);
    },
  };
});

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
const opener = vi.hoisted(() => ({ openUrl: vi.fn() }));
vi.mock("@tauri-apps/plugin-opener", () => opener);
const daemon = vi.hoisted(() => ({ apiFetch: vi.fn(), apiText: vi.fn(), probeHealth: vi.fn() }));
vi.mock("../data/client", async (original) => ({
  ...(await original<typeof import("../data/client")>()),
  ...daemon,
}));

import {
  bundle,
  daemonFetch,
  daemonState,
  daemonText,
  graphNode,
  installedWorkflow,
  project,
  renderApp,
  renderWithQuery,
  type DaemonState,
} from "../test/harness";
import type { WorkflowGraph as Graph } from "../data/workflow-graph";
import { WorkflowGraph } from "./WorkflowGraph";

function graph(overrides: Partial<Graph> = {}): Graph {
  return {
    source: "library",
    version: "1.0",
    nodes: [
      graphNode({ id: "plan", fields: [{ name: "model", value: "opus" }, { name: "body", value: "skills/plan.md" }] }),
      graphNode({ id: "gate", type: "command", role: "gate", label: "Gate", fields: [{ name: "command", value: "cargo test" }] }),
    ],
    edges: [{ from: "plan", to: "gate" }],
    orphaned: [],
    ...overrides,
  };
}

/** Mount just the graph panel, over a daemon holding one graph. */
function openGraph(overrides: Partial<DaemonState> = {}, props: Partial<Parameters<typeof WorkflowGraph>[0]> = {}) {
  const state = daemonState({ graph: graph(), ...overrides });
  daemon.apiFetch.mockImplementation(daemonFetch(state));
  daemon.apiText.mockImplementation(daemonText(state));
  const rendered = renderWithQuery(
    <WorkflowGraph
      projectId="nucleos"
      name="harness"
      originPath="C:/lib/harness/1.0"
      ejected={false}
      {...props}
    />,
  );
  return { state, rendered };
}

describe("the workflow canvas", () => {
  /**
   * The lesson `FleetCanvas` paid for, asserted again because it is invisible until it is a
   * performance bug. xyflow compares these by reference and rebuilds every node when either
   * changes; declared inside the component they would be new objects on every render.
   */
  it("hands xyflow the same nodeTypes and edgeTypes on every render", async () => {
    const { rendered } = openGraph();
    await screen.findByLabelText("Node inspector");
    // Re-rendered through the same wrapper the first mount used: the component needs the query
    // cache, and a bare `rerender` would replace the provider with nothing.
    rendered.rerender(
      <QueryClientProvider client={rendered.queryClient}>
        <WorkflowGraph projectId="nucleos" name="harness" originPath="C:/lib" ejected={false} />
      </QueryClientProvider>,
    );

    expect(seen.nodeTypes.length).toBeGreaterThanOrEqual(2);
    for (const captured of seen.nodeTypes) expect(captured).toBe(seen.nodeTypes[0]);
    for (const captured of seen.edgeTypes) expect(captured).toBe(seen.edgeTypes[0]);
  });

  /**
   * §6.4: the node says what it is without anybody reading a field. The accessible name carries the
   * same distinction the colour does, which is what keeps the vocabulary readable to somebody who
   * cannot see the colour at all.
   */
  it("draws each node with what it is, and marks the gate as a gate", async () => {
    openGraph();
    expect(await screen.findByLabelText(/Plan, a model is asked to do this/)).toBeTruthy();
    expect(screen.getByLabelText(/Gate, a command a branch depends on/)).toBeTruthy();
  });

  /**
   * §6.2: a node switched off here stays in the graph, marked. Hiding it would make the picture lie
   * about what the workflow is — and §12 needs it to stay distinguishable from a node the bundle
   * does not have, which cannot be drawn at all.
   */
  it("keeps a node this project switched off in the picture, marked", async () => {
    openGraph({
      graph: graph({
        nodes: [graphNode({ id: "plan", disabled: true, overridden: true }), graphNode({ id: "gate", type: "command" })],
      }),
    });
    expect(await screen.findByLabelText(/Plan, a model is asked to do this/)).toBeTruthy();
    expect(screen.getByText("off here")).toBeTruthy();
    expect(screen.getAllByText("project").length).toBeGreaterThan(0);
  });

  /**
   * The inspector shows the effective value AND what the origin said. An override whose other half
   * you cannot see is a fork nobody can undo.
   */
  it("shows what the origin said beside what this project changed", async () => {
    openGraph({
      graph: graph({
        nodes: [
          graphNode({
            id: "plan",
            overridden: true,
            fields: [{ name: "model", value: "haiku", origin: "opus" }],
          }),
        ],
        edges: [],
      }),
    });

    fireEvent.click(await screen.findByLabelText(/Plan, a model is asked/));
    expect(await screen.findByText("haiku")).toBeTruthy();
    expect(screen.getByText("the bundle says opus")).toBeTruthy();
  });

  /**
   * **Two different edits, and telling them apart is the point.**
   *
   * Changing which model runs a node is the overlay — the project's, no copy taken — so it must NOT
   * be behind the eject guard. Putting it there is how every project ends up ejected over one word.
   */
  it("changes the model with no guard and no copy taken", async () => {
    const { state } = openGraph();
    fireEvent.click(await screen.findByLabelText(/Plan, a model is asked/));

    const field = await screen.findByLabelText("model in this project");
    fireEvent.change(field, { target: { value: "haiku" } });
    fireEvent.blur(field);

    await waitFor(() => expect(state.overlays.length).toBe(1));
    expect(state.overlays[0]).toEqual({
      name: "harness",
      node: "plan",
      body: { model: "haiku" },
    });
    // No guard was involved, and nothing was ejected.
    expect(screen.queryByRole("group", { name: "Edit this node's instructions" })).toBeNull();
    expect(state.workflowChanges).toEqual([]);
  });

  /** Emptying the field means *stop overriding*, which is the same request rather than a second one. */
  it("clears an override by emptying the field", async () => {
    const { state } = openGraph({
      graph: graph({
        nodes: [graphNode({ id: "plan", overridden: true, fields: [{ name: "model", value: "haiku", origin: "opus" }] })],
        edges: [],
      }),
    });
    fireEvent.click(await screen.findByLabelText(/Plan, a model is asked/));
    const field = await screen.findByLabelText("model in this project");
    fireEvent.change(field, { target: { value: "  " } });
    fireEvent.blur(field);

    await waitFor(() => expect(state.overlays.length).toBe(1));
    expect(state.overlays[0].body).toEqual({ model: null });
  });

  /**
   * §6.3, and only for the edit it is actually about: changing what a node SAYS is changing the
   * bundle. Three exits, inline, and the middle one is the point — most of the time what somebody
   * wants is to improve the workflow rather than diverge from it.
   */
  it("guards editing the instructions of a referenced node, inline and never as a dialog", async () => {
    const { rendered } = openGraph();
    fireEvent.click(await screen.findByLabelText(/Plan, a model is asked/));
    fireEvent.click(await screen.findByRole("button", { name: "edit the instructions" }));

    expect(screen.getByRole("group", { name: "Edit this node's instructions" })).toBeTruthy();
    expect(rendered.container.querySelector("dialog")).toBeNull();

    fireEvent.click(screen.getByRole("button", { name: "edit in the library" }));
    expect(opener.openUrl).toHaveBeenCalledWith("vscode://file/C:/lib/harness/1.0");
  });

  /**
   * An ejected workflow has no guard: the copy is already this project's, and there is nothing left
   * to be deliberate about.
   */
  it("opens an ejected node's instructions with no guard at all", async () => {
    openGraph({ graph: graph({ source: "project" }) }, { ejected: true });
    fireEvent.click(await screen.findByLabelText(/Plan, a model is asked/));
    fireEvent.click(await screen.findByRole("button", { name: "edit the instructions" }));
    expect(screen.queryByRole("group", { name: "Edit this node's instructions" })).toBeNull();
    expect(opener.openUrl).toHaveBeenCalled();
  });

  /**
   * §12: `switched off here` ≠ `not in the bundle`. The second cannot be drawn, so it is said — and
   * this is the only moment somebody learns a bundle they updated dropped the node they configured.
   */
  it("says which overrides apply to nothing", async () => {
    openGraph({ graph: graph({ orphaned: ["council"] }) });
    expect(await screen.findByText(/overrides council, which 1.0 does not have/)).toBeTruthy();
  });

  /** A bundle with no graph is a halfway state, and a graph that will not parse is a fault. */
  it("distinguishes a bundle with no graph from one that does not parse", async () => {
    openGraph({ graph: null });
    // Matched on the half that lives in one text node — the file name is its own `<span>`, so a
    // regex across the whole sentence matches nothing.
    expect(await screen.findByText(/halfway state, not a broken bundle/)).toBeTruthy();
  });
});

describe("the workflow miniature", () => {
  /**
   * §4.5: the chain in the Estado mode reads in the same order the canvas lays out, so the two
   * cannot teach different shapes for one workflow.
   */
  it("draws the installed workflow as a chain in the state mode", async () => {
    const state = daemonState({
      projects: [project({ project_id: "nucleos", mode: "shadow" })],
      library: [bundle()],
      workflows: [installedWorkflow()],
      graph: graph(),
    });
    daemon.apiFetch.mockImplementation(daemonFetch(state));
    daemon.apiText.mockImplementation(daemonText(state));
    daemon.probeHealth.mockResolvedValue(true);
    await renderApp({ initialPath: "/projects/nucleos/estado" });

    const chain = await screen.findByLabelText("harness as a chain");
    expect(Array.from(chain.querySelectorAll("li")).map((item) => item.textContent)).toEqual([
      "Plan",
      "Gate",
    ]);
  });
});
