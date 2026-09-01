// §spec motor-de-workflows
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { apiFetch } from "./client";
import { keys } from "./keys";

/**
 * The workflow library, and what one project uses out of it.
 *
 * A workflow is a graph of how work moves through a project. The núcleo holds it as a **bundle** —
 * a directory in `~/.nucleos/workflows/<name>/<version>/` — and a project keeps a **pin** naming
 * one, in its own `.ai/workflows.yaml`.
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

export function useInstallWorkflow() {
  return useWorkflowChange<{ projectId: string; name: string; version: string }>(
    ({ projectId, name, version }) => ({
      path: `/projects/${encodeURIComponent(projectId)}/workflows`,
      method: "POST",
      body: { name, version },
    }),
  );
}

export function useEjectWorkflow() {
  return useWorkflowChange<{ projectId: string; name: string }>(({ projectId, name }) => ({
    path: `/projects/${encodeURIComponent(projectId)}/workflows/${encodeURIComponent(name)}/eject`,
    method: "POST",
    body: {},
  }));
}

export function useUpdateWorkflow() {
  return useWorkflowChange<{ projectId: string; name: string; version?: string }>(
    ({ projectId, name, version }) => ({
      path: `/projects/${encodeURIComponent(projectId)}/workflows/${encodeURIComponent(name)}/update`,
      method: "POST",
      // Absent means the newest the library has. The page always sends what it showed, so the
      // button cannot install something other than what its label says.
      body: version === undefined ? {} : { version },
    }),
  );
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
 * Three tones for four standings, and the pairing is the argument. `drifted` and `missing` are both
 * amber because both are *the thing you pinned is not the thing you have* — something to look at,
 * not something broken. `ejected` is not amber: it is a deliberate choice somebody made, and
 * colouring a decision as a warning is how a page teaches people to ignore its colours.
 */
export function standingTone(standing: Standing): string {
  switch (standing) {
    case "referenced":
      return "active";
    case "ejected":
      return "shadow";
    default:
      return "paused";
  }
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
 */
export const NO_WORKFLOW_MEANS =
  "This project develops however whoever is at the keyboard decides — which is a real answer and not a gap.";

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
