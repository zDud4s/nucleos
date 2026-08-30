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
  /**
   * Whether the VCS queue runs that command on a merge before publishing it.
   * Read here rather than derived: the rule lives in `.ai/autopilot.yaml`, and a
   * second copy of it in the window is a copy that will eventually disagree.
   */
  gate_before_publish: boolean;
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

/** The word each state deserves, said once, in the one vocabulary. */
export const RULE_STATE_WORD: Record<RuleState, string> = {
  armed: "armed",
  "never-fires": "never fires",
  capped: "capped today",
  unseen: "no commit seen yet",
};

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

export interface Concern {
  kind: ConcernKind;
  weight: ConcernWeight;
  /** What is wrong and what it costs, in one sentence. */
  said: string;
  /** The view that answers it, so a finding leads somewhere. */
  view: ProjectView;
}

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
    });
  }

  // The queue refuses every merge while this holds, over a key in a gitignored
  // file. A refusal nobody can explain is the worst of the gate's three states.
  const gated = rules.gate_command !== null && rules.gate_command.trim() !== "";
  if (rules.gate_before_publish && !gated) {
    found.push({
      kind: "gate-missing",
      weight: "stopped",
      said: "Merges need a gate and none is set — every merge is refused.",
      view: "rules",
    });
  }

  if (rules.project_root !== null && rootExists === false) {
    found.push({
      kind: "folder-gone",
      weight: "stopped",
      said: "The recorded folder is not on this disk — nothing here can be read.",
      view: "browse",
    });
  }

  const inert = autonomyOf(rules).filter((rule) => rule.state === "never-fires").length;
  if (inert > 0) {
    found.push({
      kind: "rules-inert",
      weight: "held",
      said:
        inert === 1
          ? "One rule is armed and can never fire."
          : `${inert} rules are armed and can never fire.`,
      view: "rules",
    });
  }

  if (rules.queue_full) {
    found.push({
      kind: "brake-holding",
      weight: "held",
      said: "The ceiling is holding new work back until something is reviewed.",
      view: "rules",
    });
  }

  if (rules.project_root === null) {
    found.push({
      kind: "folder-unset",
      weight: "unfinished",
      said: "No folder has been recorded, so there is nothing to look inside.",
      view: "rules",
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

  if (rules.wip_limit === null) {
    parts.push(`${rules.open_proposals} open, no ceiling`);
  } else {
    parts.push(
      `${rules.open_proposals} of ${rules.wip_limit} open${rules.queue_full ? ", holding" : ""}`,
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
