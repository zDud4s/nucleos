import { useQuery } from "@tanstack/react-query";
import { apiFetch } from "./client";

/** "No literal guard": the statement applies from any state. */
export const ANY_STATE = "*";
/** "Bound at runtime": the target is a parameter, so the code alone cannot name it. */
export const COMPUTED_STATE = "?";

/** One compare-and-set `UPDATE`, anchored at the line of its `UPDATE`. */
export interface StateTransition {
  from: string;
  to: string;
  /** The enclosing function, or `null` when the statement sits at top level. */
  function: string | null;
  /** Repository-relative path. */
  path: string;
  /** 1-based. */
  line: number;
}

/** The lifecycle of one (table, column), read off the code. */
export interface StateMachine {
  table: string;
  column: string;
  /** Literal states only, sorted. */
  states: string[];
  transitions: StateTransition[];
}

/** `GET /projects/{id}/map/states`. */
export interface ProjectStates {
  /** Absolute project root, for the editor anchors. */
  root: string;
  files_read: number;
  machines: StateMachine[];
}

/**
 * The state machines of a project. The key sits under the `map` prefix, so invalidating the map
 * refreshes this too; it is written inline so the shared key table stays untouched.
 */
export function useProjectStates(projectId: string | null) {
  return useQuery({
    queryKey: ["projects", projectId ?? "", "map", "states"] as const,
    queryFn: () =>
      apiFetch<ProjectStates>(`/projects/${encodeURIComponent(projectId ?? "")}/map/states`),
    enabled: projectId !== null && projectId !== "",
  });
}
