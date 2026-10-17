import { describe, expect, it } from "vitest";
import { fireEvent, render } from "@testing-library/react";

import { AreaChart, areaTicks, dayLabelIndices, type AreaChartProps } from "./AreaChart";
import type { ChartDatum, ChartKey } from "./chart-series";

/**
 * happy-dom lays nothing out and has no `ResizeObserver`, so every chart here is drawn at the
 * fallback width (640): x positions run from 6 to 634 and `clientX` maps straight onto them.
 */

const KEYS: ChartKey[] = [
  { key: "alpha", label: "alpha" },
  { key: "beta", label: "beta" },
  { key: "estimate", label: "Estimated", className: "ui-chart-estimated" },
];

const DATA: ChartDatum[] = [
  { day: "2026-10-15", parts: [{ key: "alpha", value: 1 }, { key: "beta", value: 2 }] },
  { day: "2026-10-16", parts: [{ key: "alpha", value: 4 }, { key: "estimate", value: 50 }] },
  { day: "2026-10-17", parts: [{ key: "beta", value: 1 }], partial: true },
];

const usd = (value: number) => `$${value.toFixed(2)}`;

function draw(props: Partial<AreaChartProps> = {}) {
  return render(
    <AreaChart
      data={DATA}
      keys={KEYS}
      label="Spend per day, last 3 days"
      format={usd}
      empty="No spend in this window"
      tone="ui-chart-cost"
      apart="estimate"
      {...props}
    />,
  );
}

/** Each tooltip row as "label value". */
function tipRows(container: HTMLElement): string[] {
  return [...container.querySelectorAll(".ui-area-tip-row")].map(
    (row) => `${row.querySelector(".ui-area-tip-label")?.textContent} ${row.querySelector(".ui-area-tip-value")?.textContent}`,
  );
}

describe("AreaChart", () => {
  it("names the chart with its label", () => {
    const { container } = draw();
    const svg = container.querySelector("svg");
    expect(svg?.getAttribute("role")).toBe("img");
    expect(svg?.getAttribute("aria-label")).toBe("Spend per day, last 3 days");
  });

  it("draws at its measured size: the viewBox is the width by the height, so text never scales", () => {
    const { container } = draw();
    expect(container.querySelector("svg")?.getAttribute("viewBox")).toBe("0 0 640 200");
  });

  it("draws one area and one line, coloured by the tone class", () => {
    const { container } = draw();
    expect(container.querySelector("path.ui-area-fill")?.getAttribute("d")).toBeTruthy();
    expect(container.querySelector("path.ui-area-line:not(.ui-area-partial)")?.getAttribute("d")).toBeTruthy();
    expect(container.querySelector("svg")?.getAttribute("class")).toContain("ui-chart-cost");
  });

  it("fills with a gradient whose id is unique per instance and safe inside url()", () => {
    const { container } = render(
      <>
        <AreaChart data={DATA} keys={KEYS} label="one" format={usd} empty="-" tone="ui-chart-cost" />
        <AreaChart data={DATA} keys={KEYS} label="two" format={usd} empty="-" tone="ui-chart-runs" />
      </>,
    );
    const gradients = [...container.querySelectorAll("linearGradient")];
    expect(gradients).toHaveLength(2);
    const ids = gradients.map((gradient) => gradient.id);
    expect(new Set(ids).size).toBe(2);
    for (const id of ids) expect(id).toMatch(/^[a-zA-Z0-9_-]+$/);
    const fills = [...container.querySelectorAll("path.ui-area-fill")].map((path) => path.getAttribute("fill"));
    expect(fills).toEqual(ids.map((id) => `url(#${id})`));
    expect(gradients[1].getAttribute("class")).toContain("ui-chart-runs");
  });

  it("dashes the segment into a partial day and labels it today", () => {
    const { container } = draw();
    const dashed = container.querySelector("path.ui-area-partial");
    expect(dashed?.getAttribute("d")).toBeTruthy();
    const days = [...container.querySelectorAll("text.ui-area-day")].map((text) => text.textContent);
    expect(days[0]).toBe("10-15");
    expect(days[days.length - 1]).toBe("today");
  });

  it("draws no dashed segment when the last day is whole", () => {
    const { container } = draw({ data: DATA.map((datum) => ({ ...datum, partial: false })) });
    expect(container.querySelector("path.ui-area-partial")).toBeNull();
    const days = [...container.querySelectorAll("text.ui-area-day")].map((text) => text.textContent);
    expect(days[days.length - 1]).toBe("10-17");
  });

  it("never folds the apart key into the scale: the top gridline is the measured peak's", () => {
    const { container } = draw();
    // Measured peak is 4 (day two); with the 50 of estimate folded in, the top would be $50+.
    const ticks = [...container.querySelectorAll("text.ui-area-tick")].map((text) => text.textContent);
    expect(ticks[ticks.length - 1]).toBe("$4.00");
  });

  it("shows the hovered day's breakdown, a Total, and the apart key on its own row", () => {
    const { container } = draw();
    const overlay = container.querySelector("rect.ui-area-overlay")!;
    expect(container.querySelector(".ui-area-tip")).toBeNull();

    fireEvent.pointerMove(overlay, { clientX: 10 });
    expect(container.querySelector(".ui-area-tip-head")?.textContent).toBe("2026-10-15");
    expect(tipRows(container)).toEqual(["alpha $1.00", "beta $2.00", "Total $3.00"]);

    // Day two: one measured key, so no Total — and the estimate apart, never summed in.
    fireEvent.pointerMove(overlay, { clientX: 320 });
    expect(tipRows(container)).toEqual(["alpha $4.00", "Estimated $50.00"]);
    const apart = container.querySelector(".ui-area-tip-apart .ui-area-tip-swatch");
    expect(apart?.getAttribute("class")).toContain("ui-chart-estimated");

    fireEvent.pointerMove(overlay, { clientX: 630 });
    expect(container.querySelector(".ui-area-tip-head")?.textContent).toBe("2026-10-17 · so far");

    fireEvent.pointerLeave(overlay);
    expect(container.querySelector(".ui-area-tip")).toBeNull();
  });

  it("colours each tooltip swatch with its key's series class", () => {
    const { container } = draw();
    fireEvent.pointerMove(container.querySelector("rect.ui-area-overlay")!, { clientX: 10 });
    const swatches = [...container.querySelectorAll(".ui-area-tip-part .ui-area-tip-swatch")].map((rect) =>
      rect.getAttribute("class"),
    );
    expect(swatches).toEqual(["ui-area-tip-swatch ui-chart-s1", "ui-area-tip-swatch ui-chart-s2"]);
  });

  it("reads day by day from the keyboard: focus shows the last day, arrows move, Escape hides", () => {
    const { container } = draw();
    const overlay = container.querySelector("rect.ui-area-overlay")!;
    expect(overlay.getAttribute("tabindex")).toBe("0");
    const head = () => container.querySelector(".ui-area-tip-head")?.textContent;

    fireEvent.focus(overlay);
    expect(head()).toBe("2026-10-17 · so far");
    fireEvent.keyDown(overlay, { key: "ArrowLeft" });
    expect(head()).toBe("2026-10-16");
    fireEvent.keyDown(overlay, { key: "Home" });
    expect(head()).toBe("2026-10-15");
    fireEvent.keyDown(overlay, { key: "ArrowLeft" });
    expect(head()).toBe("2026-10-15");
    fireEvent.keyDown(overlay, { key: "End" });
    expect(head()).toBe("2026-10-17 · so far");
    fireEvent.keyDown(overlay, { key: "Escape" });
    expect(container.querySelector(".ui-area-tip")).toBeNull();
  });

  it("says the shown day to a screen reader through a live region", () => {
    const { container } = draw();
    fireEvent.pointerMove(container.querySelector("rect.ui-area-overlay")!, { clientX: 10 });
    expect(container.querySelector("[aria-live]")?.textContent).toBe("2026-10-15: alpha $1.00, beta $2.00, Total $3.00");
  });

  it("in compact mode draws only the curve: no labels, no grid, no tooltip, and hidden from AT", () => {
    const { container } = draw({ compact: true });
    const svg = container.querySelector("svg")!;
    expect(svg.getAttribute("aria-hidden")).toBe("true");
    expect(svg.getAttribute("role")).toBeNull();
    expect(svg.getAttribute("viewBox")).toBe("0 0 640 40");
    expect(container.querySelector("path.ui-area-fill")).not.toBeNull();
    expect(container.querySelector("circle.ui-area-end")).not.toBeNull();
    expect(container.querySelector("text")).toBeNull();
    expect(container.querySelector(".ui-area-grid")).toBeNull();
    expect(container.querySelector(".ui-area-overlay")).toBeNull();
    expect(container.querySelector("[aria-live]")).toBeNull();
  });

  it("keeps the grid and says why when every total is zero", () => {
    const { container, getByText } = draw({
      data: DATA.map((datum) => ({ ...datum, parts: [{ key: "estimate", value: 3 }] })),
    });
    expect(getByText("No spend in this window").className).toBe("ui-chart-empty");
    expect(container.querySelectorAll("line.ui-area-grid").length).toBeGreaterThan(1);
    expect(container.querySelector("path.ui-area-fill")).toBeNull();
    expect(container.querySelector("rect.ui-area-overlay")).toBeNull();
  });

  it("never writes an inline style, hovered or not", () => {
    const { container } = draw();
    expect(container.querySelector("[style]")).toBeNull();
    fireEvent.pointerMove(container.querySelector("rect.ui-area-overlay")!, { clientX: 630 });
    expect(container.querySelector(".ui-area-tip")).not.toBeNull();
    expect(container.querySelector("[style]")).toBeNull();
  });

  it("keeps the tooltip inside the chart near the right edge", () => {
    const { container } = draw();
    fireEvent.pointerMove(container.querySelector("rect.ui-area-overlay")!, { clientX: 630 });
    const transform = container.querySelector(".ui-area-tip")?.getAttribute("transform") ?? "";
    const [, left] = /translate\(([-\d.]+)/.exec(transform) ?? [];
    const width = Number(container.querySelector(".ui-area-tip-bg")?.getAttribute("width"));
    expect(Number(left) + width).toBeLessThanOrEqual(640);
    expect(Number(left)).toBeGreaterThanOrEqual(0);
  });
});

describe("areaTicks", () => {
  it("steps by 1, 2 or 5 to a round top", () => {
    expect(areaTicks(6, false)).toEqual([0, 2, 4, 6]);
    expect(areaTicks(5, false)).toEqual([0, 2, 4, 6]);
    expect(areaTicks(0.37, false)).toEqual([0, 0.2, 0.4]);
    expect(areaTicks(0, false)).toEqual([0, 0.5, 1]);
  });

  it("never steps by a fraction when every value is whole", () => {
    expect(areaTicks(1, true)).toEqual([0, 1]);
    expect(areaTicks(3, true)).toEqual([0, 1, 2, 3]);
  });
});

describe("dayLabelIndices", () => {
  it("labels every day of a short window, and about six of a long one, always the last", () => {
    expect(dayLabelIndices(1)).toEqual([0]);
    expect(dayLabelIndices(3)).toEqual([0, 1, 2]);
    expect(dayLabelIndices(7)).toEqual([0, 2, 4, 6]);
    expect(dayLabelIndices(30)).toEqual([0, 6, 12, 18, 24, 29]);
    expect(dayLabelIndices(90)).toEqual([0, 18, 36, 54, 72, 89]);
  });

  it("drops the label that would crowd the last one", () => {
    expect(dayLabelIndices(8)).toEqual([0, 2, 4, 7]);
  });
});
