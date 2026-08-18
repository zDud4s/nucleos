import { keepPreviousData, useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { apiFetch } from "./client";
import { keys } from "./keys";
import { POLL } from "./poll";

/**
 * Errands — standing work on a Telegram topic: the list, one errand's files,
 * its notebook, its rules, and the PATCH that pauses, resumes, changes brain
 * or sets an investigation.
 *
 * Every shape below was read off `core/src/errands.rs` and the routes
 * `core/src/http.rs` mounts under `/errands`, field for field.
 *
 * **There is no `GET /errands/{id}`.** An errand's detail is a row out of the
 * same list this file polls, not a query of its own: `usePatchErrand` and
 * `useCloseErrand` invalidate the list, and the refetch that follows is what
 * a detail screen sees change.
 *
 * **`POST /errands` is not built here.** It takes a `chat_key` composed by
 * the Telegram sidecar, and nothing on this side of the wire can invent one
 * — `Errands.tsx`'s empty state says so rather than offering a form that
 * cannot work.
 */

export type Brain = "cloud" | "local";
export type ErrandStatus = "active" | "paused" | "done";

/** One row of the list — and the whole of an errand's detail too. `errands::Errand`. */
export interface Errand {
  id: number;
  name: string;
  /** The topic this errand sits on. Composed by the Telegram sidecar; the shell never writes one. */
  chat_key: string;
  brain: Brain;
  folder: string;
  status: ErrandStatus;
  /** `null` means this errand answers when spoken to and nothing else. */
  done_when: string | null;
  /** How many more turns of its own initiative it may take. `0` is a real ceiling, not an absence. */
  windows_left: number;
}

/** One standing instruction of an errand — `errands::Rule`. */
export interface ErrandRule {
  id: number;
  errand_id: number;
  name: string;
  cron: string;
  prompt: string;
  /** `null` is UTC — the scheduler's own default, not an unset field. */
  timezone: string | null;
  last_fired_at: string;
  fires_date: string | null;
  fires_today: number;
  created_at: string;
}

/* ------------------------------------------------------------------ keys -- */

/**
 * `keys.errands` landed as a minimal `{ all }` root — this packet's roster of
 * allowed files does not include `data/keys.ts`, so every sub-key is built
 * here, under that root, the same way `COUNCIL_KEYS` extends `keys.council.all`
 * in `data/council.ts`.
 */
const ERRAND_KEYS = {
  list: [...keys.errands.all, "list"] as const,
  files: (id: number) => [...keys.errands.all, "files", id] as const,
  file: (id: number, path: string) => [...keys.errands.all, "file", id, path] as const,
  notebook: (id: number) => [...keys.errands.all, "notebook", id] as const,
  rules: (id: number) => [...keys.errands.all, "rules", id] as const,
} as const;

/** Every segment percent-encoded, the slashes between them left alone — the wildcard route splits on them. */
function encodePath(path: string): string {
  return path
    .split("/")
    .map((segment) => encodeURIComponent(segment))
    .join("/");
}

/* ------------------------------------------------------------------ reads -- */

/**
 * The list — `GET /errands`, newest first, closed ones included (unlike
 * chats, which hide what was archived). Polled at `POLL.queue` — design
 * §6.8's cadence for this list, and the only place an errand's own fields
 * (status, brain, the investigation) are ever read from.
 */
export function useErrands() {
  return useQuery({
    queryKey: ERRAND_KEYS.list,
    queryFn: () => apiFetch<Errand[]>("/errands"),
    refetchInterval: POLL.queue,
    placeholderData: keepPreviousData,
  });
}

/** The names inside one errand's folder — flat, not a tree (see the packet's follow-up). Read on open. */
export function useErrandFiles(id: number) {
  return useQuery({
    queryKey: ERRAND_KEYS.files(id),
    queryFn: () => apiFetch<string[]>(`/errands/${id}/files`),
  });
}

/** One file's contents, read on open — not polled, the same as `Projects.tsx`'s file view. */
export function useErrandFile(id: number, path: string | null) {
  return useQuery({
    queryKey: ERRAND_KEYS.file(id, path ?? ""),
    queryFn: () => apiFetch<{ contents: string }>(`/errands/${id}/files/${encodePath(path ?? "")}`),
    select: (body: { contents: string }) => body.contents,
    enabled: path !== null,
  });
}

/**
 * `caderno.md`, read on open. A notebook never written answers `200` with
 * `""`, not `404` — so an empty string here is a real read, not a missing
 * one, and the page must not draw it as an error.
 */
export function useErrandNotebook(id: number) {
  return useQuery({
    queryKey: ERRAND_KEYS.notebook(id),
    queryFn: () => apiFetch<{ contents: string }>(`/errands/${id}/notebook`),
    select: (body: { contents: string }) => body.contents,
  });
}

/** This errand's standing instructions, by name — read on open, not polled. */
export function useErrandRules(id: number) {
  return useQuery({
    queryKey: ERRAND_KEYS.rules(id),
    queryFn: () => apiFetch<ErrandRule[]>(`/errands/${id}/rules`),
  });
}

/* --------------------------------------------------------------- writes -- */

export interface PatchErrandInput {
  id: number;
  status?: ErrandStatus;
  brain?: Brain;
  done_when?: string;
  windows?: number;
}

/**
 * Pause, resume, change brain, or set an investigation — the one PATCH
 * route.
 *
 * `done_when` and `windows` travel together and only when at least one is
 * sent: a call that only carries `status` never touches the investigation,
 * which is what keeps pausing an errand from silently cancelling one.
 * Callers build the smallest object that says what they mean — an object
 * literal with only the keys it needs — and `JSON.stringify` drops the rest.
 */
export function usePatchErrand() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: ({ id, ...body }: PatchErrandInput) =>
      apiFetch<void>(`/errands/${id}`, { method: "PATCH", body: JSON.stringify(body) }),
    retry: false,
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: ERRAND_KEYS.list });
    },
  });
}

/**
 * Closes an errand. Deletes nothing: the asking stops, the row stays, and
 * the folder keeps what was found.
 */
export function useCloseErrand() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (id: number) => apiFetch<void>(`/errands/${id}`, { method: "DELETE" }),
    retry: false,
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: ERRAND_KEYS.list });
    },
  });
}

export interface WriteErrandFileInput {
  id: number;
  path: string;
  contents: string;
}

/** PUTs a file into this errand's folder. */
export function useWriteErrandFile() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: ({ id, path, contents }: WriteErrandFileInput) =>
      apiFetch<void>(`/errands/${id}/files/${encodePath(path)}`, {
        method: "PUT",
        body: JSON.stringify({ contents }),
      }),
    retry: false,
    onSuccess: (_data, { id, path }) => {
      void queryClient.invalidateQueries({ queryKey: ERRAND_KEYS.file(id, path) });
      void queryClient.invalidateQueries({ queryKey: ERRAND_KEYS.files(id) });
    },
  });
}

export interface CreateErrandRuleInput {
  id: number;
  name: string;
  cron: string;
  prompt: string;
  timezone?: string;
}

/**
 * Arms a standing instruction. `200 { rule_id }` on success; `400` carries
 * `{"error": "<reason>"}` naming the word that was wrong, which `client.ts`
 * lifts into the refusal's `detail` — `Errands.tsx` shows it verbatim rather
 * than replacing it with generic copy.
 */
export function useCreateErrandRule() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: ({ id, ...body }: CreateErrandRuleInput) =>
      apiFetch<{ rule_id: number }>(`/errands/${id}/rules`, {
        method: "POST",
        body: JSON.stringify(body),
      }),
    retry: false,
    onSuccess: (_data, { id }) => {
      void queryClient.invalidateQueries({ queryKey: ERRAND_KEYS.rules(id) });
    },
  });
}

/**
 * Disarms one rule. There is no update route — a rule already armed is
 * deleted and recreated, never edited in place, and the page offers no
 * control that would pretend otherwise.
 */
export function useDeleteErrandRule() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: ({ id, ruleId }: { id: number; ruleId: number }) =>
      apiFetch<void>(`/errands/${id}/rules/${ruleId}`, { method: "DELETE" }),
    retry: false,
    onSuccess: (_data, { id }) => {
      void queryClient.invalidateQueries({ queryKey: ERRAND_KEYS.rules(id) });
    },
  });
}
