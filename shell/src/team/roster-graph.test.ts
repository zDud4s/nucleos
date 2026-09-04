import { describe, expect, it } from "vitest";

import { buildRoster } from "./roster-graph";
import type { Agent } from "../data/agents";
import type { TeamItem, TeamRunView, TeamView } from "../data/teams";

/**
 * Table tests over the four shapes a real department takes, written before the tab exists.
 *
 * The case that earns most of them is the director who also works: Finanças' `controller` both
 * directs and holds an item, and a model that quietly drops either fact draws a picture that is
 * wrong in a way nobody looking at it can see.
 *
 * Nothing here imports the preview's fixtures. A test that read `preview/daemon.ts` would be a
 * test that fails when somebody edits a screenshot.
 */

function team(overrides: Partial<TeamView> = {}): TeamView {
  return {
    id: "financas",
    name: "Finanças",
    mission: "keep the books straight",
    director_agent_id: "controller",
    max_rounds: 4,
    max_parallel: 2,
    budget_usd: 5,
    max_open_actions: 5,
    max_live_runs: 1,
    created_at: "2026-08-01T09:00:00Z",
    updated_at: "2026-09-01T09:00:00Z",
    members: ["controller", "auditor", "researcher"],
    grants: [],
    ...overrides,
  };
}

function agent(id: string, name: string, overrides: Partial<Agent> = {}): Agent {
  return {
    id,
    name,
    speciality: "does the thing",
    prompt: "You do the thing.",
    engine: "claude",
    model: null,
    tool_policy: "mcp_only",
    created_at: "2026-08-01T09:00:00Z",
    updated_at: "2026-08-01T09:00:00Z",
    ...overrides,
  };
}

const CATALOGUE = [
  agent("controller", "controller", { speciality: "runs the books" }),
  agent("auditor", "Auditor Sénior", { speciality: "checks the books" }),
  agent("researcher", "researcher", { speciality: "goes and finds it" }),
];

function item(overrides: Partial<TeamItem> = {}): TeamItem {
  return {
    ordinal: 1,
    round: 1,
    agent_id: "auditor",
    description: "pull the bank export",
    state: "done",
    run_id: 11,
    output_path: null,
    ...overrides,
  };
}

function run(overrides: Partial<TeamRunView> = {}): TeamRunView {
  return {
    id: "run-live-1",
    team_id: "financas",
    request: "Chase the invoices",
    workspace: "teams/financas/run-live-1",
    state: "working",
    director_node: "replanning",
    director_run_id: 10,
    round: 2,
    next_ordinal: 5,
    dry_rounds: 0,
    plan_retries: 0,
    replanned: "",
    outcome: null,
    why: null,
    created_at: "2026-09-04T09:00:00Z",
    updated_at: "2026-09-04T09:20:00Z",
    finished_at: null,
    trigger_id: null,
    parent_id: null,
    root_id: "run-live-1",
    depth: 0,
    cost_usd: 1.24,
    items: [item()],
    ...overrides,
  };
}

describe("buildRoster", () => {
  it("puts the director on layer 0 and every other member on layer 1", () => {
    const model = buildRoster({ team: team(), agents: CATALOGUE, teams: [team()], runs: [] });

    expect(model.nodes.filter((node) => node.layer === 0).map((node) => node.label)).toEqual([
      "controller",
    ]);
    expect(model.nodes.filter((node) => node.layer === 1).map((node) => node.label)).toEqual([
      "Auditor Sénior",
      "researcher",
    ]);
  });

  it("says what the director is doing rather than only that it directs", () => {
    const model = buildRoster({
      team: team(),
      agents: CATALOGUE,
      teams: [team()],
      runs: [run({ director_node: "replanning" })],
    });

    const director = model.nodes.find((node) => node.layer === 0)!;
    expect(director.state).toBe("directs");
    expect(director.said).toBe("replanning");
  });

  it("hangs an item under the specialist who holds it", () => {
    const model = buildRoster({ team: team(), agents: CATALOGUE, teams: [team()], runs: [run()] });
    const work = model.nodes.filter((node) => node.layer === 2);

    expect(work).toHaveLength(1);
    expect(work[0].parent).toBe("member:auditor");
    expect(work[0].label).toBe("pull the bank export");
    expect(work[0].state).toBe("done");
    expect(work[0].said).toBe("round 1 · Chase the invoices");
    expect(work[0].href).toBe("/team-runs/run-live-1");
    expect(work[0].crosses).toBe(false);
  });

  it("keeps a director's own item on layer 2 and marks it as crossing", () => {
    // Finanças as it really is: `controller` directs AND holds round 2's item.
    const model = buildRoster({
      team: team(),
      agents: CATALOGUE,
      teams: [team()],
      runs: [
        run({
          items: [
            item(),
            item({
              ordinal: 3,
              round: 2,
              agent_id: "controller",
              description: "match them line by line",
              state: "working",
              run_id: 13,
            }),
          ],
        }),
      ],
    });

    const own = model.nodes.find((node) => node.label === "match them line by line")!;
    expect(own.layer).toBe(2);
    expect(own.parent).toBe("director:controller");
    expect(own.crosses).toBe(true);
  });

  it("marks a member whose agent no longer exists", () => {
    const model = buildRoster({
      team: team({ members: ["controller", "ghost"] }),
      agents: CATALOGUE,
      teams: [team()],
      runs: [],
    });

    const ghost = model.nodes.find((node) => node.id === "member:ghost")!;
    expect(ghost.missing).toBe(true);
    expect(ghost.label).toBe("ghost");
    expect(ghost.said).toBe("deleted from the catalogue");
  });

  it("says which other departments a specialist serves", () => {
    const marketing = team({
      id: "marketing",
      name: "Marketing",
      director_agent_id: "editor",
      members: ["editor", "researcher"],
    });
    const model = buildRoster({
      team: team(),
      agents: CATALOGUE,
      teams: [team(), marketing],
      runs: [],
    });

    expect(model.nodes.find((node) => node.id === "member:researcher")!.said).toBe(
      "also in Marketing",
    );
    expect(model.nodes.find((node) => node.id === "member:auditor")!.said).toBe("checks the books");
  });

  it("gives the same order whatever order the members and runs arrive in", () => {
    const forwards = buildRoster({
      team: team({ members: ["controller", "auditor", "researcher"] }),
      agents: CATALOGUE,
      teams: [team()],
      runs: [
        run({ id: "run-a" }),
        run({
          id: "run-b",
          items: [item({ agent_id: "researcher", description: "pull the ledger", run_id: 12 })],
        }),
      ],
    });
    const backwards = buildRoster({
      team: team({ members: ["researcher", "auditor", "controller"] }),
      agents: CATALOGUE,
      teams: [team()],
      runs: [
        run({
          id: "run-b",
          items: [item({ agent_id: "researcher", description: "pull the ledger", run_id: 12 })],
        }),
        run({ id: "run-a" }),
      ],
    });

    expect(backwards.nodes.map((node) => node.id)).toEqual(forwards.nodes.map((node) => node.id));
  });

  it("draws two layers and no third when nothing is in flight", () => {
    const model = buildRoster({ team: team(), agents: CATALOGUE, teams: [team()], runs: [] });

    expect(model.nodes.some((node) => node.layer === 2)).toBe(false);
  });
});
