import { describe, expect, it } from "vitest";

import {
  collisionBadges,
  exclusionEdges,
  orderColumns,
  partnersOf,
  slotDetail,
} from "./fleet-derive";
import {
  LIVE_LIST_LIMIT,
  type FleetExclusion,
  type HeldSlot,
  type Job,
  type ProjectConcurrency,
  type Proposal,
  type RunSearchResult,
} from "./api";

function slot(over: Partial<HeldSlot> = {}): HeldSlot {
  return {
    project_id: "alpha",
    slot: 0,
    owner_kind: "job",
    owner_id: 41,
    claimed_at: "2026-08-09T00:00:00Z",
    ...over,
  };
}

// No `as Job`: the return annotation already does the checking, and the cast would hide exactly the
// missing-field problem it looks like it is guarding against.
function job(over: Partial<Job> = {}): Job {
  return {
    id: 41,
    project_id: "alpha",
    rule_name: null,
    status: "implementing",
    wait_reason: null,
    max_items: 5,
    slot: 0,
    round: 0,
    max_rounds: 1,
    created_at: "2026-08-09T00:00:00Z",
    completed_at: null,
    ...over,
  };
}

function project(over: Partial<ProjectConcurrency> = {}): ProjectConcurrency {
  return {
    project_id: "alpha",
    limit: 2,
    slots: [slot()],
    collision: {
      declared: { state: "not_measured", overlaps: [] },
      observed: { state: "clean", overlaps: [] },
    },
    ...over,
  };
}

describe("slotDetail", () => {
  it("finds the job that holds the slot", () => {
    // No fourth argument, so the default is exercised.
    expect(slotDetail(slot(), [job()], [])).toEqual({ kind: "job", job: job() });
  });

  it("reads a failed listing as detail unavailable, never as an empty slot", () => {
    expect(slotDetail(slot(), null, [], LIVE_LIST_LIMIT)).toEqual({ kind: "unknown" });
  });

  it("reads a listing that came back full as detail unavailable, because it may be cut", () => {
    const full = Array.from({ length: LIVE_LIST_LIMIT }, (_, index) => job({ id: 1000 + index }));

    expect(slotDetail(slot(), full, [], LIVE_LIST_LIMIT)).toEqual({ kind: "unknown" });
  });

  it("calls a slot orphaned only when a whole listing answered and the owner was not in it", () => {
    expect(slotDetail(slot(), [job({ id: 99 })], [], LIVE_LIST_LIMIT)).toEqual({
      kind: "orphaned",
    });
  });

  it("never lets a run id be read as a job id", () => {
    const runSlot = slot({ owner_kind: "run", owner_id: 41 });
    const runs: RunSearchResult[] = [];

    expect(slotDetail(runSlot, [job({ id: 41 })], runs, LIVE_LIST_LIMIT)).toEqual({
      kind: "orphaned",
    });
  });
});

describe("orderColumns", () => {
  it("puts the projects with work in flight first, alphabetically within a tie", () => {
    const columns = orderColumns([
      project({ project_id: "zeta", slots: [] }),
      project({ project_id: "beta", slots: [slot({ project_id: "beta" })] }),
      project({ project_id: "alpha", slots: [] }),
    ]);

    expect(columns.map((column) => column.project_id)).toEqual(["beta", "alpha", "zeta"]);
  });

  it("keeps an idle project's column rather than dropping it", () => {
    expect(orderColumns([project({ slots: [] })])).toHaveLength(1);
  });
});

describe("collisionBadges", () => {
  const owner = { kind: "job", id: 41 };

  it("says nothing when both sources are clean", () => {
    const clean = project({
      collision: {
        declared: { state: "clean", overlaps: [] },
        observed: { state: "clean", overlaps: [] },
      },
    });

    expect(collisionBadges(clean, owner)).toEqual([]);
  });

  it("keeps the two sources apart on screen", () => {
    const both = project({
      slots: [slot(), slot({ slot: 1, owner_id: 42 })],
      collision: {
        declared: {
          state: "collide",
          overlaps: [{ a: owner, b: { kind: "job", id: 42 }, paths: ["planned.rs"] }],
        },
        observed: {
          state: "collide",
          overlaps: [{ a: owner, b: { kind: "job", id: 42 }, paths: ["written.rs"] }],
        },
      },
    });

    const badges = collisionBadges(both, owner);

    expect(badges.map((badge) => badge.source)).toEqual(["observed", "predicted"]);
    expect(badges[0].paths).toEqual(["written.rs"]);
    expect(badges[1].paths).toEqual(["planned.rs"]);
  });

  it("shows nothing to a slot no overlap names", () => {
    const elsewhere = project({
      slots: [slot(), slot({ slot: 1, owner_id: 42 }), slot({ slot: 2, owner_id: 43 })],
      collision: {
        declared: { state: "clean", overlaps: [] },
        observed: {
          state: "collide",
          overlaps: [{ a: { kind: "job", id: 42 }, b: { kind: "job", id: 43 }, paths: ["x.rs"] }],
        },
      },
    });

    expect(collisionBadges(elsewhere, owner)).toEqual([]);
  });

  it("says not measured out loud when there is something it could have collided with", () => {
    const twoTrees = project({
      slots: [slot(), slot({ slot: 1, owner_id: 42 })],
      collision: {
        declared: { state: "not_measured", overlaps: [] },
        observed: { state: "not_measured", overlaps: [] },
      },
    });

    const badges = collisionBadges(twoTrees, owner);

    expect(badges).toHaveLength(2);
    expect(badges.every((badge) => badge.state === "not_measured")).toBe(true);
  });

  it("stays quiet about a question that cannot arise", () => {
    const alone = project({
      slots: [slot()],
      collision: {
        declared: { state: "not_measured", overlaps: [] },
        observed: { state: "not_measured", overlaps: [] },
      },
    });

    expect(collisionBadges(alone, owner)).toEqual([]);
  });
});

describe("the edges: what somebody asked, and what is in force", () => {
  function rule(over: Partial<FleetExclusion> = {}): FleetExclusion {
    return {
      id: 1,
      project_id: "alpha",
      job_low: 41,
      job_high: 42,
      proposal_id: 9,
      paths: null,
      created_at: "2026-08-15T00:00:00Z",
      ...over,
    };
  }

  function request(low: number, high: number, over: Partial<Proposal> = {}): Proposal {
    return {
      id: 9,
      kind: "fleet-exclusion",
      status: "pending",
      run_id: null,
      session_id: null,
      project_id: "alpha",
      tool_name: null,
      reasoning: "they both touch it",
      tool_input: JSON.stringify({ pair: `${low}:${high}`, job_low: low, job_high: high }),
      created_at: "2026-08-15T00:00:00Z",
      decided_at: null,
      ...over,
    };
  }

  /**
   * Asked and in force are two drawings because they are two situations. A pending edge constrains
   * nothing yet, and showing it as a rule leaves somebody asking why both jobs are still running.
   */
  it("keeps a request apart from a rule", () => {
    const edges = exclusionEdges([rule()], [request(7, 8)]);

    expect(edges).toEqual([
      { low: 41, high: 42, state: "active", id: 1 },
      { low: 7, high: 8, state: "pending", id: 9 },
    ]);
  });

  /** One pair is one edge, and of the two readings the rule is the one doing something. */
  it("draws a pair once when it is both asked about and in force", () => {
    const edges = exclusionEdges([rule()], [request(41, 42)]);

    expect(edges).toEqual([{ low: 41, high: 42, state: "active", id: 1 }]);
  });

  /** Other kinds, decided ones, and anything unreadable are not edges. */
  it("draws nothing from what it cannot read", () => {
    const decided = request(1, 2, { status: "approved" });
    const otherKind = request(3, 4, { kind: "action-approval" });
    const malformed = request(5, 6, { tool_input: "not json" });
    const empty = request(5, 6, { tool_input: null });
    const noPair = request(5, 6, { tool_input: JSON.stringify({ pair: "5:6" }) });

    expect(exclusionEdges([], [decided, otherKind, malformed, empty, noPair])).toEqual([]);
    // A failed read of either list is not an empty one: it draws nothing and claims nothing.
    expect(exclusionEdges(null, null)).toEqual([]);
  });

  /**
   * The same edge reads differently from its two ends.
   *
   * Only the higher id is ever held, so a card saying "waiting for the other one" at both ends
   * would describe a deadlock the daemon cannot produce.
   */
  it("says which end of an edge is the one that waits", () => {
    const edges = exclusionEdges([rule({ job_low: 41, job_high: 42 })], []);

    expect(partnersOf(edges, 42)).toEqual([
      { low: 41, high: 42, state: "active", id: 1, partner: 41, waits: true },
    ]);
    expect(partnersOf(edges, 41)).toEqual([
      { low: 41, high: 42, state: "active", id: 1, partner: 42, waits: false },
    ]);
    expect(partnersOf(edges, 99)).toEqual([]);
  });
});
