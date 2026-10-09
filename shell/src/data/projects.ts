import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { apiFetch, apiText, isApiRefusal } from "./client";
import { keys } from "./keys";
// `roster.ts` imports only TYPES from this file, which are erased — so there is no runtime cycle.
import { whereWaiting } from "./roster";

/**
 * A project's rules, and the read-only window onto its tree.
 *
 * Two kinds of hook, with opposite cadences, and the split is the whole file.
 *
 * **The rules are read on open and not polled.** `get_project_rules` loads
 * the project's `autopilot.yaml` off the disk and stats it on every call; putting that on
 * a three-second timer would be a file read per tick for a document somebody
 * edits once a week. The *live* numbers a rules panel needs — `open_review_items`,
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
 * Three writes live here, and none is a file: the WIP ceiling, the judge and the IDE verify switch,
 * all rows in the núcleo's database. "The one write is the WIP ceiling" was true until the judge arrived beside it,
 * and a sentence that outlives its truth is read as a promise.
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
/** The post-merge gate's state for one project's target, as the rules read serves it. */
export interface PostgateState {
  target: string;
  last_green: string | null;
  running: string | null;
  red_groups: string[];
  red_since: string | null;
  red_sha: string | null;
  red_base: string | null;
  phase: "flake_check" | "bisect" | null;
  culprit: string | null;
  candidates: string[];
  also_suspect: string[];
}

export interface ProjectRules {
  project_id: string;
  project_root: string | null;
  /**
   * The three states the project's `autopilot.yaml` can be in. `absent` is ordinary: the file
   * exists once somebody writes rules for the project.
   */
  rules_file: "present" | "absent" | "unreadable";
  /**
   * Where that file is, as a person is shown it: `~/.nucleos/projects/<id>/autopilot.yaml`. Served
   * by the daemon so the page never carries a location of its own. Optional: the shell can be newer
   * than the daemon, and `rulesFileName` falls back to the bare name.
   */
  rules_path?: string;
  /** Why the file could not be read. Non-null exactly when `rules_file` is `unreadable`. */
  rules_error: string | null;
  gate_command: string | null;
  /**
   * Whether the VCS queue runs that command on a merge before publishing it.
   * Read here rather than derived: the rule lives in the project's `autopilot.yaml`,
   * and a second copy of it in the window is a copy that will eventually disagree.
   */
  gate_before_publish: boolean;
  /**
   * Whether the IDE verify switch is on. Optional: the shell can be newer than the daemon, and an
   * older one does not report it — the toggle then offers no control.
   */
  ide_verify?: boolean;
  /**
   * The post-merge gate's state for the project's target: `null` when it has never run, absent
   * when the daemon is older than the shell.
   */
  postgate?: PostgateState | null;
  /** Who answers an approval a conversation on `auto` would otherwise put to a person. */
  judge: JudgeState;
  schedules: ScheduleView[];
  repo_triggers: RepoTriggerView[];
  /** The effective ceiling. `null` means the brake is **off**, which is not a ceiling of zero. */
  wip_limit: number | null;
  open_review_items: number;
  /** The two queues the total is made of. Optional: the shell can be newer than the daemon. */
  open_proposals?: number;
  open_shadow_decisions?: number;
  queue_full: boolean;
}

/**
 * The three states a project's judge can be in.
 *
 * Tagged and not two nullable fields, because `{ brain: null }` cannot tell "nobody has chosen"
 * from "somebody chose nobody" — and those two must not be collapsed: the first should follow the
 * default wherever it moves, the second is a decision that has to survive it.
 */
export type JudgeState =
  | { state: "default" }
  | { state: "off" }
  | { state: "named"; brain: "local" | "openrouter"; model: string | null };

/** What `POST /projects/{id}/judge` accepts. A null brain switches the judge off. */
export interface JudgeChange {
  projectId: string;
  brain: "local" | "openrouter" | null;
  model: string | null;
}

/**
 * Names this project's judge, or switches it off.
 *
 * The DELETE is a second hook rather than a `null` through this one, matching the two doors the
 * daemon opens: naming nobody and withdrawing the choice are different acts, and a single writer
 * would have to carry the difference as null-versus-missing.
 */
export function useSetJudge() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: ({ projectId, brain, model }: JudgeChange) =>
      apiFetch<void>(`/projects/${encodeURIComponent(projectId)}/judge`, {
        method: "POST",
        body: JSON.stringify({ brain, model }),
      }),
    retry: false,
    onSettled: () => {
      void queryClient.invalidateQueries({ queryKey: keys.projects.all });
    },
  });
}

/** Puts the project back on the default judge. 404 when it was already on it. */
export function useClearJudge() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (projectId: string) =>
      apiFetch<void>(`/projects/${encodeURIComponent(projectId)}/judge`, {
        method: "DELETE",
      }),
    retry: false,
    onSettled: () => {
      void queryClient.invalidateQueries({ queryKey: keys.projects.all });
    },
  });
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

/** What the reconcile did to one IDE worktree (`verify_provision::ProvisionState`). */
export type IdeVerifyState = "provisioned" | "removed" | "not_provisioned" | "untouched";

export interface WorktreeReport {
  path: string;
  state: IdeVerifyState;
  reason?: string;
}

/** The answer to `POST /projects/{id}/ide-verify`: the switch as stored and every worktree seen. */
export interface IdeVerifyAnswer {
  project: string;
  enabled: boolean;
  worktrees: WorktreeReport[];
}

/**
 * Switch IDE verify on or off for one project. Owner-only on the daemon side. Posting `true` to a
 * project that is already on re-provisions it, which is how a worktree created later is picked up.
 */
export function useSetIdeVerify() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: ({ projectId, enabled }: { projectId: string; enabled: boolean }) =>
      apiFetch<IdeVerifyAnswer>(`/projects/${encodeURIComponent(projectId)}/ide-verify`, {
        method: "POST",
        body: JSON.stringify({ enabled }),
      }),
    retry: false,
    onSettled: () => {
      void queryClient.invalidateQueries({ queryKey: keys.projects.all });
    },
  });
}

/* ------------------------------------------------------------- the exit -- */

/**
 * What a project has on record, in the nouns somebody would recognise.
 *
 * Every field is a thing this app has a screen for, so each one is something the reader can picture
 * losing. The daemon counts more tables than these — scheduler bookkeeping, trigger state — and
 * deliberately does not report them: a number nobody has ever seen a page for makes the decision
 * harder rather than easier, and forgetting takes them either way.
 */
export interface ProjectForgets {
  runs: number;
  jobs: number;
  proposals: number;
  decisions: number;
  stamps: number;
  commands: number;
  feed: number;
}

/**
 * What is still going on in this project right now.
 *
 * **A different question from {@link ProjectForgets}, and a different shape for that reason.** One
 * is what a removal would erase and the other is what stops it happening at all; one flat bag of
 * numbers would let a panel print "312 runs, 1 slot" as if those were the same kind of fact, and
 * only the second is a reason the daemon will refuse.
 */
export interface ProjectHolds {
  /** Slots taken right now — a run, a job or one of a job's items, working here. */
  slots: number;
  /** Worktrees still checked out on disk. Separate from the slots because the two come apart. */
  worktrees: number;
}

export interface ProjectRecord {
  forgets: ProjectForgets;
  holds: ProjectHolds;
}

/**
 * What removing this project would forget, and what is holding it.
 *
 * **On demand and never polled**, like the inspect readers above and for a sharper version of the
 * same reason: this is nine `COUNT(*)`s over the whole database, asked so that one person can
 * answer one checkbox. A roster of twenty-five running them on the three-second timer would be
 * counting a database to draw buttons nobody pressed.
 *
 * `enabled` is therefore the control being *open*, not the row being on screen.
 */
export function useProjectRecord(projectId: string | null, enabled = true) {
  return useQuery({
    queryKey: keys.projects.record(projectId ?? ""),
    queryFn: () => apiFetch<ProjectRecord>(`/projects/${encodeURIComponent(projectId ?? "")}/record`),
    enabled: enabled && projectId !== null,
  });
}

export interface ProjectRemoval {
  projectId: string;
  /**
   * Whether the record goes with the row.
   *
   * Defaulted nowhere and always passed, because the caller deciding is the point: the owner's
   * standing decision is that history is kept unless somebody says otherwise *at the moment they
   * say it*, which is a checkbox next to a number and not a default buried in a hook.
   */
  forgetHistory: boolean;
}

/**
 * Take a project off the roster.
 *
 * **Nothing on the disk is touched, and there is no argument here that would make it be.** Deleting
 * a folder is a second act with its own route and its own confirmation; a flag on this one would
 * give two very different weights the same shape, and the caller that eventually passed it would be
 * a caller that meant the reversible one.
 *
 * `retry: false` because both refusals this can get are settled answers: a 404 is a name that is
 * not there, and a 409 is work in flight that a retry a millisecond later cannot have finished.
 * The 409 clears by the work ending, which is a thing the person watches rather than a thing a
 * client waits out.
 */
export function useRemoveProject() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: ({ projectId, forgetHistory }: ProjectRemoval) =>
      apiFetch<void>(
        `/projects/${encodeURIComponent(projectId)}?forget_history=${forgetHistory ? "true" : "false"}`,
        { method: "DELETE" },
      ),
    retry: false,
    onSettled: () => {
      // The roster prefix, which reaches this project's record and every other reader under it —
      // including, on a refusal, the `holds` the panel is showing, which is exactly the number that
      // just proved to be out of date.
      void queryClient.invalidateQueries({ queryKey: keys.projects.all });
    },
  });
}

/* ---------------------------------------------------------- the folder -- */

/**
 * What is in this folder and nowhere else.
 *
 * **Not a file count, and that is a decision rather than an omission.** On a working repository a
 * file count is dominated by `node_modules` and `target`: the big frightening number would be
 * mostly build output that regenerates in a minute. What cannot be regenerated is work git has not
 * been told to keep, and work it has been told to keep that no remote has a copy of.
 */
export interface OnlyHere {
  /** Files with changes no commit holds — modified, staged or untracked. */
  uncommitted: number;
  /**
   * Commits no remote has. **`null` is not zero and it is the more serious answer**: a repository
   * with no remote configured has no elsewhere at all, so every commit in it is only here. Reporting
   * that as a count would be reporting the size of the history rather than the size of the loss.
   */
  unpushed: number | null;
}

/** A standing refusal about the path itself, named so a page can say it before offering anything. */
export interface FolderBlock {
  refusal: string;
  detail: string;
}

/**
 * Everything the delete control has to know before it draws.
 *
 * Three questions kept in three fields, because a page says a different sentence for each and a
 * single "can I?" boolean would collapse them: `only_here` is what would be lost for ever,
 * `blocked` is a standing refusal about the path, and `holds` is work in flight, which clears on
 * its own.
 */
export interface ProjectFolder {
  root: string | null;
  /** Whether the recorded folder is actually on this disk. */
  exists: boolean;
  /** `null` for a folder git knows nothing about — the more serious answer, not a missing one. */
  only_here: OnlyHere | null;
  blocked: FolderBlock | null;
  holds: ProjectHolds;
}

/**
 * What deleting this project's folder would take.
 *
 * Two `git` subprocesses and a `stat` behind it, so `enabled` is the control being open rather than
 * the page being on screen. Nothing about this belongs on a timer: it is read once, by somebody who
 * is deciding.
 */
export function useProjectFolder(projectId: string | null, enabled = true) {
  return useQuery({
    queryKey: keys.projects.folder(projectId ?? ""),
    queryFn: () => apiFetch<ProjectFolder>(`/projects/${encodeURIComponent(projectId ?? "")}/folder`),
    enabled: enabled && projectId !== null,
  });
}

/**
 * Delete the folder, and take the project off the roster with it.
 *
 * **The only irreversible thing this app does.** Its own route and its own hook rather than an
 * argument to {@link useRemoveProject}, so that nothing can reach it by passing a flag it did not
 * read — removing a project cannot be made to touch a disk, whatever it is sent.
 */
export function useDeleteProjectFolder() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: ({ projectId, forgetHistory }: ProjectRemoval) =>
      apiFetch<void>(
        `/projects/${encodeURIComponent(projectId)}/folder?forget_history=${forgetHistory ? "true" : "false"}`,
        { method: "DELETE" },
      ),
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

/**
 * The folder a path sits in. The root is the empty path.
 *
 * What a grep hit links to beside the file it names: the listing next to an opened file is its
 * folder, and a link that carried only the file left the listing on the project root.
 */
export function parentPath(path: string): string {
  const segments = pathSegments(path);
  return segments.slice(0, -1).join("/");
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

/* ------------------------------------------------------------ the views -- */

/** The four views, in the order the tabs read. */
export const VIEWS = ["browse", "search", "diff", "rules"] as const;
export type ProjectView = (typeof VIEWS)[number];

/**
 * A `$view` param as one of the four.
 *
 * Falls back rather than refusing. TanStack hands route params through as
 * strings with no validation of its own, so `/projects/alpha/brwose` is a path
 * a person can reach by typing — and answering a typo with a dead end teaches
 * nothing. Browse is the right landing: it is the view that needs no input.
 *
 * `rules` needs no input either, and is the view worth more. It is not the
 * landing anyway: this is a documented, tested contract with a stated reason,
 * and the header and the concerns strip now report a project's faults from
 * every view — so landing on `browse` is no longer landing on a dead end.
 */
export function normaliseView(raw: string | undefined): ProjectView {
  const candidate = (raw ?? "").trim().toLowerCase();
  return (VIEWS as readonly string[]).includes(candidate) ? (candidate as ProjectView) : "browse";
}

/* -------------------------------------------------- what runs on its own -- */

/**
 * What makes a rule go: a clock, or a commit.
 *
 * The two used to be two panels, and a project with two schedules and one
 * trigger read as two half-empty lists rather than as *three things run here
 * without you*. They are one class of thing — a rule that starts work when
 * nobody asked — with different clocks, so they are one table with a column
 * that says which clock.
 */
export type RuleClock = "cron" | "commit";

/**
 * One vocabulary for the state of a rule, across both clocks.
 *
 * `armed` used to mean two different things in two adjacent panels: on a
 * schedule that the cron parses and today's allowance is not spent, on a
 * trigger that a commit has been seen. Same word, same green pill, different
 * fact. `unseen` is the trigger's own state and says so.
 */
export type RuleState = "armed" | "never-fires" | "capped" | "unseen";

/** One rule that starts work here without you, whichever clock drives it. */
export interface AutonomyRule {
  name: string;
  clock: RuleClock;
  /** What makes it go: the cron expression, or the branch. */
  when: string;
  /** A cron's timezone. `null` for a commit rule, which has no clock to place. */
  zone: string | null;
  state: RuleState;
  /** The daemon's own account of why this never fires. Non-null only for `never-fires`. */
  problem: string | null;
  /** What the rule asks for, in the words somebody wrote in the file. */
  prompt: string;
  /** Where it runs, when that is not the project root. */
  cwd: string | null;
  /** A cron's next fire. `null` for a commit rule, and for a cron that is broken. */
  next: string | null;
  last: string | null;
  /** Today's allowance. `null` for a commit rule, which has none. */
  today: { fired: number; cap: number } | null;
  /** The commit a trigger last saw. `null` for a cron rule and for a trigger with none. */
  sha: string | null;
}

/**
 * Everything that starts work in this project without being asked, as one list.
 *
 * Schedules first, then repo triggers, each in the order the file names them:
 * a clock is the commoner case and the one somebody is usually looking for, and
 * re-sorting a file's own order would make a rule hard to find in the document
 * it came from.
 */
export function autonomyOf(rules: ProjectRules): AutonomyRule[] {
  const clocks: AutonomyRule[] = rules.schedules.map((schedule) => ({
    name: schedule.name,
    clock: "cron",
    when: schedule.cron,
    // `null` is UTC — the scheduler's own default, not an unset field.
    zone: schedule.timezone ?? "UTC",
    state: scheduleNeverFires(schedule)
      ? "never-fires"
      : scheduleCapped(schedule)
        ? "capped"
        : "armed",
    problem: schedule.problem,
    prompt: schedule.prompt,
    cwd: schedule.cwd,
    next: schedule.next_fire_at,
    last: schedule.last_fired_at,
    today: { fired: schedule.fires_today, cap: schedule.daily_cap },
    sha: null,
  }));

  const commits: AutonomyRule[] = rules.repo_triggers.map((trigger) => ({
    name: trigger.name,
    clock: "commit",
    when: trigger.branch,
    zone: null,
    state: trigger.last_sha === null ? "unseen" : "armed",
    problem: null,
    prompt: trigger.prompt,
    cwd: null,
    next: null,
    last: null,
    today: null,
    sha: trigger.last_sha,
  }));

  return [...clocks, ...commits];
}

/* ------------------------------------------------------------- concerns -- */

/**
 * The ways a project can be stopped without looking stopped.
 *
 * Every one of these means *this project is doing less than somebody thinks*,
 * and not one of them is visible from the roster. They used to be scattered
 * across three of five stacked panels, two of them in muted body text — so the
 * page that a completely halted project produced was one alert, two panels
 * saying "nothing is scheduled" (which the alert had just disclaimed), and the
 * two most consequential sentences on the page in the quietest style on it.
 */
export type ConcernKind =
  | "rules-unreadable"
  | "gate-missing"
  | "folder-gone"
  | "rules-inert"
  | "brake-holding"
  | "folder-unset";

/**
 * How bad a finding is, and therefore what to do about it.
 *
 * Three, because they ask for three different responses: `stopped` is happening
 * now and will not clear itself, `held` clears when the thing it waits for
 * happens, and `unfinished` is a setup nobody completed. Collapsing them into
 * one alarm makes a brake that is working correctly look like a fault.
 */
export type ConcernWeight = "stopped" | "held" | "unfinished";

/**
 * Where a finding is put right, named by what you do there.
 *
 * A route and a verb rather than the name of a tab. The strip used to link every finding to "On its
 * own" — including "no folder has been recorded", which that view cannot fix: the folder is named
 * on the Autopilot page, and the concern's own sentence was the only place that said so, as text.
 * A diagnosis that does not lead to the fix leaves the person who has to make it holding both.
 */
export interface ConcernFix {
  /** A route that exists in `router.tsx` today. Never one this file wishes existed. */
  to: string;
  /** What the person will do there, as a verb phrase. */
  label: string;
}

export interface Concern {
  kind: ConcernKind;
  weight: ConcernWeight;
  /** What is wrong and what it costs, in one sentence. */
  said: string;
  /**
   * The inspector view that shows the finding at length. The strip drops the finding on that view
   * — the block below already says it, with its own way to the fix — rather than printing it twice.
   */
  view: ProjectView;
  /** Where it is fixed. */
  fix: ConcernFix;
}

/**
 * The rules file as the page names it: the daemon's `~`-spelled path when it sent one, and the bare
 * file name from a daemon too old to.
 */
export function rulesFileName(rules: ProjectRules): string {
  return rules.rules_path ?? "autopilot.yaml";
}

/**
 * Where a project's `autopilot.yaml` is edited: the workspace's State mode, whose "Files the app
 * owns" section holds the raw editor. The State mode and not the inspector, because the inspector
 * reads the tree and that editor is the file's legitimate author (`project/OwnedFiles.tsx`).
 */
export function rulesEditorPath(projectId: string): string {
  return `/projects/${projectId}/state`;
}

/** Where a project's folder is recorded — the Autopilot page, which names it when a mode is set. */
export const FOLDER_FIX_PATH = "/autopilot";

/**
 * Everything wrong with this project, worst first.
 *
 * `rootExists` is the roster's own answer and is not on the rules read, so it
 * is passed in rather than fetched again: `null` means no folder was ever
 * named, `false` means one was and is not there, and `undefined` is a daemon
 * older than this shell — which reads as "not named" rather than inventing a
 * folder that is there.
 *
 * Each sentence is a headline and not a paragraph: the panel that answers the
 * finding says it again at length, and two near-identical paragraphs seven
 * hundred pixels apart read as a defect rather than as a summary.
 *
 * Returns empty when there is nothing wrong, and the strip that draws it
 * renders nothing at all for an empty list. A permanently visible "all clear"
 * would be a hole in every healthy project's page.
 */
export function concernsOf(rules: ProjectRules, rootExists: boolean | null | undefined): Concern[] {
  const found: Concern[] = [];

  if (rules.rules_file === "unreadable") {
    found.push({
      kind: "rules-unreadable",
      weight: "stopped",
      said: "The rules file will not parse — nothing runs here at all.",
      view: "rules",
      fix: { to: rulesEditorPath(rules.project_id), label: "Edit autopilot.yaml" },
    });
  }

  // The queue refuses every merge while this holds, over a key in an unreviewed
  // file. A refusal nobody can explain is the worst of the gate's three states.
  const gated = rules.gate_command !== null && rules.gate_command.trim() !== "";
  if (rules.gate_before_publish && !gated) {
    found.push({
      kind: "gate-missing",
      weight: "stopped",
      said: "Merges need a gate and none is set — every merge is refused.",
      view: "rules",
      fix: { to: rulesEditorPath(rules.project_id), label: "Set a gate in autopilot.yaml" },
    });
  }

  if (rules.project_root !== null && rootExists === false) {
    found.push({
      kind: "folder-gone",
      weight: "stopped",
      said: "The recorded folder is not on this disk — nothing here can be read.",
      view: "browse",
      fix: { to: FOLDER_FIX_PATH, label: "Record the folder again on Autopilot" },
    });
  }

  /* `stopped`, not `held`. The rule's own badge says `never fires` in Wrong Red, and the strip said
     the same fact in ember with a `!` — two weights for one finding. It is the stopped kind by this
     file's own definition: happening now, and it will not clear itself. */
  const inert = autonomyOf(rules).filter((rule) => rule.state === "never-fires").length;
  if (inert > 0) {
    found.push({
      kind: "rules-inert",
      weight: "stopped",
      said:
        inert === 1
          ? "One rule is armed and can never fire."
          : `${inert} rules are armed and can never fire.`,
      view: "rules",
      fix: {
        to: rulesEditorPath(rules.project_id),
        label: inert === 1 ? "Fix the rule in autopilot.yaml" : "Fix the rules in autopilot.yaml",
      },
    });
  }

  if (rules.queue_full) {
    found.push({
      kind: "brake-holding",
      weight: "held",
      said: "The ceiling is holding new work back until something is reviewed.",
      view: "rules",
      fix: { to: "/waiting", label: "Review what is waiting" },
    });
  }

  if (rules.project_root === null) {
    found.push({
      kind: "folder-unset",
      weight: "unfinished",
      said: "No folder has been recorded, so there is nothing to look inside.",
      // `browse` and not `rules`: the browse view is the one whose refusal says this at length.
      view: "browse",
      fix: { to: FOLDER_FIX_PATH, label: "Record a folder on Autopilot" },
    });
  }

  return found;
}

/**
 * One derived sentence about this project, for the page header.
 *
 * The header used to carry a constant — "reading the folder as it is on disk
 * right now" — which describes the page rather than reporting on its subject,
 * and which is false on the view that reads no folder at all. `PageHeader` says
 * what belongs here in its own docstring: *the line that changes*.
 *
 * Nothing to say returns `undefined`, and the header then draws no line.
 */
export function headlineFor(rules: ProjectRules): string {
  const parts: string[] = [rules.project_root ?? "no folder recorded"];

  const running = autonomyOf(rules);
  if (rules.rules_file === "unreadable") {
    parts.push("rules unreadable");
  } else if (running.length === 0) {
    parts.push("nothing runs on its own");
  } else {
    const inert = running.filter((rule) => rule.state === "never-fires").length;
    const said = `${running.length} ${running.length === 1 ? "rule" : "rules"} on its own`;
    parts.push(inert === 0 ? said : `${said}, ${inert} never firing`);
  }

  // Where they are, not just how many. See `whereWaiting` — a full queue and an empty proposals
  // list were both true at once, and the total alone could not say so.
  const where = whereWaiting(rules);
  const suffix = where === null ? "" : ` (${where})`;
  if (rules.wip_limit === null) {
    parts.push(`${rules.open_review_items} open${suffix}, no ceiling`);
  } else {
    parts.push(
      `${rules.open_review_items} of ${rules.wip_limit} open${suffix}${rules.queue_full ? ", holding" : ""}`,
    );
  }

  return parts.join(" · ");
}

/* ---------------------------------------------------------- reading text -- */

/**
 * A diff's lines, tagged by what git's first column says.
 *
 * Colour alone would not carry this — the `+` and the `-` are already at the
 * start of each line and do the work for anybody who cannot separate the hues.
 * The tag is what lets the stylesheet reinforce it.
 */
export type DiffLineKind = "add" | "remove" | "hunk" | "file" | "context";

export interface DiffLine {
  kind: DiffLineKind;
  text: string;
}

/**
 * How many lines of a diff are worth drawing one span each.
 *
 * A `git diff` of a dirty working tree has no upper bound — a regenerated lock
 * file alone is tens of thousands of lines — and a span per line is a DOM node
 * per line. Past this the diff is drawn as one block of text and the page says
 * why, which is honest and stays fast.
 */
export const DIFF_LINE_CAP = 2000;

export function diffLines(diff: string): DiffLine[] {
  return diff.split("\n").map((text) => ({ kind: diffLineKind(text), text }));
}

function diffLineKind(line: string): DiffLineKind {
  if (line.startsWith("@@")) return "hunk";
  // `+++` and `---` are the file header's, not a changed line's, and they come
  // in a pair that would otherwise read as one addition and one removal.
  if (line.startsWith("+++") || line.startsWith("---")) return "file";
  if (line.startsWith("diff --git") || line.startsWith("index ")) return "file";
  if (line.startsWith("+")) return "add";
  if (line.startsWith("-")) return "remove";
  return "context";
}

/** Grep hits gathered under the file they are in, in the order the daemon sent them. */
export interface MatchGroup {
  path: string;
  matches: InspectMatch[];
}

/**
 * Matches grouped by file.
 *
 * A flat list repeats the path once per hit — forty times for a common word —
 * and the path is the longest thing on the row. Grouped, the path is said once
 * and the lines sit under it, which is what every reader of grep output does.
 */
export function groupMatches(matches: InspectMatch[]): MatchGroup[] {
  const groups: MatchGroup[] = [];
  for (const match of matches) {
    const last = groups[groups.length - 1];
    if (last !== undefined && last.path === match.path) {
      last.matches.push(match);
      continue;
    }
    groups.push({ path: match.path, matches: [match] });
  }
  return groups;
}
