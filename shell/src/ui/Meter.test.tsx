import { describe, expect, it } from "vitest";
import { render, screen } from "@testing-library/react";

import { LimitChip, Meter, usd } from "./Meter";

/**
 * The pair's whole reason to exist is the distinction between a ceiling
 * something occupies and a rule applied to each task, so these are the two
 * assertions that matter: an absent ceiling never becomes a bar, and a per-task
 * rule never grows one.
 */

describe("Meter", () => {
  it("draws a dashed rail for a null ceiling, and never a bar at 0% or 100%", () => {
    const { container } = render(<Meter label="at work" value={2} ceiling={null} />);

    // The rail, not a bar: no fill element exists at all, so there is nothing
    // that could be read as empty or as full.
    expect(container.querySelector(".ui-gauge-open")).not.toBeNull();
    expect(container.querySelector(".ui-gauge-fill")).toBeNull();

    // And it says so in words, which is what an assistive reader gets.
    expect(screen.getByText("no ceiling")).toBeDefined();
    expect(screen.getByRole("img", { name: "at work: 2, no ceiling" })).toBeDefined();

    // The count is still shown. No ceiling is not no reading.
    expect(screen.getByText("2")).toBeDefined();
  });

  it("fills to the share of a real ceiling, and marks the one that is spent", () => {
    const { container, rerender } = render(<Meter label="waiting on you" value={3} ceiling={5} />);

    const fill = container.querySelector(".ui-gauge-fill") as HTMLElement;
    expect(fill.style.width).toBe("60%");
    expect(screen.getByText("3 / 5")).toBeDefined();
    expect(container.querySelector(".ui-gauge-full")).toBeNull();

    rerender(<Meter label="waiting on you" value={5} ceiling={5} />);
    expect(container.querySelector(".ui-gauge-full")).not.toBeNull();
  });

  it("treats a ceiling of zero as a real setting rather than a division by nothing", () => {
    // `null` is the brake off; `0` is the brake fully on — "never start one".
    // The two must not render the same way, and neither may render as NaN.
    const { container } = render(<Meter label="at work" value={1} ceiling={0} />);

    const fill = container.querySelector(".ui-gauge-fill") as HTMLElement;
    expect(fill.style.width).toBe("100%");
    expect(screen.getByText("1 / 0")).toBeDefined();
    expect(container.querySelector(".ui-gauge-open")).toBeNull();
  });
});

describe("LimitChip", () => {
  it("writes a per-task rule with no bar anywhere in it", () => {
    const { container } = render(<LimitChip name="rounds" ceiling={4} />);

    expect(screen.getByText("rounds")).toBeDefined();
    expect(screen.getByText("≤ 4")).toBeDefined();
    // The point of the chip: nothing is being consumed, so nothing is drawn as
    // consumed. A bar here would promise a reading that does not exist.
    expect(container.querySelector(".ui-gauge-track")).toBeNull();
    expect(container.querySelector(".ui-gauge-fill")).toBeNull();
  });

  it("writes money as money, and an unset rule as no ceiling rather than as zero", () => {
    render(<LimitChip name="spend" ceiling={5} format={usd} />);
    expect(screen.getByText("≤ $5.00")).toBeDefined();

    render(<LimitChip name="spend" ceiling={null} format={usd} />);
    expect(screen.getByText("no ceiling")).toBeDefined();
    expect(screen.queryByText("≤ $0.00")).toBeNull();
  });
});
