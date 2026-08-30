// §spec mapa-do-projeto
import { describe, expect, it } from "vitest";

import { claimedFiles, claimsFor, isSettled, standingLabel } from "./map-claims";
import type { Anchored, Junction } from "../data/project-map";

const decision = (over: Partial<Anchored> = {}): Anchored => ({
  decision_id: 1,
  ordinal: 1,
  spec_slug: "mapa-do-projeto",
  section: "5.1",
  text: "A decision.",
  kind: "countable",
  anchor: "declared",
  modules: [],
  foreign: [],
  record: null,
  ...over,
});

const junction = (decisions: Anchored[]): Junction => ({
  decisions,
  unclaimed: [],
  unmatched: [],
  counts: {
    decisions: decisions.length,
    declared: 0,
    ambiguous: 0,
    silent: 0,
    unnumbered: 0,
    unclaimed: 0,
    unmatched: 0,
  },
});

describe("claimedFiles", () => {
  it("gathers every file any decision names", () => {
    const found = claimedFiles(
      junction([
        decision({ decision_id: 1, modules: ["a.rs", "b.rs"] }),
        decision({ decision_id: 2, modules: ["b.rs", "c.rs"] }),
      ]),
    );
    expect([...found].sort()).toEqual(["a.rs", "b.rs", "c.rs"]);
  });

  it("is empty when nothing has been approved, rather than absent", () => {
    // A project on day one has no decisions and every file unclaimed. That is a true answer about
    // the project, and the panel says it in words; an undefined here would be a broken screen.
    expect(claimedFiles(junction([])).size).toBe(0);
  });
});

describe("claimsFor", () => {
  it("finds only the decisions that name this file", () => {
    const world = junction([
      decision({ decision_id: 1, modules: ["a.rs"] }),
      decision({ decision_id: 2, modules: ["b.rs"] }),
    ]);
    expect(claimsFor(world, "a.rs").map((d) => d.decision_id)).toEqual([1]);
    expect(claimsFor(world, "nowhere.rs")).toEqual([]);
  });

  it("orders by document and then by the ordinal the owner approved, never by verdict", () => {
    // A row that moves when its verdict changes is a row the reader cannot find twice.
    const world = junction([
      decision({ decision_id: 3, spec_slug: "zeta", ordinal: 1, modules: ["a.rs"] }),
      decision({ decision_id: 2, spec_slug: "alfa", ordinal: 9, modules: ["a.rs"] }),
      decision({ decision_id: 1, spec_slug: "alfa", ordinal: 2, modules: ["a.rs"] }),
    ]);
    expect(claimsFor(world, "a.rs").map((d) => d.decision_id)).toEqual([1, 2, 3]);
  });
});

describe("standingLabel", () => {
  it("uses the words the stamp panel already uses", () => {
    expect(standingLabel({ state: "settled", stamped_at: "", watch: {} as never })).toBe("stamped");
    expect(standingLabel({ state: "never" })).toBe("nobody has looked");
    expect(standingLabel({ state: "withdrawn", stamped_at: "", note: null })).toBe("withdrawn");
  });

  it("says nobody has looked when the núcleo sent no standing at all", () => {
    // `apiFetch` is a cast, so a missing key is `undefined` rather than a state. Reading that as a
    // green would put the owner's word over something they never saw.
    expect(standingLabel(undefined)).toBe("nobody has looked");
  });
});

describe("isSettled", () => {
  it("counts only a standing verdict, and lapsed is not one", () => {
    // §7: a verdict with an expiry is not a verdict that never expires. The code moved since.
    expect(isSettled({ state: "settled", stamped_at: "", watch: {} as never })).toBe(true);
    expect(isSettled({ state: "lapsed", stamped_at: "", why: {} as never })).toBe(false);
    expect(isSettled(undefined)).toBe(false);
  });
});
