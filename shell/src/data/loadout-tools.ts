import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { apiFetch } from "./client";
import { keys } from "./keys";
import { POLL } from "./poll";

/**
 * The tools an agent or a team may call beyond its box's base, as hooks.
 *
 * Four routes, read off `core/src/http.rs` and `core/src/tool_loadout.rs`:
 *
 * | what                    | route                              |
 * |-------------------------|------------------------------------|
 * | the rows, any status    | `GET /loadout/tools`               |
 * | let a proposed one in   | `POST /loadout/tools/{id}/approve` |
 * | refuse a proposed one   | `POST /loadout/tools/{id}/reject`  |
 * | take an active one back | `POST /loadout/tools/{id}/revoke`  |
 *
 * A request arrives from a run (`POST /loadout/tool-requests`) and is never
 * made from the shell. The route is owner-only.
 */

export type LoadoutToolStatus = "proposed" | "active" | "rejected" | "revoked";

/** One row of `loadout_tools` — `tool_loadout::ToolRow`. */
export interface LoadoutTool {
  id: number;
  owner_kind: "agent" | "team";
  owner_id: string;
  /** An MCP tool of the núcleo, by name. Never a built-in. */
  tool: string;
  status: LoadoutToolStatus;
  /** `owner` is a grant made here; `request` is a run asking for it. */
  source: "owner" | "request";
  /** What the run said. Untrusted text: shown, never acted on. */
  reason: string | null;
  run_id: string | null;
  created_at: string;
  decided_at: string | null;
}

/** Where an approval lands: the row's own owner (`{}`), or a team by id. */
export type ApproveTarget = { team: string } | null;

/**
 * Rows in one status, optionally those of one owner. Polled at the queue's
 * pace: a request lands while somebody is looking at the Brain.
 */
export function useLoadoutTools(
  status: LoadoutToolStatus,
  owner?: { kind: "agent" | "team"; id: string },
) {
  const params = new URLSearchParams({ status });
  if (owner !== undefined) {
    params.set("owner_kind", owner.kind);
    params.set("owner_id", owner.id);
  }
  return useQuery({
    queryKey: keys.loadoutTools.list(status, owner?.kind ?? null, owner?.id ?? null),
    queryFn: () => apiFetch<LoadoutTool[]>(`/loadout/tools?${params.toString()}`),
    refetchInterval: POLL.queue,
  });
}

function useToolDecision<Input>(send: (input: Input) => Promise<LoadoutTool>) {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: send,
    // A decision is settled; a retried approval is a second attempt at granting a tool.
    retry: false,
    // `onSettled`: a 409 means it was decided elsewhere, and the list is stale either way.
    onSettled: () => {
      void queryClient.invalidateQueries({ queryKey: keys.loadoutTools.all });
    },
  });
}

/** Yes — to the row's own owner, or re-targeted to a team (`owner: "team"` + `team_id`). */
export function useApproveLoadoutTool() {
  return useToolDecision(({ id, target }: { id: number; target: ApproveTarget }) =>
    apiFetch<LoadoutTool>(`/loadout/tools/${id}/approve`, {
      method: "POST",
      body: JSON.stringify(target === null ? {} : { owner: "team", team_id: target.team }),
    }),
  );
}

/** No — kept as a refusal; the run may ask again. */
export function useRejectLoadoutTool() {
  return useToolDecision((id: number) =>
    apiFetch<LoadoutTool>(`/loadout/tools/${id}/reject`, {
      method: "POST",
      body: JSON.stringify({}),
    }),
  );
}

/** Take an active tool back. 409 when it is not active. */
export function useRevokeLoadoutTool() {
  return useToolDecision((id: number) =>
    apiFetch<LoadoutTool>(`/loadout/tools/${id}/revoke`, {
      method: "POST",
      body: JSON.stringify({}),
    }),
  );
}
