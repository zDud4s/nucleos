import { describe, expect, it, vi } from "vitest";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

import { cancellableOwner, type HeldSlot } from "./fleet";

const held = (over: Partial<HeldSlot>): HeldSlot =>
  ({
    project_id: "alpha", slot: 1, owner_kind: "run", owner_id: 7, claimed_at: "2026-10-20T09:00:00Z",
    job_id: null, ordinal: null, ...over,
  }) as HeldSlot;

describe("cancellableOwner", () => {
  it("offers a stop for a run and for a job, aimed at the owner's own id", () => {
    expect(cancellableOwner(held({ owner_kind: "run", owner_id: 7 }))).toEqual({ kind: "run", id: 7 });
    expect(cancellableOwner(held({ owner_kind: "job", owner_id: 41 }))).toEqual({ kind: "job", id: 41 });
  });

  it("offers none for an item: no route cancels an item, and its owner_id is not a run id", () => {
    expect(cancellableOwner(held({ owner_kind: "item", owner_id: 7, job_id: 41, ordinal: 0 }))).toBeNull();
  });

  it("offers none for a wave: its owner_id is a wave_workers.id, the same wrong-number gesture", () => {
    expect(cancellableOwner(held({ owner_kind: "wave", owner_id: 7 }))).toBeNull();
  });
});
