import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { apiFetch } from "./client";
import { keys } from "./keys";
import { POLL, pollWhile } from "./poll";

/**
 * What a project can be asked to do to itself.
 *
 * **Not `data/commands.ts`.** That one is the slash commands a conversation offers. These are shell
 * commands somebody declared for a project — `gate`, `fmt`, `typecheck` — and the two share a word
 * and nothing else.
 */

/** Who may run a command. `person` is the default the núcleo stores when nobody said. */
export type RunnableBy = "person" | "agent";

/** Where a command came from. `project` shadows `workflow` of the same name; the núcleo resolves it. */
export type CommandSource = "project" | "workflow";

/**
 * What a command last did.
 *
 * Four, and the absence of a `last` is a fifth. `errored` is not `failed`: one says the project is
 * broken and the other says we could not tell — a missing binary, a timeout, a signal. A surface
 * that drew them the same colour would report broken tests when the measurement broke.
 */
export type CommandOutcome = "running" | "passed" | "failed" | "errored";

export interface LastRun {
  outcome: CommandOutcome;
  started_at: string;
  /** `null` while it is still running, and only then. */
  ended_at: string | null;
  /** `null` when there was no exit code at all — a signal, or a command that never started. */
  exit_code: number | null;
  output: string | null;
}

export interface ProjectCommand {
  id: number;
  name: string;
  command: string;
  /** Relative to the project root. `null` is the root itself. */
  cwd: string | null;
  is_gate: boolean;
  pass_exit_code: number;
  runnable_by: RunnableBy;
  source: CommandSource;
  /** `null` for a command nobody has run — which is not a command that failed. */
  last: LastRun | null;
}

export interface Declaration {
  projectId: string;
  name: string;
  command: string;
  cwd?: string | null;
  is_gate?: boolean;
  pass_exit_code?: number;
  runnable_by?: RunnableBy;
}

/**
 * The commands this project offers, gates first.
 *
 * **Polled only while something is running.** `pollWhile` decides per tick against the data already
 * in the cache, so the moment the last command settles the poll switches itself off — a list of
 * declarations does not change on its own, and a three-second tick against a page nobody is acting
 * on is a cost with no reader.
 */
export function useProjectCommands(projectId: string | null) {
  return useQuery({
    queryKey: keys.projects.commands(projectId ?? ""),
    queryFn: () =>
      apiFetch<ProjectCommand[]>(`/projects/${encodeURIComponent(projectId ?? "")}/commands`),
    enabled: projectId !== null,
    refetchInterval: pollWhile<ProjectCommand[]>(POLL.fast, (rows) =>
      rows.some((row) => row.last?.outcome === "running"),
    ),
  });
}

/**
 * Start one, and stop waiting.
 *
 * **The route answers 202 and the work carries on without it.** So there is nothing to draw from
 * the response: the result lands on the command's own row, the list above picks it up on its next
 * tick, and a reload in between shows the same thing. That is also why the invalidation matters
 * more than usual — it is what turns the 202 into a `running` badge without a wait.
 *
 * No `retry`. Every refusal here is settled — the stop is engaged, the command is already running,
 * the folder is gone — and asking again would answer the same thing one second later.
 */
export function useRunProjectCommand() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: ({ projectId, id }: { projectId: string; id: number }) =>
      apiFetch<void>(
        `/projects/${encodeURIComponent(projectId)}/commands/${id}/run`,
        { method: "POST" },
      ),
    retry: false,
    onSettled: () => {
      void queryClient.invalidateQueries({ queryKey: keys.projects.all });
    },
  });
}

/**
 * Declare one, replacing any of the same name.
 *
 * An upsert, because the name is the identity: editing `gate` is editing the `gate` you have. The
 * núcleo refuses a declaration that could only ever fail — an unbalanced quote, a folder outside
 * the project — with the reason in `detail`, which is the half that makes the form worth having
 * over a text file.
 */
export function useDeclareProjectCommand() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: ({ projectId, ...declaration }: Declaration) =>
      apiFetch<{ id: number }>(`/projects/${encodeURIComponent(projectId)}/commands`, {
        method: "POST",
        body: JSON.stringify(declaration),
      }),
    retry: false,
    onSettled: () => {
      void queryClient.invalidateQueries({ queryKey: keys.projects.all });
    },
  });
}

/** Forget one. */
export function useForgetProjectCommand() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: ({ projectId, id }: { projectId: string; id: number }) =>
      apiFetch<void>(`/projects/${encodeURIComponent(projectId)}/commands/${id}`, {
        method: "DELETE",
      }),
    retry: false,
    onSettled: () => {
      void queryClient.invalidateQueries({ queryKey: keys.projects.all });
    },
  });
}

/**
 * The tone a verdict is drawn in, or `null` for a command nobody has run.
 *
 * Four answers for four states, and `null` is the fifth. `errored` gets `paused` — the same amber
 * the gate reading uses for "could not run" — rather than the red `failed` gets, because those two
 * facts send somebody to two different places.
 */
export function outcomeTone(last: LastRun | null): string | null {
  switch (last?.outcome) {
    case "passed":
      return "active";
    case "failed":
      return "danger";
    case "errored":
      return "paused";
    case "running":
      return "info";
    default:
      return null;
  }
}

/** What a verdict says, in words, for the row under a button. */
export function outcomeSentence(last: LastRun | null): string {
  if (last === null) return "never run here";
  switch (last.outcome) {
    case "running":
      return "running now";
    case "passed":
      return "passed";
    case "failed":
      return last.exit_code === null ? "failed" : `failed with exit ${last.exit_code}`;
    // Deliberately not "failed". A command that could not be measured says nothing about the
    // project, and the words have to keep saying so after the colour has been forgotten.
    case "errored":
      return "could not be measured";
  }
}
