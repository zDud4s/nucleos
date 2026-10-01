import { describe, expect, it } from "vitest";
import {
  measuredByGenerator,
  parseEvidence,
  type Known,
} from "./knowledge";

function known(over: Partial<Known> = {}): Known {
  return {
    id: 1,
    layer: "episodic",
    scope_kind: "project",
    scope_id: "alpha",
    source: "consolidator",
    generator: "gate",
    kind: "memory",
    title: "A measured fact",
    body: "The gate observed it.",
    status: "active",
    proposal_id: null,
    supersedes: null,
    origin_run_id: 900001,
    evidence: null,
    observations: 1,
    fingerprint: null,
    expires_after_runs: null,
    last_confirmed_at: null,
    shown_count: 0,
    outcome_count: 0,
    green_count: 0,
    last_shown_at: null,
    created_at: "2026-09-20T09:00:00+00:00",
    activated_at: "2026-09-20T09:05:00+00:00",
    ended_at: null,
    ...over,
  };
}

describe("parseEvidence", () => {
  it("reads a malformed value as absent and drops elements it cannot name", () => {
    expect(parseEvidence(null)).toEqual([]);
    expect(parseEvidence("not json")).toEqual([]);
    expect(parseEvidence("{}")).toEqual([]);
    expect(
      parseEvidence(
        JSON.stringify([
          { t: "run", id: 1 },
          { t: "nope", id: 2 },
          { t: "run" },
        ]),
      ),
    ).toEqual([{ t: "run", id: 1 }]);
  });

  it("an evidence of only unknown tags counts as empty", () => {
    expect(parseEvidence(JSON.stringify([{ t: "mystery", id: 7 }]))).toEqual([]);
  });
});

describe("measuredByGenerator", () => {
  it("groups active measured rows by scope and generator and ignores the rest", () => {
    const rows = [
      known({ id: 1, generator: "gate" }),
      known({ id: 2, generator: "gate" }),
      known({ id: 3, generator: "refused-action" }),
      known({ id: 4, scope_kind: "machine", scope_id: null, generator: null }),
      known({ id: 5, source: "owner" }),
      known({ id: 6, status: "proposed" }),
      known({ id: 7, layer: "semantic" }),
    ];

    expect(measuredByGenerator(rows)).toEqual([
      {
        scope: "alpha",
        total: 3,
        byGenerator: [
          { generator: "gate", count: 2 },
          { generator: "refused-action", count: 1 },
        ],
      },
      {
        scope: "this machine",
        total: 1,
        byGenerator: [{ generator: "unknown", count: 1 }],
      },
    ]);
  });
});
