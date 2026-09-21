import { describe, expect, it } from "vitest";
import { FEED_LANES, feedGravityOf, feedKindLeavesOpen, feedLaneKinds, feedLaneOf, feedMarkTone, feedOpenKinds } from "./lanes";
import { readState, statesOf } from "./state-map";

describe("the Feed's lanes", () => {
  it("places every kind the map reads, and nothing the map does not", () => {
    // Both directions, like the map's own completeness test: a kind added to the map without a
    // lane would otherwise fall silently into the machine lane, and a lane row for a kind the map
    // dropped would outlive it as a description of something that no longer exists.
    const mapped = new Set(statesOf("feed"));
    const placed = new Set(feedLaneKinds());
    expect([...mapped].filter((kind) => !placed.has(kind))).toEqual([]);
    expect([...placed].filter((kind) => !mapped.has(kind))).toEqual([]);
    const lanes = new Set(FEED_LANES.map((lane) => lane.id));
    for (const kind of placed) expect(lanes.has(feedLaneOf(kind)), kind).toBe(true);
  });

  it("sends a kind nobody classified to the machine lane, except mail and the web by family", () => {
    expect(feedLaneOf("map_stamp_recorded")).toBe("machine");
    expect(feedLaneOf("sidecar_restarted")).toBe("machine");
    expect(feedLaneOf("email_shopping")).toBe("errands");
    expect(feedLaneOf("web.fetch")).toBe("errands");
  });

  it("uses every lane, so none is drawn empty by construction", () => {
    const used = new Set(feedLaneKinds().map(feedLaneOf));
    expect(used.size).toBe(FEED_LANES.length);
  });
});

describe("a line's gravity", () => {
  it("is the map's tone, and nothing else", () => {
    for (const kind of statesOf("feed")) {
      const tone = readState("feed", kind)?.tone;
      const gravity = feedGravityOf(kind);
      if (tone === "danger") expect(gravity, kind).toBe("wrong");
      else if (tone === "paused") expect(gravity, kind).toBe("held");
      else if (tone === "pending") expect(gravity, kind).toBe("asks");
      else expect(gravity, kind).toBe("routine");
    }
  });

  it("keeps a parked job routine, because no park reason is a question for the reader", () => {
    expect(feedGravityOf("job_waiting")).toBe("routine");
    expect(readState("feed", "job_waiting")?.tone).not.toBe("pending");
    // Slot contention frees itself; only the two a person could lift keep Held Ember.
    expect(readState("wait_reason", "slot")?.tone).not.toBe("pending");
    expect(readState("wait_reason", "budget")?.tone).toBe("paused");
    expect(readState("wait_reason", "excluded")?.tone).toBe("paused");
  });

  it("quota brake wait reason is held ember", () => {
    expect(readState("wait_reason", "quota")?.tone).toBe("paused");
  });

  it("the two lines that ask something of you still do", () => {
    expect(feedGravityOf("email_urgent")).toBe("asks");
    expect(feedGravityOf("promotion_ready")).toBe("asks");
  });

  it("an unknown kind is routine and drawn switched off, never guessed", () => {
    expect(feedGravityOf("nonesuch_kind")).toBe("routine");
    expect(feedMarkTone("nonesuch_kind")).toBe("off");
  });
});

describe("the kinds that leave a sequence open", () => {
  it("are kinds the map reads, and none of them is an exception", () => {
    // An open sequence is work still going. A kind that went wrong, was held or asks for you has
    // said how it ended, so it may never be one of these.
    const mapped = new Set(statesOf("feed"));
    for (const kind of feedOpenKinds()) {
      expect(mapped.has(kind), kind).toBe(true);
      expect(feedGravityOf(kind), kind).toBe("routine");
    }
  });

  it("a parked job and a retried run are open; a finish, a failure and an unknown kind close", () => {
    expect(feedKindLeavesOpen("job_waiting")).toBe(true);
    expect(feedKindLeavesOpen("run_retry")).toBe(true);
    expect(feedKindLeavesOpen("council_stage")).toBe(true);
    for (const kind of ["job_finished", "council_finished", "run_failed_final", "worktree_released", "nonesuch_kind"]) {
      expect(feedKindLeavesOpen(kind), kind).toBe(false);
    }
  });
});
