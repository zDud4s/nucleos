import { keepPreviousData, useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { apiFetch } from "./client";
import { keys } from "./keys";
import { POLL } from "./poll";
import { useSidecars, useSystemHealth } from "./system";
import type { SidecarState, SubsystemReadout } from "./system";

export type { SidecarState, SubsystemReadout } from "./system";

/**
 * The Browser pillar: the sessions in flight, the wheel handover's own
 * decisions (distinct from Waiting's), the sites a project has logged into,
 * and the one health reading this pillar has.
 *
 * `BrowserSession`, `WheelRequest`, `isWheelRequest` and {@link useWheelRequests}
 * moved here from `data/waiting.ts` — Browser data, not Waiting's — and are
 * re-exported there under the idiom that file already uses for
 * `useContactMerges`, so the Waiting queue's own import keeps working
 * unchanged.
 *
 * **Wheel decisions are not this page's.** Taking or refusing a requested
 * wheel is `POST /proposals/{id}/approve|reject`, decided on the Waiting
 * page — this file still reads `GET /browser/sessions` because that is also
 * where every OTHER session mode lives, but it adds no door back into that
 * decision. What this pillar owns instead is the handover's own two-step
 * question, once a person is actually driving: `POST /browser/return` gives
 * the wheel back (and closes the session in the same call), and `POST
 * /browser/keep` answers "keep these?" over the chain that came back with
 * it — a session already closed by the time that question is asked, so the
 * chain travels in the mutation's own response rather than in a further
 * read of the sessions list (`core/src/browser_wheel.rs:222-275`).
 */

/* ----------------------------------------------------------------- shapes -- */

/** One open browser session, exactly as `browser::SessionRow` serialises. */
export interface BrowserSession {
  id: number;
  sidecar_id: string;
  run_id: number | null;
  /** The project the session was opened FOR — not necessarily whose profile it runs in. */
  project_id: string | null;
  profile_kind: string;
  profile_id: string;
  requested_url: string;
  final_url: string;
  rule: string;
  /** Spec §4.4's state machine. `wheel-requested` is the only one Waiting decides. */
  mode: "agent" | "wheel-requested" | "human" | "delivery-failed";
  refusal: string | null;
  /** The proposal that asked for the wheel, once one exists. Null means nothing to decide yet. */
  proposal_id: number | null;
  /** The navigation chain the person's window recorded, as a JSON-encoded `string[]` — parse it. */
  chain: string | null;
  chain_decided_at: string | null;
  opened_at: string;
  closed_at: string | null;
}

/**
 * A session that is actually asking, narrowed to say so in the type.
 *
 * `proposal_id` is nullable on a session and not on one of these: a session in
 * `wheel-requested` with no proposal yet is the window between the mode flipping
 * and the record landing (spec §4.4 rule 3), and there is nothing to answer.
 */
export interface WheelRequest extends BrowserSession {
  proposal_id: number;
}

/** Whether a session is asking for a person, and has a proposal to answer with. */
export function isWheelRequest(session: BrowserSession): session is WheelRequest {
  return session.mode === "wheel-requested" && session.proposal_id !== null;
}

/** One origin a project's profile admits — `browser::Site`, `GET /browser/sites/{project_id}`. */
export interface Site {
  /** Normalised WITH port, e.g. `"https://jira.example.org:443"`. Shown literally — punycode never prettified. */
  origin: string;
  kind: "destination" | "idp";
  granted_at: string;
  /** The destination whose login brought an idp in. `null` when this row IS the destination. */
  granted_for: string | null;
}

/**
 * What this page shows for browser health: the one subsystem the daemon
 * measures, plus — only when it is not `ok` — the sidecar's own prose. Never
 * three states: `health.rs:160-169` says outright that the Chromium download
 * belongs to the sidecar (which reports it by refusing to start with a
 * message naming the path), and no route exposes progress on it.
 */
export interface BrowserHealth {
  subsystem: SubsystemReadout | null;
  sidecar: SidecarState | null;
}

/* ------------------------------------------------------------------ reads -- */

/**
 * Every open session, whatever mode it is in.
 *
 * Same route and same cache entry as {@link useWheelRequests} — `select` is
 * what narrows one to the other, not a second query. Oldest first, admin
 * scope.
 */
export function useBrowserSessions() {
  return useQuery({
    queryKey: keys.browser.sessions,
    queryFn: () => apiFetch<BrowserSession[]>("/browser/sessions"),
    refetchInterval: POLL.queue,
    placeholderData: keepPreviousData,
  });
}

/**
 * The sessions where an agent has asked for the wheel — moved from
 * `data/waiting.ts` verbatim; Waiting re-exports this name.
 */
export function useWheelRequests() {
  return useQuery({
    queryKey: keys.browser.sessions,
    queryFn: () => apiFetch<BrowserSession[]>("/browser/sessions"),
    refetchInterval: POLL.queue,
    placeholderData: keepPreviousData,
    select: (sessions: BrowserSession[]) => sessions.filter(isWheelRequest),
  });
}

/** Where a project has logged in. Admin scope, like every route in this file. */
export function useBrowserSites(projectId: string | undefined) {
  return useQuery({
    queryKey: keys.browser.sites(projectId ?? ""),
    queryFn: () => apiFetch<Site[]>(`/browser/sites/${encodeURIComponent(projectId ?? "")}`),
    enabled: projectId !== undefined,
    refetchInterval: POLL.queue,
    placeholderData: keepPreviousData,
  });
}

/**
 * The one browser reading this shell can make honestly.
 *
 * Reads the SAME cache entries every other pillar's health does —
 * `useSystemHealth()` narrowed to `browser_sidecar`, `useSidecars()` narrowed
 * to `name === "browser"` — a different literal, because the health probe's
 * own label and the supervisor's registry key are not the same string
 * (`crate::sidecar::BROWSER == "browser"`). `sidecar` stays `null` while the
 * subsystem is `ok`: the sidecar's own prose has nothing to add to a
 * subsystem that is not failing. This declares no query of its own — it only
 * narrows the two hooks in `data/system.ts`.
 */
export function useBrowserHealth(): { data: BrowserHealth | undefined; isError: boolean; error: unknown } {
  const health = useSystemHealth();
  const sidecars = useSidecars();

  const subsystem = health.data?.subsystems.find((row) => row.name === "browser_sidecar") ?? null;
  const sidecar =
    subsystem !== null && subsystem.status !== "ok"
      ? (sidecars.data?.find((row) => row.name === "browser") ?? null)
      : null;

  return {
    data: health.data === undefined ? undefined : { subsystem, sidecar },
    isError: health.isError,
    error: health.error,
  };
}

/* -------------------------------------------------------------- decisions -- */

/** Everything this pillar's writes can move, invalidated together. */
function browserKeys() {
  return [keys.browser.all];
}

/** Close a session outright — the only door that clears a `delivery-failed` row, which nothing else does. */
export function useCloseSession() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (sessionId: number) =>
      apiFetch<void>("/browser/close", { method: "POST", body: JSON.stringify({ session_id: sessionId }) }),
    retry: false,
    onSettled: () => {
      for (const key of browserKeys()) void queryClient.invalidateQueries({ queryKey: key });
    },
  });
}

/** What `POST /browser/return` hands back — raw navigation urls, candidates and not grants. */
export interface ReturnedChain {
  chain: string[];
}

/**
 * Give the wheel back.
 *
 * Also closes the session, in the same daemon call — so the session this
 * mutation was called for is already gone from `GET /browser/sessions` by
 * the time it settles. The chain that comes back is what the caller must
 * hold onto to ask the next question with {@link useKeepChain}.
 */
export function useReturnWheel() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (sessionId: number) =>
      apiFetch<ReturnedChain>("/browser/return", {
        method: "POST",
        body: JSON.stringify({ session_id: sessionId }),
      }),
    retry: false,
    onSettled: () => {
      for (const key of browserKeys()) void queryClient.invalidateQueries({ queryKey: key });
    },
  });
}

export interface KeepChainInput {
  sessionId: number;
  keep: boolean;
}

/** What `POST /browser/keep` hands back — the origins actually granted (empty when `keep` was false). */
export interface KeptGrants {
  granted: string[];
}

/**
 * Answer "keep these?" — the only way `browser_sites` ever grows.
 *
 * A single session id and one boolean, never a list of hosts: the dialogue
 * is Keep them or Keep none, the whole set or nothing. Answerable exactly
 * once — a second call answers 409.
 */
export function useKeepChain() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (input: KeepChainInput) =>
      apiFetch<KeptGrants>("/browser/keep", {
        method: "POST",
        body: JSON.stringify({ session_id: input.sessionId, keep: input.keep }),
      }),
    retry: false,
    onSettled: () => {
      for (const key of browserKeys()) void queryClient.invalidateQueries({ queryKey: key });
    },
  });
}

export interface RevokeSiteInput {
  projectId: string;
  origin: string;
}

/** Withdraw one origin's standing login. Takes effect immediately — no second decision needed. */
export function useRevokeSite() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (input: RevokeSiteInput) =>
      apiFetch<void>("/browser/revoke", {
        method: "POST",
        body: JSON.stringify({ project_id: input.projectId, origin: input.origin }),
      }),
    retry: false,
    onSettled: (_data, _error, input) => {
      void queryClient.invalidateQueries({ queryKey: keys.browser.sites(input.projectId) });
    },
  });
}

/** What `POST /browser/forget` hands back — how many running sessions it stopped along the way. */
export interface ForgetOutcome {
  stopped: number;
}

/**
 * Forget a project's whole browser identity — every site, the profile, and
 * any session still running in it. Destructive and irreversible; the page
 * gates this behind a `ConfirmButton`.
 */
export function useForgetProfile() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (projectId: string) =>
      apiFetch<ForgetOutcome>("/browser/forget", {
        method: "POST",
        body: JSON.stringify({ project_id: projectId }),
      }),
    retry: false,
    onSettled: (_data, _error, projectId) => {
      void queryClient.invalidateQueries({ queryKey: keys.browser.sites(projectId) });
      void queryClient.invalidateQueries({ queryKey: keys.browser.sessions });
    },
  });
}

/** What a person hands over to open a window of their own. */
export interface OpenWindowInput {
  projectId: string;
  url: string;
}

/**
 * A person opens a window on a project's profile, from an address they typed.
 *
 * No proposal and no confirmation: the §4.4 dialogue defends against an AGENT choosing a
 * destination while carrying a stranger's words, and here the person typed it. The daemon
 * refuses with 409 both when the pillar is off and when nobody is at the machine, telling
 * the two apart only in its prose — which is why the page renders the daemon's own
 * sentence rather than one of its own.
 *
 * `onSettled` rather than `onSuccess`, and that is load-bearing: a launch that fails still
 * leaves a `delivery-failed` row behind, so the sessions list has moved either way.
 */
export function useOpenWindow() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (input: OpenWindowInput) =>
      apiFetch<BrowserSession>("/browser/window", {
        method: "POST",
        body: JSON.stringify({ project_id: input.projectId, url: input.url }),
      }),
    retry: false,
    onSettled: () => {
      for (const key of browserKeys()) void queryClient.invalidateQueries({ queryKey: key });
    },
  });
}
