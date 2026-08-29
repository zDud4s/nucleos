// §spec workspace-de-projeto
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { apiFetch } from "./client";
import { keys } from "./keys";

/**
 * What is already in a folder somebody is about to hand this app.
 *
 * §9's second step. A project worth adding has a history, commands its people type every day, and
 * often a written-down way of working — **this repository's is `.ai/`, and it built NucleOS.** The
 * núcleo reads all of that and this offers it; what gets saved is what somebody ticked, which is the
 * same rule the command registry already runs on. A list that changed by itself when a
 * `package.json` was edited would be noise, and there would be nowhere in it to mark the gate.
 */

export interface Suggestion {
  name: string;
  command: string;
  /** `package.json`, `Makefile`, `.cargo/config.toml`. Shown, because `check` means three things. */
  source: string;
}

export interface Harness {
  /** Relative to the folder, forward slashes. */
  path: string;
  what: string;
  files: number;
}

export interface Detected {
  /** The folder as the filesystem resolved it — what gets registered, not what was typed. */
  root: string;
  is_git: boolean;
  remote: string | null;
  branch: string | null;
  head: string | null;
  harnesses: Harness[];
  commands: Suggestion[];
  /** Never silent: a truncated list that did not say so would read as the whole of what is there. */
  commands_omitted: number;
  /** A project this daemon already keeps at this folder. The one thing the folder cannot say. */
  taken_by: string | null;
}

/**
 * Look at a folder.
 *
 * `enabled` on a non-empty path, so nothing is asked until somebody has typed one. `retry: false`:
 * a folder that is not there is not there a second later either, and the wizard needs the refusal's
 * own name to say which of the three it was.
 */
export function useDetect(path: string | null) {
  return useQuery({
    queryKey: keys.projects.detect(path ?? ""),
    queryFn: () => apiFetch<Detected>(`/projects/detect?path=${encodeURIComponent(path ?? "")}`),
    enabled: path !== null && path.trim() !== "",
    retry: false,
    // A folder does not change while somebody reads a wizard page about it, and re-reading one on
    // every window focus would re-run three git commands against a disk for nothing.
    refetchOnWindowFocus: false,
  });
}

/**
 * Record a folder this project already has as its workflow.
 *
 * Adopting is not installing, and it is a different call rather than a flag: it takes a folder
 * instead of a coordinate, copies nothing, and produces a pin no library can ever update.
 */
export function useAdoptWorkflow() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: ({ projectId, name, path }: { projectId: string; name: string; path: string }) =>
      apiFetch<void>(`/projects/${encodeURIComponent(projectId)}/workflows/adopt`, {
        method: "POST",
        body: JSON.stringify({ name, path }),
      }),
    retry: false,
    onSettled: () => {
      void queryClient.invalidateQueries({ queryKey: keys.projects.all });
      void queryClient.invalidateQueries({ queryKey: keys.workflows.all });
    },
  });
}

/**
 * A project id from a folder, for the field to start from.
 *
 * The last segment, lowercased, with anything that is not a letter, a digit or a dash turned into a
 * dash. A suggestion and not a rule — the field stays editable — because the folder's name is right
 * often enough to save typing and wrong often enough that it must not be imposed.
 *
 * Both separators, because this daemon runs on Windows and a path arrives with either.
 */
export function suggestedId(root: string): string {
  const last = root.replace(/[\\/]+$/, "").split(/[\\/]/).pop() ?? "";
  return last
    .toLowerCase()
    .replace(/[^a-z0-9-]+/g, "-")
    .replace(/^-+|-+$/g, "");
}
