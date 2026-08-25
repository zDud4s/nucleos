import { describe, expect, it } from "vitest";
import { render } from "@testing-library/react";

import { Sparkline } from "./Sparkline";

/**
 * The geometry, because the geometry is where this component goes wrong.
 *
 * Its two shipped defects were both invisible to every other kind of test: an
 * area chart drew a filled slab over a flat series, and a slot of
 * `width / count` spread six days across a 96px table cell as loose blocks.
 * Neither changed a single string, so nothing that reads text could have had an
 * opinion. These read the `<rect>`s.
 */

function bars(container: HTMLElement) {
  return [...container.querySelectorAll("rect.ui-spark-bar")].map((rect) => ({
    x: Number(rect.getAttribute("x")),
    width: Number(rect.getAttribute("width")),
    height: Number(rect.getAttribute("height")),
  }));
}

describe("Sparkline", () => {
  it("draws one bar per value, and every one of them inside the box", () => {
    const { container } = render(
      <Sparkline values={[1, 1, 1, 1, 1, 1]} label="the 6 runs in the window" width={96} height={20} />,
    );

    const drawn = bars(container);
    expect(drawn).toHaveLength(6);
    for (const bar of drawn) {
      expect(bar.x).toBeGreaterThanOrEqual(0);
      expect(bar.x + bar.width).toBeLessThanOrEqual(96);
    }
  });

  it("keeps the bars adjacent instead of spreading them across the box", () => {
    // The defect: `slot = width / count` gave three days in a 96px cell 32px
    // each, so 9px bars sat 23px apart and read as loose blocks rather than a
    // series.
    const { container } = render(
      <Sparkline values={[1, 2, 1]} label="the 3 runs in the window" width={96} height={20} />,
    );

    const drawn = bars(container);
    expect(drawn).toHaveLength(3);
    const gap = drawn[1].x - (drawn[0].x + drawn[0].width);
    expect(gap).toBeLessThanOrEqual(4);
  });

  it("ends at the right edge, so the newest bar is at the same x on every row", () => {
    const { container } = render(
      <Sparkline values={[1, 1, 1]} label="the 3 runs in the window" width={96} height={20} />,
    );

    const drawn = bars(container);
    const last = drawn[drawn.length - 1];
    // Flush right within one slot's inset — down a column of these, "now" lines up.
    expect(96 - (last.x + last.width)).toBeLessThanOrEqual(2);
  });

  it("gives a day with nothing in it a mark rather than a gap", () => {
    const { container } = render(
      <Sparkline values={[3, 0, 2]} label="the 5 runs in the window" width={96} height={20} />,
    );

    const drawn = bars(container);
    expect(drawn).toHaveLength(3);
    // Zero still draws, or the row would have holes that read as days the
    // window does not cover.
    expect(drawn[1].height).toBeGreaterThanOrEqual(1);
    expect(drawn[0].height).toBeGreaterThan(drawn[1].height);
  });

  it("draws a rail rather than a chart when there is not enough to plot", () => {
    const { container } = render(<Sparkline values={[4]} label="the 1 run in the window" />);

    expect(bars(container)).toHaveLength(0);
    expect(container.querySelector(".ui-spark-rail")).not.toBeNull();
  });

  it("keeps the label as the accessible name when it is hidden", () => {
    const { container } = render(
      <Sparkline values={[1, 2]} label="Finanças: the 2 runs in the window" labelHidden />,
    );

    const svg = container.querySelector("svg");
    expect(svg?.getAttribute("aria-label")).toBe("Finanças: the 2 runs in the window");
    // Off the screen, never out of the tree.
    expect(container.querySelector(".ui-spark-label-said")?.textContent).toBe(
      "Finanças: the 2 runs in the window",
    );
  });
});
