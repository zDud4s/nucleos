import { keepPreviousData, useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { apiFetch } from "./client";
import { keys } from "./keys";

/**
 * The agent catalogue: list, create, edit and delete.
 *
 * Every shape below was read off `core/src/agent.rs` and the routes
 * `core/src/http.rs` mounts under `/agents`, field for field.
 *
 * **There is no provenance field.** The `agents` table has ten columns
 * (`core/migrations/0071_teams.sql`) and none of them records a recruitment,
 * and every proposal route is pending-only — so a link that was decided cannot
 * be recovered afterwards either. `Agents.tsx` says the catalogue records no
 * recruitment link, and nothing more.
 *
 * *(Corrected 2026-08-25: this said the design's provenance "needs
 * `/team-recruits`, which does not exist — verified: no team route is
 * mounted". Every team route is mounted now, `/team-recruits` among them
 * (`core/src/http.rs:221`), so anybody rechecking the claim found the route and
 * had no way to tell whether the conclusion had survived with it. It has, for a
 * different reason: the column, not the route.)*
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

/* -------------------------------------------------------------- readings -- */

/**
 * PURE: the id a name earns — the mirror of `agent::slug` (`core/src/agent.rs:129`).
 *
 * A second copy of a núcleo function, which is normally the thing this codebase
 * refuses. It is here because the id is computed ONCE, at creation, and `update`
 * never recomputes it — by design, since `team_members`, `teams.director_agent_id`,
 * `team_items.agent_id`, `job_items.agent_id` and `council_seats.agent_id` all
 * point at it, and a reference that moved when somebody edited a label would
 * break in silence. The consequence is that after a rename the name and the id
 * DISAGREE, permanently, and no route answers "do these still match" — the only
 * way to know is to recompute the slug and compare.
 *
 * Faithful to the Rust character for character: ASCII alphanumerics lowercased,
 * every other run collapsed to ONE dash, no leading dash, trailing dash cut.
 * `trim()` differs slightly between the two (Rust trims Unicode whitespace, JS
 * also trims line terminators) and it cannot change the answer: leading
 * whitespace never emits a dash because the output is still empty, and trailing
 * whitespace can only emit the dash that the last step strips.
 */
export function slugOf(name: string): string {
  let out = "";
  let lastWasDash = false;
  for (const character of name.trim()) {
    if (/^[A-Za-z0-9]$/.test(character)) {
      out += character.toLowerCase();
      lastWasDash = false;
    } else if (!lastWasDash && out !== "") {
      out += "-";
      lastWasDash = true;
    }
  }
  return out.replace(/-+$/, "");
}

/**
 * PURE: whether the machine knows this agent by a different word than the person does.
 *
 * True after any rename. Not an error and not a state — it is a fact the rest of
 * the app depends on, because everything else addresses an agent by its id and
 * only this page holds both halves.
 */
export function renamed(agent: Agent): boolean {
  return slugOf(agent.name) !== agent.id;
}

/**
 * PURE: whether this agent could ever take a council seat.
 *
 * Turns on the model alone. `council::seat_from_spec` refuses an agent whose
 * engine no seat can host AND one that names no model — but `ENGINE_SEAT_KINDS`
 * is total over `ENGINES`, enforced by a test that walks it in both directions
 * (`core/src/config.rs:2058`), so the engine arm cannot fire for anything this
 * catalogue is allowed to hold. What is left is `core/src/council.rs:841`: *"a
 * seat records the model it ran"*, and a null model is exactly a disqualified
 * agent.
 *
 * A null model is still perfectly good for a department. This is why the page
 * says what the null costs rather than drawing it as a fault.
 */
export function canTakeASeat(agent: Agent): boolean {
  return agent.model !== null;
}

/** PURE: whether this agent has any tools at all — the whole of what `tool_policy` decides. */
export function hasTools(agent: Agent): boolean {
  return agent.tool_policy === "mcp_only";
}

/**
 * The only fields of a department this file reads.
 *
 * Structural rather than an `import type { TeamView }`, for two reasons. The
 * blunt one is that `data/teams.ts` already imports `AgentRequest` from here, so
 * the type import would close a cycle — erased at build, but a cycle a reader
 * has to reason about. The real one is that a function's parameter should say
 * what the function reads: `employmentOf` looks at four fields and would not
 * notice the other eleven changing.
 */
export interface Employer {
  id: string;
  name: string;
  director_agent_id: string;
  members: string[];
}

/**
 * Where one agent is employed, by department NAME.
 *
 * Names and not ids, because this reading is read out loud — beside the delete
 * button, in the sentence that says what is standing on this agent.
 *
 * How MUCH, never WHERE: the matrix on `/teams` answers where, this answers how
 * much, and the two are not the same question. This is the one that decides
 * whether an agent can be deleted, and whether anybody is using it at all.
 *
 * `directs` and `staffs` are separate and a department can be in both: the
 * daemon stores the director apart from the roster (`core/src/team.rs`), so a
 * director is not necessarily in `members`, and a director that IS in `members`
 * is not a duplicate to be merged away.
 */
export interface Employment {
  directs: string[];
  staffs: string[];
}

export function employmentOf(agentId: string, teams: Employer[]): Employment {
  return {
    directs: teams.filter((team) => team.director_agent_id === agentId).map((team) => team.name),
    staffs: teams.filter((team) => team.members.includes(agentId)).map((team) => team.name),
  };
}

/** PURE: whether anything at all names this agent, across both kinds of standing. */
export function unemployed(employment: Employment): boolean {
  return employment.directs.length === 0 && employment.staffs.length === 0;
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
