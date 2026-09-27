import { describe, expect, it } from "vitest";
import {
  MODE_REFUSAL_PROSE,
  MODE_SENTENCES,
  promotionConfirmLabel,
  promotionConsequence,
} from "./mode";
import type { PrerequisiteList } from "./mode";
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
      project({ project_id: "alpha", wip_limit: 4, open_review_items: 3 }),
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
      project({ project_id: "beta", wip_limit: null, open_review_items: 3 }),
    );
    expect(said).toBe("beta acts on its own — no ceiling on proposals, no approval");
    expect(said).not.toContain("0");
    // Absence, not a number dressed as absence: nothing here counts anything.
    expect(said).not.toMatch(/in use/);
  });

  /* The arithmetic is the roster row's. A full queue is reported, never corrected. */
  it("carries the daemon's numbers as they are, even at the ceiling", () => {
    const said = promotionConsequence(
      project({ project_id: "gamma", wip_limit: 2, open_review_items: 2 }),
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
      project({ project_id: "bravo", wip_limit: 2, open_review_items: 2 }),
    );
    expect(said).toBe("Let bravo act");
    expect(said).not.toMatch(/slot|proposal|approval/);
  });
});

/**
 * What the mode door says when it answers 422, as data.
 *
 * The route answers a bare status for four prerequisites and a fifth that only acting needs, and
 * three of them are files a person has to go and look at. Inside one sentence those paths could
 * only be read; as a list each one can be drawn as a path, and the words around it stay one copy.
 */
describe("MODE_SENTENCES", () => {
  it("the 422 prerequisites are data, four items and a plus, two of them paths", () => {
    // Narrowed through `unknown` by the test itself, so what is asserted is the shape and not
    // whatever the module's own type already promises.
    const value: unknown = MODE_SENTENCES.unprocessable;
    expect(typeof value).toBe("object");
    const list = value as PrerequisiteList;

    expect(list.items).toHaveLength(4);
    expect(list.plus).toBeDefined();

    const paths = [...list.items, list.plus]
      .filter((item) => item.path !== undefined)
      .map((item) => item.path);
    // Onboarding is a prerequisite and not a file: whether the project keeps any particular
    // workflow in its folder is none of the mode door's business.
    expect(paths).toEqual([".claude/hooks/ask_daemon.py", ".claude/settings.json"]);
    expect(list.items.map((item) => item.text)).toContain("the project onboarded to NucleOS");

    // The other two answers are still sentences: nothing in them is a list.
    expect(MODE_SENTENCES.bad_request).toBe("that is not one of the three settings");
    expect(MODE_SENTENCES.internal).toBe("the núcleo hit an error of its own while changing this");
  });
});

/**
 * The 422 as it read before it became a list, pasted byte for byte from `MODE_SENTENCES`.
 *
 * A surface that still prints a sentence must print this one, not a re-join of the list that has
 * drifted by a comma. Two of its characters are not ASCII, and a paste that lost either would pass
 * against itself — hence the two guards at the top of the case.
 */
const OLD_UNPROCESSABLE =
  "the núcleo would not put this project into that mode, and it did not say which prerequisite is missing. It needs all of these: a folder, the project onboarded to NucleOS, .claude/hooks/ask_daemon.py on disk, and a PreToolUse hook in .claude/settings.json naming that file — plus, to act, a folder that is a git repository.";

describe("MODE_REFUSAL_PROSE", () => {
  it("the prose form is byte-identical to the sentence it replaces", () => {
    // Escapes, not the characters: a guard spelled with the same bytes as the paste proves nothing.
    expect(OLD_UNPROCESSABLE).toContain("\u2014");
    expect(OLD_UNPROCESSABLE).toContain("n\u00facleo");

    expect(MODE_REFUSAL_PROSE).toBeDefined();
    expect(MODE_REFUSAL_PROSE.unprocessable).toBe(OLD_UNPROCESSABLE);
    expect(MODE_REFUSAL_PROSE.bad_request).toBe("that is not one of the three settings");
    // The one named refusal says what is missing, and never where a workflow's file would be.
    expect(MODE_REFUSAL_PROSE.not_onboarded).toMatch(/has not been onboarded/);
    expect(MODE_REFUSAL_PROSE.not_onboarded).not.toMatch(/\.ai\/|workflow\.md/);
    expect(MODE_REFUSAL_PROSE.internal).toBe(
      "the núcleo hit an error of its own while changing this",
    );
  });
});
