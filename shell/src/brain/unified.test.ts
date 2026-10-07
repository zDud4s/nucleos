import { describe, expect, it } from "vitest";
import type { Known } from "../data/knowledge";
import type { OwnerNote } from "../data/owner-notes";
import { filterItems, toItems, type ItemFilters } from "./unified";

function note(over: Partial<OwnerNote> = {}): OwnerNote {
  return {
    id: 1,
    text: "a note",
    origin: "shell",
    state: "active",
    created_at: "2026-09-01T09:00:00+00:00",
    updated_at: "2026-09-01T09:00:00+00:00",
    ...over,
  };
}

function known(over: Partial<Known> = {}): Known {
  return {
    id: 1,
    layer: "semantic",
    scope_kind: "project",
    scope_id: "nucleos",
    source: "run",
    generator: null,
    kind: "memory",
    title: "Suite needs PATH",
    body: "Tests spawn echo.",
    status: "active",
    proposal_id: null,
    supersedes: null,
    origin_run_id: null,
    evidence: null,
    observations: null,
    fingerprint: null,
    expires_after_runs: null,
    last_confirmed_at: null,
    shown_count: 0,
    outcome_count: 0,
    green_count: 0,
    last_shown_at: null,
    created_at: "2026-08-01T09:00:00+00:00",
    activated_at: "2026-08-01T09:05:00+00:00",
    ended_at: null,
    ...over,
  };
}

const ALL: ItemFilters = { type: "all", state: "all", layer: "all", scope: "all", q: "" };

describe("toItems", () => {
  it("tags each row with its kind", () => {
    const items = toItems([note()], [known()]);
    expect(items.map((item) => item.kind)).toEqual(["note", "knowledge"]);
  });
});

describe("filterItems", () => {
  it("sorts newest first, using activated_at for knowledge", () => {
    const items = toItems(
      [note({ id: 1, created_at: "2026-09-01T00:00:00+00:00" })],
      [
        known({ id: 5, activated_at: "2026-09-03T00:00:00+00:00" }),
        known({ id: 6, activated_at: null, created_at: "2026-09-02T00:00:00+00:00" }),
      ],
    );
    const out = filterItems(items, ALL).map((item) => (item.kind === "note" ? `n${item.note.id}` : `k${item.known.id}`));
    expect(out).toEqual(["k5", "k6", "n1"]);
  });

  it("filters by type", () => {
    const items = toItems([note()], [known()]);
    expect(filterItems(items, { ...ALL, type: "note" }).map((i) => i.kind)).toEqual(["note"]);
    expect(filterItems(items, { ...ALL, type: "knowledge" }).map((i) => i.kind)).toEqual(["knowledge"]);
  });

  it("filters by state; the list keeps proposed rows out of in force", () => {
    const items = toItems(
      [note({ id: 1 }), note({ id: 2, state: "archived" })],
      [known({ id: 3 }), known({ id: 4, status: "proposed" }), known({ id: 5, status: "rejected" })],
    );
    const ids = (state: ItemFilters["state"]) =>
      filterItems(items, { ...ALL, state }).map((i) => (i.kind === "note" ? `n${i.note.id}` : `k${i.known.id}`)).sort();
    expect(ids("in_force")).toEqual(["k3", "n1"]);
    expect(ids("out")).toEqual(["k5", "n2"]);
    expect(ids("all")).toHaveLength(5);
  });

  it("layer and scope apply to knowledge and exclude notes", () => {
    const items = toItems(
      [note()],
      [
        known({ id: 2, layer: "episodic" }),
        known({ id: 3, scope_kind: "machine", scope_id: null }),
      ],
    );
    const layer = filterItems(items, { ...ALL, layer: "episodic" });
    expect(layer.map((i) => i.kind)).toEqual(["knowledge"]);
    const scope = filterItems(items, { ...ALL, scope: "machine" });
    expect(scope.map((i) => (i.kind === "knowledge" ? i.known.id : 0))).toEqual([3]);
    expect(filterItems(items, { ...ALL, scope: "nucleos" })).toHaveLength(1);
  });

  it("q matches knowledge title and body, case-insensitively, and leaves notes alone", () => {
    const items = toItems(
      [note({ text: "unrelated" })],
      [known({ id: 2, title: "Alpha" }), known({ id: 3, title: "Beta", body: "mentions ALPHA here" }), known({ id: 4, title: "Gamma" })],
    );
    const out = filterItems(items, { ...ALL, q: "alpha" });
    expect(out.filter((i) => i.kind === "knowledge")).toHaveLength(2);
    expect(out.filter((i) => i.kind === "note")).toHaveLength(1);
  });
});
