import { describe, expect, it } from "vitest";
import { promotionConfirmLabel, promotionConsequence } from "./mode";
import { project } from "../test/harness";

/**
 * The sentence the caller prints under the control, before a project is let loose.
 *
 * It exists because the armed half of the interlock used to say "It may act on its own",
 * which is the same sentence as the button that had just been pressed — it named neither
 * the project nor the ceiling that would govern it. A confirmation that repeats the offer
 * is a second click, not a second thought.
 *
 * It is no longer the armed LABEL — see `promotionConfirmLabel` below — because 52 characters
 * inside a switch segment wrap and grow the row under the finger about to press it again.
 */
describe("promotionConsequence", () => {
  /*
    The count is a count already taken, and the words have to say so.

    "3 of 4 proposal slots" in a sentence about what will happen next reads as an allowance
    being granted — as though pressing this were what hands the project three slots. Three are
    already spent; one is what is left. The tense around the clause cannot say which, so the
    clause does.
  */
  it("names the project and the slots already in use", () => {
    const said = promotionConsequence(
      project({ project_id: "alpha", wip_limit: 4, open_proposals: 3 }),
    );
    expect(said).toBe(
      "alpha acts on its own — 3 of its 4 proposal slots already in use, no approval",
    );
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
    // Absence, not a number dressed as absence: nothing here counts anything.
    expect(said).not.toMatch(/in use/);
  });

  /* The arithmetic is the roster row's. A full queue is reported, never corrected. */
  it("carries the daemon's numbers as they are, even at the ceiling", () => {
    const said = promotionConsequence(
      project({ project_id: "gamma", wip_limit: 2, open_proposals: 2 }),
    );
    expect(said).toBe(
      "gamma acts on its own — 2 of its 2 proposal slots already in use, no approval",
    );
  });
});

/**
 * What the armed segment itself says.
 *
 * Two jobs pulling against each other: not the words of the button it replaced, and short
 * enough to stay on one line inside a switch segment. The consequence sentence did the first
 * and failed the second — measured, it grew `.ap-project-row` from 90.6 to 125.0 pixels while
 * armed, which moves the button away from the pointer that has four seconds to press it again.
 */
describe("promotionConfirmLabel", () => {
  it("is a short line that names the project", () => {
    const said = promotionConfirmLabel(project({ project_id: "alpha" }));
    expect(said).toBe("Let alpha act");

    // Not the same words as the offer it replaces. A confirmation that repeats the offer is a
    // second click, not a second thought — and on a roster of four, the project's name is the
    // one word that says which row is about to be let loose.
    expect(said).not.toBe("Let it act");
    expect(said).toContain("alpha");
  });

  /*
    It says nothing about the ceiling, deliberately.

    That is `promotionConsequence`'s sentence and the caller renders it under the control,
    where it has a full-width line. Two sentences competing for one segment is how the row
    grew in the first place.
  */
  it("carries no arithmetic, whatever the row holds", () => {
    const said = promotionConfirmLabel(
      project({ project_id: "bravo", wip_limit: 2, open_proposals: 2 }),
    );
    expect(said).toBe("Let bravo act");
    expect(said).not.toMatch(/slot|proposal|approval/);
  });
});
