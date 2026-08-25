import { describe, expect, it } from "vitest";
import { money } from "./readings";

/**
 * What a spend is allowed to read as.
 *
 * Two invariants, and they pull against each other, which is why they are asserted
 * together: money is written with at least two decimals, and a run that spent
 * something is never written as having spent nothing. Everything else — how many
 * decimals an ordinary turn gets — falls out of removing the zeros that carry no
 * information.
 */
describe("money", () => {
  it("drops the zero that says nothing", () => {
    // The reading that started this: a turn costing 3.1 cents, written with a
    // trailing zero in the footing under every turn of a long transcript.
    expect(money(0.031)).toBe("$ 0.031");
    expect(money(1.72)).toBe("$ 1.72");
    expect(money(0.03)).toBe("$ 0.03");
  });

  it("never writes money with one decimal", () => {
    expect(money(2)).toBe("$ 2.00");
    expect(money(0)).toBe("$ 0.00");
    expect(money(1.7)).toBe("$ 1.70");
    expect(money(10)).toBe("$ 10.00");
  });

  it("keeps the fourth decimal when it is the only one that says anything", () => {
    // The case the four-decimal rule existed for: a cheap run must not read as free.
    expect(money(0.004)).toBe("$ 0.004");
    expect(money(0.0004)).toBe("$ 0.0004");
  });

  it("refuses to round a real spend down to nothing", () => {
    // Below the last decimal it can print, and still not zero. `$ 0.00` here would
    // be the exact lie the four-decimal rule was written to prevent.
    expect(money(0.00004)).toBe("< $ 0.0001");
    expect(money(0.00000001)).toBe("< $ 0.0001");
  });
});
