import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { apiFetch } from "./client";
import { keys } from "./keys";

/**
 * The write boundary, and the one write that crosses it.
 *
 * The rule the app runs on is not *read vs. write* — it is **who is the file's legitimate author**.
 * `core/src/ownership.rs` holds that as a table, `GET /projects/{id}/ownership` serves it, and this
 * module is the shell's half: it asks what the app is allowed to author and offers exactly that.
 *
 * **Nothing here hard-codes a path.** A client carrying its own copy of the fence goes out of date
 * silently — it would offer an editor for a file the daemon refuses, or hide one for a file it
 * would accept, and neither mistake announces itself. So the list of editable files is a query, not
 * a constant, and a project with an empty list gets no editor rather than a broken one.
 */

/** One file the app declares itself the author of. */
export interface Claim {
  /** Relative to the project root, forward slashes. Sent back verbatim when writing. */
  path: string;
  /** A stable wire value (`core` today, a workflow's name later). The page writes its own words. */
  owner: string;
  /** What editing it changes, in the daemon's sentence. */
  what: string;
}

export interface FileWrite {
  projectId: string;
  path: string;
  contents: string;
}

/**
 * Which files this app may author in this project.
 *
 * 404 when the project has no folder on disk, which is the same answer the write route gives — so
 * a page that cannot draw the fence also cannot offer an editor that would fail on save.
 */
export function useProjectOwnership(projectId: string | null) {
  return useQuery({
    queryKey: keys.projects.ownership(projectId ?? ""),
    queryFn: () =>
      apiFetch<Claim[]>(`/projects/${encodeURIComponent(projectId ?? "")}/ownership`),
    enabled: projectId !== null,
  });
}

/**
 * Write one file the app owns.
 *
 * **No optimistic write, and no `retry`.** Both for the same reason the mode change has neither:
 * the daemon validates before it writes, so a refusal means the file on disk is still the old one,
 * and a page that had already drawn the new text would be showing a state that exists nowhere. The
 * five refusals are five different sentences — `kill_switch`, `no_project_root`, `not_ours`,
 * `unwritable`, `invalid` — and `invalid` arrives with the parser's own words in `detail`, which is
 * what makes the raw editor usable at all.
 *
 * The invalidation reaches the file's own read: `keys.projects.cat` is
 * `["projects", id, "cat", path]`, under `keys.projects.all`. `rules` lives under the same prefix
 * and is the other reader that must not go stale — it is the panel showing what the daemon now
 * thinks this project does on its own, which is precisely what just changed.
 */
export function useWriteProjectFile() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: ({ projectId, path, contents }: FileWrite) =>
      apiFetch<void>(`/projects/${encodeURIComponent(projectId)}/write`, {
        method: "POST",
        body: JSON.stringify({ path, contents }),
      }),
    retry: false,
    onSettled: () => {
      void queryClient.invalidateQueries({ queryKey: keys.projects.all });
    },
  });
}
