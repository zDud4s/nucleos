import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
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

/**
 * The intention layer: what a model read out of a spec, waiting for its owner to say yes or no.
 *
 * `kind` really is spelled `countable`/`character` on the wire. The núcleo's own column holds
 * `b`/`c` under a `CHECK`, and translates on the way out on purpose — a list the owner is meant to
 * approve at a glance cannot label its two kinds with letters.
 */

export type DecisionKind = "countable" | "character";

/** Which model reads a spec. See {@link useExtractSpec} for why this is a call-time choice. */
export type Brain = "cloud" | "local";

/** One sentence a model read out of a spec. Field for field off `crate::map_store::Decision`. */
export interface MapDecision {
  id: number;
  spec_slug: string;
  section: string;
  ordinal: number;
  text: string;
  kind: DecisionKind;
  brain: string;
  extracted_at: string;
  /** `null` until the owner answers this line, one way or the other. */
  approved_at: string | null;
}

/**
 * Which specs this project has, named the way the owner reads them.
 *
 * Read once when the mode opens, and not polled — the same posture {@link useProjectMap} takes,
 * for the same reason: the document set does not move between two ticks of a three-second timer.
 */
export function useProjectSpecs(projectId: string | null) {
  return useQuery({
    queryKey: keys.projects.mapSpecs(projectId ?? ""),
    queryFn: () =>
      apiFetch<string[]>(`/projects/${encodeURIComponent(projectId ?? "")}/map/specs`),
    enabled: projectId !== null && projectId !== "",
  });
}

/**
 * The pile nobody has read yet.
 *
 * Not folded into {@link useProjectMap}: the structure layer is derived off disk on every read and
 * the pile is a table somebody writes to. One query invalidated by an approval would re-walk the
 * whole tree to answer a question the tree cannot answer.
 */
export function useMapDecisions(projectId: string | null) {
  return useQuery({
    queryKey: keys.projects.mapDecisions(projectId ?? ""),
    queryFn: () =>
      apiFetch<MapDecision[]>(`/projects/${encodeURIComponent(projectId ?? "")}/map/decisions`),
    enabled: projectId !== null && projectId !== "",
  });
}

/** What {@link useExtractSpec} sends: the document, and which model should read it. */
export interface ExtractSpecInput {
  specSlug: string;
  brain: Brain;
}

/**
 * Ask a model what one spec decided, and get back the whole pile — this extraction's rows landed
 * among whatever was already waiting, which is what the owner reads.
 *
 * `brain` is a parameter here and not a setting anywhere, because the choice is the owner's to make
 * per request and the núcleo will not make it for them: asked for `local` on a machine with none
 * configured, it refuses rather than quietly falling back to the cloud — a substitution nobody
 * would see coming, and one that spends money the owner did not agree to. A setting would turn that
 * refusal into somebody's permanent default; asking each time keeps it a decision.
 */
export function useExtractSpec(projectId: string) {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: ({ specSlug, brain }: ExtractSpecInput) =>
      apiFetch<MapDecision[]>(`/projects/${encodeURIComponent(projectId)}/map/extract`, {
        method: "POST",
        body: JSON.stringify({ spec_slug: specSlug, brain }),
      }),
    retry: false,
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: keys.projects.mapDecisions(projectId) });
    },
  });
}

/** What {@link useDecideMapLine} sends: the row, and the owner's answer to it. */
export interface DecideMapLineInput {
  id: number;
  approved: boolean;
}

/**
 * The owner's answer to one line.
 *
 * **204, so there is no body** (`post_project_map_decision`) — `void` is the honest type, and
 * `apiFetch` exempts 204 from its JSON parse, the way {@link useSetShadowVerdict} in `autopilot.ts`
 * already does for its own bodyless verdict. `404` means the row was already answered, belonged to
 * another project, or never existed — three reasons the daemon deliberately collapses into one,
 * because all three mean the same thing to whoever asked: *that line is not yours to answer now*.
 */
export function useDecideMapLine(projectId: string) {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: ({ id, approved }: DecideMapLineInput) =>
      apiFetch<void>(`/projects/${encodeURIComponent(projectId)}/map/decisions/${id}`, {
        method: "POST",
        body: JSON.stringify({ approved }),
      }),
    retry: false,
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: keys.projects.mapDecisions(projectId) });
    },
  });
}
