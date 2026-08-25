import { describe, expect, it } from "vitest";

import { detectDrift, snapshotFromView, type TeamSnapshot } from "./drift";
import type { TeamView } from "../data/teams";

/**
 * Table tests, and they are written before any of the tab that uses this
 * exists. The failure being guarded against — a hire deleted by somebody
 * saving a form they opened before the hire happened — leaves no trace on
 * screen, so it has to be provable without a screen.
 */

function snapshot(overrides: Partial<TeamSnapshot> = {}): TeamSnapshot {
  return {
    name: "Finanças",
    mission: "keep the books straight",
    directorAgentId: "controller",
    maxRounds: 4,
    maxParallel: 2,
    budgetUsd: 5,
    maxOpenActions: 5,
    maxLiveRuns: 1,
    members: ["controller", "auditor"],
    grants: [{ kind: "send_email", mode: "propose" }],
    ...overrides,
  };
}

describe("detectDrift", () => {
  it("reports a field the daemon changed under a form that was not editing it", () => {
    // 09:41 the form was seeded with two; 09:52 a recruit was hired elsewhere.
    const seed = snapshot();
    const current = snapshot({ members: ["controller", "auditor", "tax-analyst"] });

    const drifted = detectDrift(seed, current, []);

    expect(drifted).toHaveLength(1);
    expect(drifted[0].field).toBe("members");
    expect(drifted[0].label).toBe("Staff");
    expect(drifted[0].was).toBe("auditor, controller");
    expect(drifted[0].now).toBe("auditor, controller, tax-analyst");
  });

  it("says nothing about a field the person edited themselves", () => {
    // The same drift, except this time they were editing the roster. Removing
    // somebody on purpose is their decision and there is nothing to ask.
    const seed = snapshot();
    const current = snapshot({ members: ["controller", "auditor", "tax-analyst"] });

    expect(detectDrift(seed, current, ["members"])).toEqual([]);
  });

  it("says nothing when nothing moved", () => {
    expect(detectDrift(snapshot(), snapshot(), [])).toEqual([]);
  });

  it("does not mistake a reordered roster or a reordered grant list for a change", () => {
    // The daemon re-inserts with INSERT OR IGNORE and reads back in its own
    // order, so order is not a fact anybody set. A guard that fired on it would
    // fire constantly, and a guard people click through is not a guard.
    const seed = snapshot({
      members: ["auditor", "controller"],
      grants: [
        { kind: "send_email", mode: "propose" },
        { kind: "file_document", mode: "allow" },
      ],
    });
    const current = snapshot({
      members: ["controller", "auditor"],
      grants: [
        { kind: "file_document", mode: "allow" },
        { kind: "send_email", mode: "propose" },
      ],
    });

    expect(detectDrift(seed, current, [])).toEqual([]);
  });

  it("reports an empty roster on either side without inventing a value", () => {
    const empty = snapshot({ members: [], grants: [] });

    expect(detectDrift(empty, empty, [])).toEqual([]);

    const grew = detectDrift(empty, snapshot({ members: ["controller"], grants: [] }), []);
    expect(grew).toHaveLength(1);
    expect(grew[0].was).toBe("nobody");
    expect(grew[0].now).toBe("controller");
  });

  it("never writes an absent budget ceiling as zero", () => {
    const drifted = detectDrift(snapshot({ budgetUsd: null }), snapshot({ budgetUsd: 5 }), []);

    expect(drifted).toHaveLength(1);
    expect(drifted[0].was).toBe("no ceiling");
    expect(drifted[0].now).toBe("$5.00");
  });

  it("reports every drifted field at once, not just the first", () => {
    const drifted = detectDrift(
      snapshot(),
      snapshot({ mission: "and file them", maxRounds: 6, members: ["controller"] }),
      [],
    );

    expect(drifted.map((one) => one.field)).toEqual(["mission", "maxRounds", "members"]);
  });
});

describe("snapshotFromView", () => {
  it("reads a department off the wire without renaming absence into zero", () => {
    const view: TeamView = {
      id: "financas",
      name: "Finanças",
      mission: "keep the books straight",
      director_agent_id: "controller",
      max_rounds: 4,
      max_parallel: 2,
      budget_usd: null,
      max_open_actions: 5,
      max_live_runs: 1,
      created_at: "2026-08-24T09:00:00Z",
      updated_at: "2026-08-24T09:00:00Z",
      members: ["controller"],
      grants: [],
    };

    expect(snapshotFromView(view)).toEqual({
      name: "Finanças",
      mission: "keep the books straight",
      directorAgentId: "controller",
      maxRounds: 4,
      maxParallel: 2,
      budgetUsd: null,
      maxOpenActions: 5,
      maxLiveRuns: 1,
      members: ["controller"],
      grants: [],
    });
  });
});
