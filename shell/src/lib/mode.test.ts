import { describe, expect, it } from "vitest";
import { promotionConsequence } from "./mode";
import { project } from "../test/harness";

/**
 * The sentence the interlock arms with, before a project is let loose.
 *
 * It exists because the armed half of the interlock used to say "It may act on its own",
 * which is the same sentence as the button that had just been pressed — it named neither
 * the project nor the ceiling that would govern it. A confirmation that repeats the offer
 * is a second click, not a second thought.
 */
describe("promotionConsequence", () => {
  it("names the project and the slots it is holding", () => {
    const said = promotionConsequence(
      project({ project_id: "alpha", wip_limit: 4, open_proposals: 3 }),
    );
    expect(said).toBe("alpha acts on its own — 3 of 4 proposal slots, no approval");
  });

  /*
    A null ceiling is the ABSENCE of one and is never written as a zero.

    "0 of 0 proposal slots" would read as a project allowed to do nothing, which is the
    exact opposite of what no ceiling means — the same trap `ceiling()` on Home already
    avoids for a null spending limit.
  */
  it("says no ceiling rather than a zero when there is none", () => {
    const said = promotionConsequence(
      project({ project_id: "beta", wip_limit: null, open_proposals: 3 }),
    );
    expect(said).toBe("beta acts on its own — no ceiling on proposals, no approval");
    expect(said).not.toContain("0");
  });

  /* The arithmetic is the roster row's. A full queue is reported, never corrected. */
  it("carries the daemon's numbers as they are, even at the ceiling", () => {
    const said = promotionConsequence(
      project({ project_id: "gamma", wip_limit: 2, open_proposals: 2 }),
    );
    expect(said).toBe("gamma acts on its own — 2 of 2 proposal slots, no approval");
  });
});
