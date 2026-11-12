import { useQuery } from "@tanstack/react-query";
import { apiFetch } from "./client";
import { keys } from "./keys";

/**
 * A project's history, as git reports it.
 *
 * Read on demand and never polled, the same rule the rest of the inspect routes follow: these walk
 * somebody's repository with a subprocess, and putting that on a three-second timer would mean
 * spawning git forever in the background for an answer that changes when a person commits.
 */

/** One commit, as the núcleo reports it. */
export interface Commit {
  sha: string;
  short_sha: string;
  author: string;
  /** Strict ISO 8601 with an offset. */
  at: string;
  subject: string;
}

export interface BranchRow {
  name: string;
  /** Commits this branch has that the integration branch does not. */
  ahead: number;
  /** Commits the integration branch has that this one does not. */
  behind: number;
  /**
   * Whether the pair above was measured at all.
   *
   * `0/0` means identical to where work lands. `measured: false` means nobody knows — a detached
   * root, or a git that refused. Drawing the second as the first would report every branch as up to
   * date at exactly the moment the measurement stopped working.
   */
  measured: boolean;
  last_commit_at: string;
  last_subject: string;
}

export interface Branches {
  /**
   * The branch the project root is standing on.
   *
   * `null` on a detached HEAD. This is the daemon's own definition of where work lands — landing
   * computes a merge's target by reading exactly this — so it is not a guess and not `master` by
   * convention.
   */
  integration: string | null;
  branches: BranchRow[];
  /** Branches past the núcleo's ceiling, which were not measured. Zero is the ordinary case. */
  omitted: number;
}

export function useProjectBranches(projectId: string | null) {
  return useQuery({
    queryKey: keys.projects.branches(projectId ?? ""),
    queryFn: () => apiFetch<Branches>(`/projects/${encodeURIComponent(projectId ?? "")}/branches`),
    enabled: projectId !== null && projectId !== "",
  });
}

export function useProjectLog(projectId: string | null, path = "", limit = 50) {
  return useQuery({
    queryKey: keys.projects.log(projectId ?? "", path),
    queryFn: () =>
      apiFetch<Commit[]>(
        `/projects/${encodeURIComponent(projectId ?? "")}/log?path=${encodeURIComponent(path)}&limit=${limit}`,
      ),
    enabled: projectId !== null && projectId !== "",
  });
}

/**
 * How a branch stands relative to where work lands.
 *
 * Named rather than left as two numbers, because the four cases want four different sentences and a
 * caller doing the comparison inline gets one of them wrong eventually — usually `diverged`, which
 * looks like `ahead` until somebody has to rebase.
 */
export type BranchStanding = "integration" | "unmeasured" | "level" | "ahead" | "behind" | "diverged";

export function standingOf(row: BranchRow, integration: string | null): BranchStanding {
  if (row.name === integration) return "integration";
  if (!row.measured) return "unmeasured";
  if (row.ahead === 0 && row.behind === 0) return "level";
  if (row.behind === 0) return "ahead";
  if (row.ahead === 0) return "behind";
  return "diverged";
}

/** The tone each standing takes, through the app's own names rather than a colour. */
export const STANDING_TONE: Record<BranchStanding, string> = {
  // Where work lands is not a state to be alarmed about, and not a success either.
  integration: "info",
  // Not knowing is not a failure of the branch. It is a failure of the measurement, and the tone
  // says "look at this" without saying "this is broken".
  unmeasured: "off",
  level: "active",
  ahead: "pending",
  behind: "off",
  diverged: "paused",
};
