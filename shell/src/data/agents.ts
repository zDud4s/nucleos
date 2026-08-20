import { keepPreviousData, useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { apiFetch } from "./client";
import { keys } from "./keys";

/**
 * The agent catalogue: list, create, edit and delete.
 *
 * Every shape below was read off `core/src/agent.rs` and the routes
 * `core/src/http.rs` mounts under `/agents`, field for field.
 *
 * **There is no provenance field.** The design's "provenance when hired by
 * recruitment" needs `/team-recruits`, which does not exist — verified: no
 * team route is mounted (`core/src/http.rs`). `Agents.tsx` says the catalogue
 * records no recruitment link yet, and nothing more.
 *
 * **This page does not poll.** Design §6.10: read on open. A catalogue of
 * named agents changes when a person edits it, at no other time.
 */

/** What an agent may declare for `engine`. `unrestricted` policy is refused for every engine — see below. */
export type AgentEngine = "claude" | "codex" | "local";

/** What an agent may declare for `tool_policy`. `unrestricted` is refused by the daemon and never offered. */
export type AgentToolPolicy = "mcp_only" | "none";

/** One row of the catalogue — `agent::Agent`. */
export interface Agent {
  id: string;
  name: string;
  /** One line. What a director reads to decide who gets the work. */
  speciality: string;
  prompt: string;
  engine: string;
  /** `null` reads as the engine's default, never as a blank. */
  model: string | null;
  tool_policy: string;
  created_at: string;
  updated_at: string;
}

/** What `POST /agents` and `PUT /agents/{id}` take — `agent::AgentRequest`. */
export interface AgentRequest {
  name: string;
  speciality: string;
  prompt: string;
  engine: string;
  model: string | null;
  tool_policy: string;
}

/* ------------------------------------------------------------------ keys -- */

/**
 * `keys.agents` landed as a minimal `{ all }` root — this packet's roster of
 * allowed files does not include `data/keys.ts`, so the list key is built
 * here, under that root, the same way `COUNCIL_KEYS` extends `keys.council.all`
 * in `data/council.ts`.
 */
const AGENT_KEYS = {
  list: [...keys.agents.all, "list"] as const,
} as const;

/* ------------------------------------------------------------------ reads -- */

/** The catalogue — `GET /agents`, alphabetical by name. Never polled; see the module header. */
export function useAgents() {
  return useQuery({
    queryKey: AGENT_KEYS.list,
    queryFn: () => apiFetch<Agent[]>("/agents"),
    placeholderData: keepPreviousData,
  });
}

/* --------------------------------------------------------------- writes -- */

/**
 * Hire an agent. `409` means the name (or the id it slugs to) is already
 * taken — the one refusal `Agents.tsx` names on this route.
 */
export function useCreateAgent() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (request: AgentRequest) =>
      apiFetch<Agent>("/agents", { method: "POST", body: JSON.stringify(request) }),
    retry: false,
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: AGENT_KEYS.list });
    },
  });
}

/**
 * Rewrite an agent. `PUT` replaces every field — there is no `PATCH` on this
 * route — so the caller must send the whole `AgentRequest`, unchanged fields
 * included.
 */
export function useUpdateAgent() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: ({ id, request }: { id: string; request: AgentRequest }) =>
      apiFetch<Agent>(`/agents/${encodeURIComponent(id)}`, {
        method: "PUT",
        body: JSON.stringify(request),
      }),
    retry: false,
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: AGENT_KEYS.list });
    },
  });
}

/**
 * Delete an agent. `409` means a team is standing on it — a director or a
 * member — and that is the only cause `delete` in `core/src/agent.rs` has: a
 * council roster does not hold an agent back.
 */
export function useDeleteAgent() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (id: string) => apiFetch<void>(`/agents/${encodeURIComponent(id)}`, { method: "DELETE" }),
    retry: false,
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: AGENT_KEYS.list });
    },
  });
}
