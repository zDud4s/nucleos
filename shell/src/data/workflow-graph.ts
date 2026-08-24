import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { apiFetch } from "./client";
import { keys } from "./keys";

/**
 * A workflow's graph, with this project's overlay painted on it.
 *
 * The núcleo resolves the overlay, not the shell — `core/src/workflow_graph.rs`. A client that
 * applied it itself would be a second implementation of §6.2's rules, and the two would eventually
 * disagree about which node this project changed, which is the one thing the seal exists to say.
 */

/** Who executes a node. Four, decided by executor, per §6.4. */
export type NodeKind = "agent" | "command" | "decision" | "fan";

/**
 * What a node is *for*, as distinct from who runs it.
 *
 * A gate is a **command something branches on**, and that is a fact about the edges. It is not a
 * fifth kind: making it one would let a bundle declare a gate nothing conditions on, which is a
 * gate in name only.
 */
export type Role = "plain" | "gate";

export type Verdict = "pass" | "fail";

export interface GraphField {
  name: string;
  value: string;
  /**
   * What the origin said, present **only** when this project overrode the field.
   *
   * Absent means inherited. A shape that always carried both would make the page compare them to
   * find out which — and two values that happened to be equal would then read as an override.
   */
  origin?: string;
}

export interface GraphNode {
  id: string;
  type: NodeKind;
  role: Role;
  label: string;
  /** Switched off in this project. Still in the graph, drawn dotted — §6.2. */
  disabled: boolean;
  /** Whether this project changed anything here. The seal §6.2 makes mandatory. */
  overridden: boolean;
  fields: GraphField[];
}

export interface GraphEdge {
  from: string;
  to: string;
  when?: string;
  verdict?: Verdict;
}

export interface WorkflowGraph {
  /** Which copy this came out of: `project` for an ejected workflow, `library` otherwise. */
  source: "project" | "library";
  version: string;
  nodes: GraphNode[];
  edges: GraphEdge[];
  /**
   * Overrides naming a node the bundle does not have.
   *
   * §12: `switched off here` ≠ `not in the bundle`. This is the second, and it is the only moment
   * somebody learns their override stopped applying.
   */
  orphaned: string[];
}

/**
 * The graph of one installed workflow.
 *
 * No polling. A bundle is a folder of files and a pin is a file; neither changes while somebody
 * looks at it, and the mutations on this page invalidate what they touched.
 */
export function useWorkflowGraph(projectId: string, name: string | null) {
  return useQuery({
    queryKey: keys.projects.workflowGraph(projectId, name ?? ""),
    queryFn: () =>
      apiFetch<WorkflowGraph>(
        `/projects/${encodeURIComponent(projectId)}/workflows/${encodeURIComponent(name ?? "")}/graph`,
      ),
    enabled: name !== null,
    // Every refusal is settled: no such workflow, no graph in the bundle, a graph that does not
    // parse. Asking again answers the same thing a second later.
    retry: false,
  });
}

/**
 * Change what this project overrides on one node — or clear it back to inherited.
 *
 * **This is the half of §6.2 that means a project does not have to eject.** Which model runs a
 * node, which tool, and whether it runs here at all belong to the project without a copy of
 * anything being taken. Ejecting is for changing the bundle's own content, which is where §6.3's
 * guard lives — a different question with a different answer.
 *
 * A change with every field empty clears the row, so "go back to what the bundle says" is the same
 * request rather than a second one.
 */
export function useSetNodeOverlay() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: ({
      projectId,
      name,
      node,
      ...overlay
    }: {
      projectId: string;
      name: string;
      node: string;
      disabled?: boolean | null;
      model?: string | null;
      tool?: string | null;
      command?: string | null;
    }) =>
      apiFetch<void>(
        `/projects/${encodeURIComponent(projectId)}/workflows/${encodeURIComponent(name)}/nodes/${encodeURIComponent(node)}`,
        { method: "POST", body: JSON.stringify(overlay) },
      ),
    retry: false,
    onSettled: () => {
      // The whole project prefix: the graph changed, and so did the count of overridden nodes the
      // listing beside it shows. Two invalidations that could drift apart is how one surface ends
      // up disagreeing with the other about what this project has changed.
      void queryClient.invalidateQueries({ queryKey: keys.projects.all });
    },
  });
}
