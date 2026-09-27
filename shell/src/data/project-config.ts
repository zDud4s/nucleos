import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { apiFetch, apiText } from "./client";
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
  /**
   * The row's identity, forward slashes, relative to its `home`. Sent back verbatim when reading
   * and writing — never shown: a person is shown `display`.
   */
  path: string;
  /**
   * `state` for the núcleo's own files, which live in `~/.nucleos/projects/<id>/` and not in the
   * project; `project` for a file in the project's folder. Optional: the shell can be newer than
   * the daemon.
   */
  home?: "state" | "project";
  /**
   * The file as a person is shown it — `~/.nucleos/projects/<id>/autopilot.yaml`. Served, so the
   * page never carries a location of its own. Optional for the same reason as `home`.
   */
  display?: string;
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
 * The text of one file the app owns, from wherever the daemon keeps it.
 *
 * Its own route and not `cat`, because `cat` reads inside the project's folder and the núcleo's own
 * files are not there any more. A 404 is a file not written yet, which the editor treats as a state
 * and not an error. Under `keys.projects.all`, so the write below invalidates it with everything
 * else.
 */
export function useProjectOwnedFile(projectId: string | null, path: string, enabled = true) {
  return useQuery({
    queryKey: keys.projects.owned(projectId ?? "", path),
    queryFn: () =>
      apiText(
        `/projects/${encodeURIComponent(projectId ?? "")}/owned?path=${encodeURIComponent(path)}`,
      ),
    enabled: enabled && projectId !== null && path !== "",
    retry: false,
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
 * The invalidation reaches the file's own read: `keys.projects.owned` is
 * `["projects", id, "owned", path]`, under `keys.projects.all`. `rules` lives under the same prefix
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
