// §spec motor-de-workflows
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { apiFetch, isApiRefusal } from "./client";
import { keys } from "./keys";

/**
 * The workflow library, and what one project uses out of it.
 *
 * A workflow is a graph of how work moves through a project. The núcleo holds it as a **bundle** —
 * a directory in `~/.nucleos/workflows/<name>/<version>/` — and a project keeps a **pin** naming
 * one, in `~/.nucleos/projects/<id>/workflows.yaml`. The pin is this app's record of what the
 * project uses, so it lives with the app's state and not in the project; an ejected copy is the
 * project's own files and stays in the project, under `.ai/workflows/<name>/`.
 *
 * **Nothing here polls.** A pin does not change unless somebody changes it, and neither does a
 * folder full of markdown; a three-second tick against either would be a cost with no reader. What
 * makes a listing go stale is a mutation on this page, and every one of them invalidates.
 */

/**
 * Where a project's workflow stands. Four, and none of them collapses into another.
 *
 * - `referenced` — the library has the pinned bundle and it is the one that was pinned.
 * - `drifted` — the library has it and it has changed underneath the pin.
 * - `ejected` — this project has its own copy, and stopped receiving updates when it took it.
 * - `missing` — pinned, and this machine's library has nothing at that coordinate. The new-machine
 *   case: the pin travelled with the repository and the bundle did not.
 */
export type Standing = "referenced" | "drifted" | "ejected" | "missing";

/** One bundle on this machine. */
export interface Bundle {
  name: string;
  version: string;
  description: string | null;
  /** Where it came from, for a machine that does not have it. */
  origin: string;
  /** Files in a project this workflow declares itself the author of. */
  owns: string[];
  hash: string;
  /** Absolute, for the door to the editor. Never joined onto. */
  path: string;
}

export interface Installed {
  name: string;
  version: string;
  origin: string;
  /** What the bundle hashed to when it was pinned. */
  hash: string;
  standing: Standing;
  /** When this project took its own copy — and so, when the updates stopped. */
  ejected_at: string | null;
  /** What the library's copy hashes to now. `null` when the library has nothing there. */
  origin_hash: string | null;
  /** What this project's own copy hashes to, for an ejected workflow. */
  local_hash: string | null;
  /** A higher version in the library. An offer, never drift — the two are different facts. */
  update_available: string | null;
  description: string | null;
  owns: string[];
  overridden_nodes: number;
  disabled_nodes: number;
}

export type Change = "added" | "removed" | "changed";

export interface FileChange {
  path: string;
  change: Change;
}

export interface WorkflowDiff {
  origin_version: string;
  changes: FileChange[];
  /** Counted rather than listed: the interesting half is the short one. */
  unchanged: number;
}

/** Every bundle on this machine, whatever any project uses. */
export function useWorkflowLibrary() {
  return useQuery({
    queryKey: keys.workflows.library,
    queryFn: () => apiFetch<Bundle[]>("/workflows/library"),
    // A 503 means this machine has nowhere to keep a library, which is settled: asking again gets
    // the same answer, and the retries would only delay the sentence that explains it.
    retry: false,
  });
}

/** What this project uses, measured against the library as it is right now. */
export function useProjectWorkflows(projectId: string | null) {
  return useQuery({
    queryKey: keys.projects.workflows(projectId ?? ""),
    queryFn: () =>
      apiFetch<Installed[]>(`/projects/${encodeURIComponent(projectId ?? "")}/workflows`),
    enabled: projectId !== null,
    retry: false,
  });
}

/**
 * Which files this project's copy differs from the origin in.
 *
 * Only asked for when somebody opens it, because it walks two directories on the daemon's disk and
 * the answer is not part of the page's first paragraph. `enabled` is what makes the button that
 * opens it the thing that pays for it.
 */
export function useWorkflowDiff(projectId: string, name: string | null) {
  return useQuery({
    queryKey: keys.projects.workflowDiff(projectId, name ?? ""),
    queryFn: () =>
      apiFetch<WorkflowDiff>(
        `/projects/${encodeURIComponent(projectId)}/workflows/${encodeURIComponent(name ?? "")}/diff`,
      ),
    enabled: name !== null,
    retry: false,
  });
}

/** How one command compares with the one the project runs today. */
export type CommandChange = "new" | "unchanged" | "changed";

/** One command node, exactly as the bundle declares it. */
export interface ConsentCommand {
  node: string;
  label: string;
  /** Verbatim. Never shortened, never summarised: the string is what will run. */
  command: string;
  cwd: string | null;
  timeout: string | null;
  change: CommandChange;
  /** Present only when `change` is `changed`. */
  previous_command?: string;
  previous_cwd?: string;
  /** What this project runs instead, when its own overlay replaces the command. */
  project_override?: string;
  /** Switched off in this project: in the graph, and not run. */
  disabled: boolean;
}

/** A command the version running today has and the new one dropped. */
export interface ConsentRemoved {
  node: string;
  command: string;
  cwd: string | null;
}

/** What installing or updating to `version` would let run in this project. */
export interface WorkflowConsent {
  version: string;
  /** The version the project runs today. `null` for a first install: nothing to diff against. */
  baseline: string | null;
  baseline_source?: "project" | "library";
  commands: ConsentCommand[];
  removed: ConsentRemoved[];
}

/**
 * Every command a bundle version would run here, verbatim, set against what runs today.
 *
 * Asked for when somebody opens the install or update step, and never in the background: the
 * answer is read off the daemon's disk, and it only matters at the one moment a person is about to
 * agree. `retry: false` because every refusal is settled — no such bundle, a graph that will not
 * parse — and asking again answers the same thing.
 */
export function useWorkflowConsent(projectId: string, name: string, version: string | null) {
  return useQuery({
    queryKey: keys.projects.workflowConsent(projectId, name, version ?? ""),
    queryFn: () =>
      apiFetch<WorkflowConsent>(
        `/projects/${encodeURIComponent(projectId)}/workflows/${encodeURIComponent(name)}/consent?version=${encodeURIComponent(version ?? "")}`,
      ),
    enabled: version !== null,
    retry: false,
    // A bundle is a folder of files and does not change while somebody reads it; but a person who
    // closes the step and opens it again after editing the library expects the new text, and a
    // cached listing from before the edit is a consent to something that is no longer there.
    gcTime: 0,
  });
}

/**
 * What differs from what runs today, and nothing else.
 *
 * The diff is the defence against a patch release that adds one line to a node that used to run
 * the tests, so it is the short list: commands that changed, commands that are new, commands that
 * were dropped. A first install has no baseline and so no delta; the full listing is what shows.
 * Empty means nothing about what runs moved, and the page then has nothing to say.
 */
export function commandDelta(consent: WorkflowConsent): {
  changed: ConsentCommand[];
  added: ConsentCommand[];
  removed: ConsentRemoved[];
} {
  if (consent.baseline === null) return { changed: [], added: [], removed: [] };
  return {
    changed: consent.commands.filter((line) => line.change === "changed"),
    added: consent.commands.filter((line) => line.change === "new"),
    removed: consent.removed,
  };
}

/**
 * Everything that changes a pin, through one hook shape.
 *
 * Four mutations that differ only in the verb, so they are one function rather than four
 * near-copies: the invalidation, the `retry: false` and the refusal handling are identical, and
 * four copies of them is four places for one of them to drift.
 *
 * `retry: false` throughout. Every refusal here is settled — the stop is engaged, there is no such
 * bundle, a copy is already there — and asking again answers the same thing a second later.
 */
function useWorkflowChange<TVariables extends { projectId: string }>(
  request: (variables: TVariables) => { path: string; method: string; body?: unknown },
) {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (variables: TVariables) => {
      const { path, method, body } = request(variables);
      return apiFetch<void>(path, {
        method,
        ...(body === undefined ? {} : { body: JSON.stringify(body) }),
      });
    },
    retry: false,
    onSettled: () => {
      // Both prefixes. A pin is a project's, but installing one also changes what the *ownership*
      // route answers — a bundle declares the files it authors — and that lives under the same
      // project prefix. The library is invalidated too because a listing is a measurement against
      // it, and the two must not be able to disagree on screen.
      void queryClient.invalidateQueries({ queryKey: keys.projects.all });
      void queryClient.invalidateQueries({ queryKey: keys.workflows.all });
    },
  });
}

/**
 * The daemon's `422 consent_required`: the listing it would accept and the digest that stands for it.
 *
 * The digest is only ever learned from this refusal, so a client re-sends the POST with
 * `consent_digest` set to it once the person has agreed to exactly these commands.
 */
export interface ConsentRefusal {
  digest: string;
  consent: WorkflowConsent;
}

/** Read a `consent_required` refusal, or `null` for any other error. */
export function consentFromRefusal(error: unknown): ConsentRefusal | null {
  if (!isApiRefusal(error) || error.code !== "consent_required") return null;
  const body = error.body as Record<string, unknown> | undefined;
  if (typeof body !== "object" || body === null || typeof body.digest !== "string") return null;
  const commands = Array.isArray(body.commands) ? (body.commands as ConsentCommand[]) : [];
  const removed = Array.isArray(body.removed) ? (body.removed as ConsentRemoved[]) : [];
  return {
    digest: body.digest,
    consent: {
      version: typeof body.version === "string" ? body.version : "",
      baseline: typeof body.baseline === "string" ? body.baseline : null,
      baseline_source: body.baseline_source === "project" || body.baseline_source === "library"
        ? body.baseline_source
        : undefined,
      commands,
      removed,
    },
  };
}

/** Whether two listings let exactly the same commands run, ignoring presentation fields. */
export function sameCommands(a: WorkflowConsent, b: WorkflowConsent): boolean {
  const key = (consent: WorkflowConsent) =>
    JSON.stringify(
      consent.commands.map((line) => [line.node, line.command, line.cwd, line.change]),
    );
  return key(a) === key(b);
}

export function useInstallWorkflow() {
  return useWorkflowChange<{
    projectId: string;
    name: string;
    version: string;
    consentDigest?: string;
  }>(({ projectId, name, version, consentDigest }) => ({
    path: `/projects/${encodeURIComponent(projectId)}/workflows`,
    method: "POST",
    body:
      consentDigest === undefined ? { name, version } : { name, version, consent_digest: consentDigest },
  }));
}

export function useEjectWorkflow() {
  return useWorkflowChange<{ projectId: string; name: string }>(({ projectId, name }) => ({
    path: `/projects/${encodeURIComponent(projectId)}/workflows/${encodeURIComponent(name)}/eject`,
    method: "POST",
    body: {},
  }));
}

export function useUpdateWorkflow() {
  return useWorkflowChange<{
    projectId: string;
    name: string;
    version?: string;
    consentDigest?: string;
  }>(({ projectId, name, version, consentDigest }) => ({
    path: `/projects/${encodeURIComponent(projectId)}/workflows/${encodeURIComponent(name)}/update`,
    method: "POST",
    // Absent means the newest the library has. The page always sends what it showed, so the
    // button cannot install something other than what its label says.
    body: {
      ...(version === undefined ? {} : { version }),
      ...(consentDigest === undefined ? {} : { consent_digest: consentDigest }),
    },
  }));
}

export function useForgetWorkflow() {
  return useWorkflowChange<{ projectId: string; name: string }>(({ projectId, name }) => ({
    path: `/projects/${encodeURIComponent(projectId)}/workflows/${encodeURIComponent(name)}`,
    method: "DELETE",
  }));
}

/**
 * The tone a standing is drawn in.
 *
 * Three tones for four standings, and the pairing is the argument. `drifted` and `missing` share
 * Awaiting You Amber because both are *the thing you pinned is not the thing you have*, and both
 * wait on a decision only the owner can make — follow the library again or eject, fetch the bundle
 * or stop using it. That is the one tone that asks something of you, which is exactly what they do.
 * The comment here used to say "both are amber" over a function returning Held Ember, which is
 * *stopped by a rule or a ceiling* — and nothing has stopped.
 *
 * `ejected` is Switched Off Grey: off on purpose, a deliberate choice, and colouring a decision as a
 * warning is how a page teaches people to ignore its colours. It was Deliberating Violet, which is
 * shadow mode — so on a project in shadow the same violet meant two unrelated things one line apart.
 */
export function standingTone(standing: Standing): string {
  switch (standing) {
    case "referenced":
      return "active";
    case "ejected":
      return "off";
    default:
      return "pending";
  }
}

/**
 * Where the library lives, as a person types it. What the empty states point at.
 *
 * The daemon does not serve the absolute root, so a page with no bundle to read it off can say it
 * but not open it — see `libraryRootOf`.
 */
export const LIBRARY_HINT = "~/.nucleos/workflows/";

/**
 * The library's folder on this machine, read off a bundle that lives in it.
 *
 * A bundle's `path` is `<root>/<name>/<version>`, absolute, so two segments up is the root the
 * daemon walked. `null` when there is no bundle to read it from — an empty library has no answer
 * here, and guessing the home directory from the webview would be a claim about a machine the
 * daemon, not this page, is the authority on.
 */
export function libraryRootOf(bundles: Bundle[] | undefined): string | null {
  const first = bundles?.[0];
  if (first === undefined) return null;
  const parts = first.path.split(/[\\/]/);
  if (parts.length < 3) return null;
  const separator = first.path.includes("\\") && !first.path.includes("/") ? "\\" : "/";
  return parts.slice(0, -2).join(separator);
}

/**
 * What no workflow installed *means*, in the one place both surfaces read it from.
 *
 * Two screens say it and they rightly say it in two shapes: the State mode says it as a status
 * with the reason behind a question, and the Workflows mode says it as the heading of a library
 * somebody has arrived at meaning to install something. The **claim** inside them is one claim,
 * and it was written twice — one copy said the app does not pretend otherwise by drawing an empty
 * graph and the other did not, so the same nothing had two explanations and only one of them
 * mentioned the graph.
 *
 * `MODE_SENTENCES` next door exists for exactly this reason, and `Settings` states it: "two copies
 * of a sentence about restraint would eventually say two different things".
 *
 * Worded for the task and not for the design: it says what is true of the project and what a
 * workflow would change, rather than arguing that the absence is "a real answer and not a gap" —
 * which was the design document talking, and read as a shrug to somebody who came to install one.
 */
export const NO_WORKFLOW_MEANS =
  "Nothing is broken: work here runs however whoever is at the keyboard runs it. A workflow writes the steps down — which model does what, and which check has to pass.";

/**
 * What a standing means, in words, with the numbers that make it checkable.
 *
 * Pure, and tested without a render, because this is where the never-collapse contract is actually
 * kept: the four sentences have to stay four. `now` is injected for the same reason `relativeText`
 * injects it — a sentence that depends on the clock is a sentence a test cannot pin otherwise.
 */
export function standingSentence(installed: Installed, now: number): string {
  switch (installed.standing) {
    case "referenced":
      return `following ${installed.version} in the library`;
    case "drifted":
      return `${installed.version} in the library is no longer the ${installed.version} this project pinned`;
    case "missing":
      return `nothing in this machine's library is ${installed.name} ${installed.version}`;
    case "ejected":
      return installed.ejected_at === null
        ? "this project has its own copy"
        : `this project's own copy, receiving no updates for ${sinceText(installed.ejected_at, now)}`;
  }
}

/**
 * How long a copy has been on its own, in the coarsest unit that is still true.
 *
 * Coarse on purpose. §6.1 asks how long the bundle has been frozen, and the answer that changes
 * anybody's mind is *months*, not *eleven weeks and two days*. An unparseable timestamp comes back
 * as "some time" rather than "NaN days", which would hide a daemon that changed its format behind
 * a word that looks like a number.
 */
export function sinceText(at: string, now: number): string {
  const when = Date.parse(at);
  if (Number.isNaN(when)) return "some time";
  const days = Math.floor((now - when) / 86_400_000);
  if (days < 1) return "less than a day";
  if (days < 14) return `${days} ${days === 1 ? "day" : "days"}`;
  const weeks = Math.floor(days / 7);
  if (days < 60) return `${weeks} weeks`;
  const months = Math.floor(days / 30);
  return `${months} months`;
}

/**
 * Whether anything about this project's workflows is worth leading the page with.
 *
 * `drifted` and `missing` only. An ejected copy is a choice, not a concern — the page says how long
 * it has been frozen where it lists it, and shouting about it at the top would be shouting about a
 * decision somebody made on purpose. An update on offer is not a concern either: nothing is wrong,
 * something is merely newer.
 */
export function driftingWorkflows(rows: Installed[] | undefined): Installed[] {
  return (rows ?? []).filter(
    (row) => row.standing === "drifted" || row.standing === "missing",
  );
}
