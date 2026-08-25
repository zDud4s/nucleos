import { useQuery } from "@tanstack/react-query";
import { apiFetch } from "./client";
import { keys } from "./keys";

/**
 * A project's structure layer: what modules exist and what they import.
 *
 * **Not the whole map.** The intention and evidence layers, the stamps and the triage are
 * later slices. This one answers only *"how is this built"* — and on its own it already
 * separates the modules that declare what they implement from the ones that declare nothing.
 */

export type Reader = "rust" | "typescript";

export interface MapModule {
  path: string;
  reader: Reader;
  /** It cites a spec section. `false` is the *code nobody asked for* pile. */
  declares: boolean;
  tested: boolean;
}

export interface MapImport {
  from: string;
  to: string;
}

export interface ProjectMap {
  modules: MapModule[];
  imports: MapImport[];
  /**
   * Files no reader here understands, kept rather than dropped.
   *
   * Mostly real code — the Go sidecars, the SQL migrations, the stylesheets and the scripts —
   * and keeping them is the whole point: a map that showed only what it could read would
   * quietly claim `sidecars/` does not exist. A tail of it was never source at all, being
   * assets and configuration. It measures what this map cannot see, and nothing that is wrong.
   */
  unread: string[];
}

/**
 * Read when the mode opens, and not polled.
 *
 * The structure of a repository does not move between two ticks of a three-second timer, and
 * putting it on one would mean walking the whole tree off disk on every tick.
 */
export function useProjectMap(projectId: string | null) {
  return useQuery({
    queryKey: keys.projects.map(projectId ?? ""),
    queryFn: () => apiFetch<ProjectMap>(`/projects/${encodeURIComponent(projectId ?? "")}/map`),
    enabled: projectId !== null && projectId !== "",
  });
}
