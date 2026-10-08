// @vitest-environment node
import { describe, expect, it } from "vitest";

import { LAYER_H, buildRoster, placeRoster } from "./roster-graph";
import { MIN_W } from "../canvas/layered";
import type { Agent } from "../data/agents";
import type { TeamItem, TeamRunView, TeamView } from "../data/teams";

/**
 * Table tests over the shapes a real department takes, written before the tab exists.
 *
 * Two cases earn most of them. The first is the director who also works: Finanças' `controller`
 * both directs and holds an item, and a model that quietly drops either fact draws a picture that
 * is wrong in a way nobody looking at it can see. The second is the department with nothing
 * running, which is the case the first version of this got wrong — the structure has to be whole
 * whether or not anything is in flight.
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

/** The structure alone: a real department with a real roster and nothing running. */
const IDLE = { team: team(), agents: CATALOGUE, teams: [team()], runs: [] };

describe("buildRoster", () => {
  it("puts the department on top, its director under it and the roster under that", () => {
    const model = buildRoster(IDLE);
    const rank = (layer: number) =>
      model.nodes.filter((node) => node.layer === layer).map((node) => node.label);

    expect(rank(0)).toEqual(["Finanças"]);
    expect(rank(1)).toEqual(["controller"]);
    expect(rank(2)).toEqual(["Auditor Sénior", "researcher"]);
  });

  /**
   * The whole point of the redesign. The limits live in the Charter as form fields; a number
   * somebody can only see by opening the form that edits it is a number nobody checks.
   */
  it("says the limits the department runs under, whether or not anything is running", () => {
    const model = buildRoster(IDLE);
    const department = model.nodes.find((node) => node.layer === 0)!;

    expect(department.said).toBe("2 on the roster");
    expect(department.facts).toEqual(["4 rounds · 2 at a time", "$5.00 ceiling · 5 open actions"]);
  });

  /** `budget_usd` is null for a department with no ceiling of its own, and null is not zero. */
  it("says a department has no ceiling of its own rather than printing zero", () => {
    const model = buildRoster({ ...IDLE, team: team({ budget_usd: null }) });

    expect(model.nodes[0].facts[1]).toBe("no ceiling of its own · 5 open actions");
  });

  it("counts one on the roster in the singular", () => {
    const model = buildRoster({ ...IDLE, team: team({ members: ["controller", "auditor"] }) });

    expect(model.nodes[0].said).toBe("1 on the roster");
  });

  it("says what the director is doing rather than only that it directs", () => {
    const model = buildRoster({ ...IDLE, runs: [run({ director_node: "replanning" })] });

    const director = model.nodes.find((node) => node.layer === 1)!;
    expect(director.state).toBe("directs");
    expect(director.said).toBe("replanning");
  });

  /**
   * A department created and never staffed. The first version drew a nameless box where the
   * director should be, which reads as a rendering fault rather than as a department nobody has
   * been put in charge of.
   */
  it("draws no director rank at all when nobody has been put in charge", () => {
    const model = buildRoster({
      ...IDLE,
      team: team({ director_agent_id: "", members: ["auditor"] }),
    });

    expect(model.nodes.some((node) => node.layer === 1)).toBe(false);
    expect(model.nodes.find((node) => node.id === "member:auditor")!.parent).toBe("team:financas");
  });

  it("hangs an item under the specialist who holds it", () => {
    const model = buildRoster({ ...IDLE, runs: [run()] });
    const work = model.nodes.filter((node) => node.layer === 3);

    expect(work).toHaveLength(1);
    expect(work[0].parent).toBe("member:auditor");
    expect(work[0].label).toBe("pull the bank export");
    expect(work[0].state).toBe("done");
    expect(work[0].said).toBe("round 1 · Chase the invoices");
    expect(work[0].href).toBe("/team-runs/run-live-1");
    expect(work[0].crosses).toBe(false);
  });

  it("keeps a director's own item on the work rank and marks it as crossing", () => {
    // Finanças as it really is: `controller` directs AND holds round 2's item.
    const model = buildRoster({
      ...IDLE,
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
    expect(own.layer).toBe(3);
    expect(own.parent).toBe("director:controller");
    expect(own.crosses).toBe(true);
  });

  it("marks a member whose agent no longer exists", () => {
    const model = buildRoster({ ...IDLE, team: team({ members: ["controller", "ghost"] }) });

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
    const model = buildRoster({ ...IDLE, teams: [team(), marketing] });

    expect(model.nodes.find((node) => node.id === "member:researcher")!.said).toBe(
      "also in Marketing",
    );
    expect(model.nodes.find((node) => node.id === "member:auditor")!.said).toBe("checks the books");
  });

  it("gives the same order whatever order the members and runs arrive in", () => {
    const ledger = item({ agent_id: "researcher", description: "pull the ledger", run_id: 12 });
    const forwards = buildRoster({
      ...IDLE,
      team: team({ members: ["controller", "auditor", "researcher"] }),
      runs: [run({ id: "run-a" }), run({ id: "run-b", items: [ledger] })],
    });
    const backwards = buildRoster({
      ...IDLE,
      team: team({ members: ["researcher", "auditor", "controller"] }),
      runs: [run({ id: "run-b", items: [ledger] }), run({ id: "run-a" })],
    });

    expect(backwards.nodes.map((node) => node.id)).toEqual(forwards.nodes.map((node) => node.id));
  });

  it("draws the structure whole when nothing is in flight, and only omits the work", () => {
    const model = buildRoster(IDLE);

    expect(model.nodes.some((node) => node.layer === 3)).toBe(false);
    expect(model.nodes.map((node) => node.id)).toEqual([
      "team:financas",
      "director:controller",
      "member:auditor",
      "member:researcher",
    ]);
  });
});

/** The Finanças fixture, in full: a director who also works, two specialists, three items. */
function full() {
  const marketing = team({
    id: "marketing",
    name: "Marketing",
    director_agent_id: "editor",
    members: ["editor", "researcher"],
  });
  return buildRoster({
    ...IDLE,
    teams: [team(), marketing],
    runs: [
      run({
        items: [
          item(),
          item({ ordinal: 2, agent_id: "researcher", description: "pull the ledger", run_id: 12 }),
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
  }).nodes;
}

describe("placeRoster", () => {
  it("centres a parent over its children", () => {
    const nodes = buildRoster({
      ...IDLE,
      runs: [
        run({ items: [item(), item({ ordinal: 2, description: "reconcile the two", run_id: 12 })] }),
      ],
    }).nodes;
    const placed = placeRoster(nodes);

    const parent = placed.boxes.find((box) => box.id === "member:auditor")!;
    const kids = placed.boxes.filter((box) => box.parent === "member:auditor");
    const first = kids[0];
    const last = kids[kids.length - 1];

    expect(parent.x + parent.width / 2).toBeCloseTo((first.x + last.x + last.width) / 2, 0);
  });

  it("gives a box the width its own label needs", () => {
    const nodes = buildRoster({
      ...IDLE,
      runs: [
        run({ items: [item({ description: "reconcile every line of the August bank export" })] }),
      ],
    }).nodes;
    const placed = placeRoster(nodes);

    expect(placed.boxes.find((box) => box.id.startsWith("item:"))!.width).toBeGreaterThan(MIN_W);
  });

  /** The department box is as wide as its limits line, not as wide as its name. */
  it("gives the department the width its limits need", () => {
    const placed = placeRoster(buildRoster(IDLE).nodes);
    const department = placed.boxes.find((box) => box.id === "team:financas")!;

    expect(department.width).toBeGreaterThan("Finanças".length * 10);
  });

  it("widens a subtree rather than overlapping three items under one specialist", () => {
    const nodes = buildRoster({
      ...IDLE,
      runs: [
        run({
          items: [
            item(),
            item({ ordinal: 2, description: "reconcile the two", run_id: 12 }),
            item({ ordinal: 3, description: "write the exceptions up", run_id: 13 }),
          ],
        }),
      ],
    }).nodes;
    const kids = placeRoster(nodes).boxes.filter((box) => box.parent === "member:auditor");

    expect(kids).toHaveLength(3);
    for (let i = 1; i < kids.length; i += 1) {
      expect(kids[i].x).toBeGreaterThanOrEqual(kids[i - 1].x + kids[i - 1].width);
    }
  });

  it("gives each rank one row, at that rank's own height", () => {
    const placed = placeRoster(full());
    const rows = [...new Set(placed.boxes.map((box) => box.y))].sort((a, b) => a - b);

    expect(rows).toHaveLength(4);
    expect(
      placed.boxes.filter((box) => box.y === rows[0]).every((box) => box.height === LAYER_H[0]),
    ).toBe(true);
    expect(
      placed.boxes.filter((box) => box.y === rows[3]).every((box) => box.height === LAYER_H[3]),
    ).toBe(true);
  });

  /** Three ranks and not two: a department with nothing running still has a shape. */
  it("still draws three ranks when nothing is in flight", () => {
    const placed = placeRoster(buildRoster(IDLE).nodes);
    const rows = [...new Set(placed.boxes.map((box) => box.y))];

    expect(rows).toHaveLength(3);
    expect(placed.boxes).toHaveLength(4);
  });

  it("marks the director's own item as the one crossing edge", () => {
    const placed = placeRoster(full());
    const crossing = placed.edges.filter((edge) => edge.crosses);

    expect(crossing).toHaveLength(1);
    expect(crossing[0].from).toBe("director:controller");
    expect(crossing[0].to).toBe("item:run-live-1#3");
  });

  it("reports a width and a height that hold every box drawn", () => {
    const placed = placeRoster(full());

    for (const box of placed.boxes) {
      expect(box.x).toBeGreaterThanOrEqual(0);
      expect(box.x + box.width).toBeLessThanOrEqual(placed.width);
      expect(box.y + box.height).toBeLessThanOrEqual(placed.height);
    }
  });

  it("places nothing and reports nothing for an empty roster", () => {
    const placed = placeRoster([]);

    expect(placed.boxes).toEqual([]);
    expect(placed.width).toBe(0);
  });
});
