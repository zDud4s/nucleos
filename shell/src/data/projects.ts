import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { apiFetch, apiText, isApiRefusal } from "./client";
import { keys } from "./keys";

/**
 * A project's rules, and the read-only window onto its tree.
 *
 * Two kinds of hook, with opposite cadences, and the split is the whole file.
 *
 * **The rules are read on open and not polled.** `get_project_rules` loads
 * `.ai/autopilot.yaml` off the disk and stats it on every call; putting that on
 * a three-second timer would be a file read per tick for a document somebody
 * edits once a week. The *live* numbers a rules panel needs — `open_proposals`,
 * `wip_limit`, `queue_full` — are already on the roster row from
 * `useProjects()`, which does poll, so the panel takes its moving parts from
 * there and its file facts from here.
 *
 * **The inspect readers are on demand and never poll at all.** `ls`, `cat`,
 * `grep` and `diff` answer questions a person asked by clicking; re-asking them
 * on a timer would walk somebody's working tree in the background forever.
 * `diff` gets an explicit refresh instead, which is the honest shape: the tree
 * changes when the person changes it.
 *
 * The one write on this page is the WIP ceiling.
 */

/* ----------------------------------------------------------------- shapes -- */

/** One scheduled rule, with what the daemon knows about it having run. */
export interface ScheduleView {
  name: string;
  cron: string;
  prompt: string;
  cwd: string | null;
  /** `null` means UTC — the scheduler's own default, not an unset field. */
  timezone: string | null;
  next_fire_at: string | null;
  /**
   * Why this rule will never fire, when that is the answer instead of a time.
   *
   * An unparseable cron or an unknown timezone makes the tick skip the rule and
   * log at debug, 2,880 times a day, while the rule silently never runs. This
   * field is the daemon's own account of that, and it is first-class
   * information on the page rather than a tooltip.
   */
  problem: string | null;
  last_fired_at: string | null;
  fires_today: number;
  daily_cap: number;
}

/** One repo trigger, with the commit it last saw. */
export interface RepoTriggerView {
  name: string;
  branch: string;
  prompt: string;
  /** `null` is armed with no first commit to compare against — which fires nothing, by design. */
  last_sha: string | null;
}

/** Everything a project will do without being asked, and what is holding it back. */
export interface ProjectRules {
  project_id: string;
  project_root: string | null;
  /** The three states `.ai/autopilot.yaml` can be in. `absent` is ordinary; the file is gitignored. */
  rules_file: "present" | "absent" | "unreadable";
  /** Why the file could not be read. Non-null exactly when `rules_file` is `unreadable`. */
  rules_error: string | null;
  gate_command: string | null;
  schedules: ScheduleView[];
  repo_triggers: RepoTriggerView[];
  /** The effective ceiling. `null` means the brake is **off**, which is not a ceiling of zero. */
  wip_limit: number | null;
  open_proposals: number;
  queue_full: boolean;
}

/** One directory entry, as `inspect::Entry` serialises. */
export interface InspectEntry {
  name: string;
  is_dir: boolean;
}

/** One grep hit. `line` is 1-based, as the daemon counts. */
export interface InspectMatch {
  path: string;
  line: number;
  text: string;
}

/** What `POST /projects/{id}/wip-limit` accepts. `null` switches the brake off. */
export interface WipLimitChange {
  projectId: string;
  limit: number | null;
}

/* ------------------------------------------------------------------ reads -- */

/**
 * A project's rules, read when the panel opens.
 *
 * `enabled` rather than a conditional hook: the panel unmounts when it closes,
 * and `null` is how the caller says *nothing is chosen*.
 */
export function useProjectRules(projectId: string | null) {
  return useQuery({
    queryKey: keys.projects.rules(projectId ?? ""),
    queryFn: () => apiFetch<ProjectRules>(`/projects/${encodeURIComponent(projectId ?? "")}/rules`),
    enabled: projectId !== null,
  });
}

/** One directory of a project's tree. The empty path is the root itself. */
export function useProjectLs(projectId: string | null, path: string, enabled = true) {
  return useQuery({
    queryKey: keys.projects.ls(projectId ?? "", path),
    queryFn: () =>
      apiFetch<InspectEntry[]>(
        `/projects/${encodeURIComponent(projectId ?? "")}/ls?path=${encodeURIComponent(path)}`,
      ),
    enabled: enabled && projectId !== null,
  });
}

/**
 * One file, as text.
 *
 * **`apiText`, never `apiFetch`.** `get_project_cat` returns a bare `String`, so
 * the body is the file and not JSON — and an empty file is a 200 with an empty
 * body, which `apiFetch` would turn into a parse error out of a success.
 */
export function useProjectCat(projectId: string | null, path: string, enabled = true) {
  return useQuery({
    queryKey: keys.projects.cat(projectId ?? "", path),
    queryFn: () =>
      apiText(`/projects/${encodeURIComponent(projectId ?? "")}/cat?path=${encodeURIComponent(path)}`),
    enabled: enabled && projectId !== null && path.trim() !== "",
  });
}

/** Matches for a query under a subtree. An empty query asks nothing and is not sent. */
export function useProjectGrep(
  projectId: string | null,
  q: string,
  path: string,
  enabled = true,
) {
  return useQuery({
    queryKey: keys.projects.grep(projectId ?? "", q, path),
    queryFn: () =>
      apiFetch<InspectMatch[]>(
        `/projects/${encodeURIComponent(projectId ?? "")}/grep?q=${encodeURIComponent(q)}&path=${encodeURIComponent(path)}`,
      ),
    enabled: enabled && projectId !== null && q.trim() !== "",
  });
}

/**
 * The working tree's uncommitted diff, as text.
 *
 * `apiText` for the same reason as `cat`, with one extra: a clean tree is a
 * **200 with an empty body**, which is the most likely answer of the four and
 * the one `apiFetch` would fail on. An empty diff is a fact, not an error.
 */
export function useProjectDiff(projectId: string | null, enabled = true) {
  return useQuery({
    // The key carries a path the route does not take: `keys.projects.diff` is
    // declared with one, and passing the empty string keeps the shape rather
    // than editing a namespace this packet does not own.
    queryKey: keys.projects.diff(projectId ?? "", ""),
    queryFn: () => apiText(`/projects/${encodeURIComponent(projectId ?? "")}/diff`),
    enabled: enabled && projectId !== null,
  });
}

/* -------------------------------------------------------------- the write -- */

/**
 * Set or clear a project's open-proposal ceiling.
 *
 * **204, so no body.** `null` is the brake off and is a different request from
 * `0`, which would mean "never start anything again" — the daemon compares
 * `open >= limit`. A negative ceiling is refused with a 400 before it is
 * stored, and the form never offers one.
 *
 * The invalidation is the roster prefix, which reaches the rules read too:
 * `keys.projects.rules(id)` is `["projects", id, "rules"]` and lives under
 * `keys.projects.all`. One invalidation, both readers.
 */
export function useSetWipLimit() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: ({ projectId, limit }: WipLimitChange) =>
      apiFetch<void>(`/projects/${encodeURIComponent(projectId)}/wip-limit`, {
        method: "POST",
        body: JSON.stringify({ limit }),
      }),
    retry: false,
    onSettled: () => {
      void queryClient.invalidateQueries({ queryKey: keys.projects.all });
    },
  });
}

/* --------------------------------------------------------------- readings -- */

/**
 * What is known about reaching a project's tree.
 *
 * The four are kept apart because the inspect routes answer **404 for two
 * different things** — a path that is not there, and a project whose *recorded
 * root* is gone from disk (`resolve_project_root` answers 404 when the row has
 * no root at all, `inspect_status` answers it when the path does not resolve) —
 * and a project that was never given a root is a third thing again. Collapsing
 * them into "not found" tells somebody to go looking for a file when the answer
 * is that the folder moved, or that they never named one.
 */
export type Reachability = "no-root" | "reading" | "ok" | "gone" | "failed";

export interface ProbeReading {
  /** Has the probe answered at all yet? */
  isSuccess: boolean;
  isError: boolean;
  error: unknown;
}

/**
 * Read a root probe — the project's own `ls` of `""` — into one of the four.
 *
 * A project with no recorded root is never probed: the route would answer 404
 * for it, and that 404 means "you have not named a folder", not "the folder is
 * gone".
 */
export function readReachability(projectRoot: string | null, probe: ProbeReading): Reachability {
  if (projectRoot === null) return "no-root";
  if (probe.isSuccess) return "ok";
  if (!probe.isError) return "reading";
  return isApiRefusal(probe.error) && probe.error.status === 404 ? "gone" : "failed";
}

/** The sentence each reachability deserves, said once. */
export const REACHABILITY_TEXT: Record<Reachability, string> = {
  "no-root": "no folder named",
  reading: "checking",
  ok: "reachable",
  gone: "folder gone",
  failed: "unreadable",
};

/**
 * Is this cron rule armed but inert?
 *
 * `problem` non-null is the daemon saying the tick skips this rule every time.
 * A rule with no next fire and no problem is a rule that has simply not been
 * scheduled yet, which is a different and much less alarming fact.
 */
export function scheduleNeverFires(schedule: ScheduleView): boolean {
  return schedule.problem !== null && schedule.problem.trim() !== "";
}

/** Has this rule used up today's allowance? The pair is only readable together. */
export function scheduleCapped(schedule: ScheduleView): boolean {
  return schedule.daily_cap > 0 && schedule.fires_today >= schedule.daily_cap;
}

/** A path's parts, for the breadcrumb trail. The root is the empty path. */
export function pathSegments(path: string): string[] {
  return path.split("/").filter((part) => part !== "");
}

/** The path formed by walking `depth` segments in from the root. */
export function pathUpTo(path: string, depth: number): string {
  return pathSegments(path).slice(0, depth).join("/");
}

/** One step deeper, without the leading slash a root join would leave behind. */
export function joinPath(path: string, name: string): string {
  return path === "" ? name : `${path}/${name}`;
}
