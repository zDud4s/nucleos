import { ApiRefusal } from "../data/client";
import type { Agent, Employer } from "../data/agents";
import { knowledgeAnswer } from "../brain/knowledge/test-helpers";
import type { Known } from "../data/knowledge";

export function agent(overrides: Partial<Agent> = {}): Agent {
  return {
    id: "copywriter",
    name: "copywriter",
    speciality: "writes short copy",
    prompt: "You write short, punchy copy.",
    engine: "claude",
    model: null,
    capabilities: ["mcp.nucleos"],
    tool_policy: "mcp_only",
    created_at: "2026-08-01T09:00:00Z",
    updated_at: "2026-08-01T09:00:00Z",
    ...overrides,
  };
}

export function team(overrides: Partial<Employer> = {}): Employer {
  return { id: "financas", name: "Finanças", director_agent_id: "", members: [], ...overrides };
}

/**
 * The agent routes, over mutable state — the same shape `councilFetch` in
 * `Council.test.tsx` uses: a page that
 * refetches after a mutation needs the next `GET` to answer with the changed
 * row, not with whatever a one-shot mock happened to return first.
 *
 * `/teams` is answered here too, because the page reads it for one derived
 * number per row. An unanswered query would leave `data` undefined, which the
 * page draws as "not known" — a state worth testing on purpose and never worth
 * falling into by accident.
 */
export function agentsFetch(
  rows: Agent[],
  opts: {
    teams?: Employer[];
    knowledge?: Known[];
    onCreate?: (body: Record<string, unknown>) => unknown;
    onUpdate?: (id: string, body: Record<string, unknown>) => unknown;
    onDelete?: (id: string) => unknown;
  } = {},
): (path: string, init?: RequestInit) => Promise<unknown> {
  /*
    Every read answers with FRESH objects, copied off the state this closure
    holds. Returning the array itself is what a hand-written fake reaches for
    first and it is quietly wrong: react-query keeps the previously stored value
    when the next one is reference-equal to it, so a fake that mutates a row in
    place and hands the same array back reports no change at all, and the page
    under test renders stale text that the store already disagrees with. A real
    daemon parses new objects out of JSON every time; so does this.
  */
  return async (path, init) => {
    if (path === "/teams") return (opts.teams ?? []).map((row) => ({ ...row }));
    const listed = knowledgeAnswer(path, opts.knowledge ?? []);
    if (listed !== undefined) return listed.map((row) => ({ ...row }));

    if (path === "/agents" && init?.method === "POST") {
      const body = JSON.parse(init.body as string) as Record<string, unknown>;
      if (opts.onCreate !== undefined) return opts.onCreate(body);
      const created: Agent = {
        id: String(body.name),
        name: String(body.name),
        speciality: String(body.speciality),
        prompt: String(body.prompt),
        engine: String(body.engine),
        model: (body.model as string | null) ?? null,
        capabilities: body.capabilities as string[],
        tool_policy: "none",
        created_at: "2026-08-18T09:00:00Z",
        updated_at: "2026-08-18T09:00:00Z",
      };
      rows.push(created);
      return created;
    }
    if (path === "/agents") return rows.map((row) => ({ ...row }));

    const idMatch = /^\/agents\/([^/]+)$/.exec(path);
    if (idMatch !== null && init?.method === "PUT") {
      const id = decodeURIComponent(idMatch[1]);
      const body = JSON.parse(init.body as string) as Record<string, unknown>;
      if (opts.onUpdate !== undefined) return opts.onUpdate(id, body);
      const row = rows.find((candidate) => candidate.id === id);
      if (row === undefined) throw new ApiRefusal(404, "not_found", "");
      row.name = String(body.name);
      row.speciality = String(body.speciality);
      row.prompt = String(body.prompt);
      row.engine = String(body.engine);
      row.model = (body.model as string | null) ?? null;
      row.capabilities = body.capabilities as string[];
      return row;
    }
    if (idMatch !== null && init?.method === "DELETE") {
      const id = decodeURIComponent(idMatch[1]);
      if (opts.onDelete !== undefined) return opts.onDelete(id);
      const index = rows.findIndex((candidate) => candidate.id === id);
      if (index === -1) throw new ApiRefusal(404, "not_found", "");
      rows.splice(index, 1);
      return undefined;
    }
    return undefined;
  };
}

/** The dwell `ConfirmButton` needs between arming and confirming — see `KillSwitchControl.test.tsx`. */
export async function afterDwell(): Promise<void> {
  await new Promise((resolve) => setTimeout(resolve, 350));
}

