import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { render, screen } from "@testing-library/react";
import { CostLine, money } from "./readings";

const here = dirname(fileURLToPath(import.meta.url));
const sheet = (...parts: string[]): string => readFileSync(join(here, ...parts), "utf8");

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
    expect(money(0.031)).toBe("$0.031");
    expect(money(1.72)).toBe("$1.72");
    expect(money(0.03)).toBe("$0.03");
  });

  it("never writes money with one decimal", () => {
    expect(money(2)).toBe("$2.00");
    expect(money(0)).toBe("$0.00");
    expect(money(1.7)).toBe("$1.70");
    expect(money(10)).toBe("$10.00");
  });

  it("keeps the fourth decimal when it is the only one that says anything", () => {
    // The case the four-decimal rule existed for: a cheap run must not read as free.
    expect(money(0.004)).toBe("$0.004");
    expect(money(0.0004)).toBe("$0.0004");
  });

  it("refuses to round a real spend down to nothing", () => {
    // Below the last decimal it can print, and still not zero. `$0.00` here would
    // be the exact lie the four-decimal rule was written to prevent.
    expect(money(0.00004)).toBe("< $0.0001");
    expect(money(0.00000001)).toBe("< $0.0001");
  });
});

describe("CostLine", () => {
  it("reads an absent cost as not recorded, exactly as a null one does", () => {
    render(<CostLine costUsd={undefined as unknown as number | null} />);

    expect(screen.getByText("cost not recorded")).toBeDefined();
  });
});

/**
 * Where the rules for these components are allowed to live.
 *
 * `readings.tsx` is a design system primitive, so its rules belong in `src/ui.css` with
 * the rest of the `ui-` prefix. They spent several rounds housed in `pages/runs.css`
 * under a heading that said "on loan", which is the kind of debt nothing can see: `tsc`
 * and vitest both pass on a class no sheet has ever heard of, and the second page to
 * render a `ContextMeter` would get an unstyled one with no failure anywhere. This is
 * the assertion that keeps the loan repaid.
 */
describe("where the reading rules live", () => {
  it("the reading and meter rules live in the design system sheet", () => {
    const ui = sheet("..", "ui.css");
    expect(ui).toContain(".ui-reading-time");
    expect(ui).toContain(".ui-cost-money");
    expect(ui).toContain(".ui-meter-track");
    expect(ui).toContain(".ui-meter-mark");

    // And no raw opacity travelled with them: `tokens.css` owns the two there are.
    expect(ui).toContain("opacity: var(--opacity-quiet)");
  });

  it("the runs sheet defines no ui- rule and keeps the house breakpoint", () => {
    const runs = sheet("..", "pages", "runs.css");
    expect(runs).not.toMatch(/^\.ui-/m);

    // 68rem was this sheet's alone; every other sheet folds at 60rem.
    expect(runs).not.toContain("68rem");
  });
});
