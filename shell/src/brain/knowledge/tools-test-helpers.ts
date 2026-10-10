import type { LoadoutTool } from "../../data/loadout-tools";

export function tool(over: Partial<LoadoutTool> = {}): LoadoutTool {
  return {
    id: 1,
    owner_kind: "agent",
    owner_id: "scout",
    tool: "web_read",
    status: "active",
    source: "request",
    reason: "I need to read the vendor page",
    run_id: "900001",
    created_at: "2026-10-08T09:00:00+00:00",
    decided_at: "2026-10-08T09:05:00+00:00",
    ...over,
  };
}

/**
 * A daemon holding these rows: `GET /loadout/tools` narrowed by the query the
 * way the núcleo narrows it, `/teams` answering the given ids, anything else
 * undefined.
 */
export function toolsDaemon(rows: LoadoutTool[], teams: string[] = []) {
  return (path: string) => {
    if (path === "/teams") return Promise.resolve(teams.map((id) => ({ id, name: id })));
    if (!path.startsWith("/loadout/tools?")) return Promise.resolve(undefined);
    const query = new URLSearchParams(path.slice("/loadout/tools?".length));
    return Promise.resolve(
      rows.filter(
        (row) =>
          (query.get("status") === null || row.status === query.get("status")) &&
          (query.get("owner_kind") === null || row.owner_kind === query.get("owner_kind")) &&
          (query.get("owner_id") === null || row.owner_id === query.get("owner_id")),
      ),
    );
  };
}
