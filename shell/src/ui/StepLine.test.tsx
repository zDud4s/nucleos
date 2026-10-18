import { describe, expect, it } from "vitest";
import { fireEvent, render, within } from "@testing-library/react";

import { StepLine, type StepSeries } from "./StepLine";

/**
 * happy-dom has no `ResizeObserver` and lays nothing out, so the chart draws at `useWidth`'s
 * fallback width (640) and `getBoundingClientRect` answers zeros: x positions run from 6 to 634 over
 * `FROM`–`TO`, and `clientX` maps straight onto them.
 */
const FROM = Date.parse("2026-10-10T00:00:00Z");
const TO = Date.parse("2026-10-17T00:00:00Z");
const DAY = 24 * 3600 * 1000;
const LEFT = 6;
const RIGHT = 634;
const xOf = (at: number) => LEFT + ((at - FROM) / (TO - FROM)) * (RIGHT - LEFT);
const pct = (value: number) => `${Math.round(value * 100)}%`;

const SERIES: StepSeries[] = [
  {
    key: "anthropic:5h",
    label: "anthropic 5h",
    points: [
      { at: Date.parse("2026-10-11T08:00:00Z"), value: 0.2 },
      { at: Date.parse("2026-10-12T08:00:00Z"), value: 0.6, stale: true },
    ],
  },
  { key: "anthropic:7d", label: "anthropic 7d", points: [{ at: Date.parse("2026-10-13T00:00:00Z"), value: 0.41 }] },
];

function draw(series: StepSeries[] = SERIES) {
  return render(
    <StepLine series={series} from={FROM} to={TO} label="Quota used, last 7 days" format={pct} empty="No quota history yet" />,
  );
}

/** The absolute `x,y` pairs of a path made of M/L commands (what a step curve emits). */
function pathPoints(d: string): Array<[number, number]> {
  const nums = (d.match(/-?\d+(?:\.\d+)?(?:e-?\d+)?/g) ?? []).map(Number);
  const out: Array<[number, number]> = [];
  for (let i = 0; i + 1 < nums.length; i += 2) out.push([nums[i], nums[i + 1]]);
  return out;
}

/** What the live region says: the tooltip, in words. */
function said(container: HTMLElement): string {
  return container.querySelector("p.sr-only")?.textContent ?? "";
}

describe("StepLine", () => {
  it("names the chart and draws one stepped path per series with points", () => {
    const { container } = draw();
    expect(container.querySelector("svg")?.getAttribute("aria-label")).toBe("Quota used, last 7 days");
    const paths = [...container.querySelectorAll("path.ui-step-line")];
    expect(paths).toHaveLength(2);
    // Steps, not slopes: a step-after path only ever moves horizontally or vertically.
    const coords = pathPoints(paths[0].getAttribute("d") ?? "");
    expect(coords.length).toBeGreaterThan(2);
    for (let i = 1; i < coords.length; i += 1) {
      const [px, py] = coords[i - 1];
      const [cx, cy] = coords[i];
      expect(px === cx || py === cy, `segment ${i} is neither horizontal nor vertical`).toBe(true);
    }
  });

  it("fills under the first series only, and dashes the others", () => {
    const { container } = draw();
    expect(container.querySelectorAll("path.ui-step-fill")).toHaveLength(1);
    const [first, second] = [...container.querySelectorAll("path.ui-step-line")];
    expect(first.classList.contains("ui-step-dashed")).toBe(false);
    expect(second.classList.contains("ui-step-dashed")).toBe(true);
    // The context line is muted; the subject keeps a colour of its own.
    expect(second.closest("g")?.classList.contains("ui-chart-muted")).toBe(true);
    expect(first.closest("g")?.classList.contains("ui-chart-s1")).toBe(true);
  });

  it("draws a stale reading as a hollow dot", () => {
    const { container } = draw();
    expect(container.querySelectorAll("circle.ui-step-dot")).toHaveLength(3);
    expect(container.querySelectorAll("circle.ui-step-stale")).toHaveLength(1);
  });

  it("draws gridlines at 0, 50 and 100 %, labels above all but 0, and no axis", () => {
    const { container } = draw();
    expect(container.querySelectorAll("line.ui-area-grid")).toHaveLength(3);
    expect([...container.querySelectorAll("text.ui-area-tick")].map((node) => node.textContent)).toEqual(["50%", "100%"]);
    expect(container.querySelector(".visx-axis, .ui-chart-axis")).toBeNull();
  });

  it("labels at most six days, the first at the start and the last, today, at the end", () => {
    const { container } = draw();
    const days = [...container.querySelectorAll("text.ui-area-day")];
    expect(days.length).toBeGreaterThan(1);
    expect(days.length).toBeLessThanOrEqual(6);
    expect(days[0].textContent).toBe("10-10");
    expect(days[0].getAttribute("text-anchor")).toBe("start");
    expect(days[days.length - 1].textContent).toBe("today");
    expect(days[days.length - 1].getAttribute("text-anchor")).toBe("end");
  });

  it("writes no style, at rest or hovered", () => {
    const { container } = draw();
    expect(container.querySelectorAll("[style]")).toHaveLength(0);
    expect(container.innerHTML).not.toMatch(/style=/);
    expect(container.querySelector("style")).toBeNull();
    fireEvent.pointerMove(container.querySelector("rect.ui-area-overlay")!, { clientX: 300 });
    expect(container.querySelector(".ui-area-tip")).not.toBeNull();
    expect(container.querySelectorAll("[style]")).toHaveLength(0);
  });

  it("snaps the crosshair to the nearest reading and lists what every series holds then", () => {
    const { container } = draw();
    const overlay = container.querySelector("rect.ui-area-overlay")!;
    const stale = Date.parse("2026-10-12T08:00:00Z");

    // A little past the stale reading: the crosshair snaps back to it, and the 7d has not read yet.
    fireEvent.pointerMove(overlay, { clientX: xOf(stale) + 10 });
    expect(Number(container.querySelector(".ui-area-crosshair")?.getAttribute("x1"))).toBeCloseTo(xOf(stale));
    expect(said(container)).toBe("10-12 08:00 UTC: anthropic 5h 60% (stale)");
    const tip = container.querySelector(".ui-area-tip") as HTMLElement;
    expect(within(tip).getByText("10-12 08:00 UTC")).toBeTruthy();

    // At the far right, both hold: the 5h its stale reading, the 7d its only one.
    fireEvent.pointerMove(overlay, { clientX: 630 });
    expect(said(container)).toBe("10-13 00:00 UTC: anthropic 5h 60% (stale), anthropic 7d 41%");
    expect(container.querySelectorAll(".ui-area-dot")).toHaveLength(2);
    expect(container.querySelector(".ui-area-tip-dash")).not.toBeNull();

    fireEvent.pointerLeave(overlay);
    expect(container.querySelector(".ui-area-tip")).toBeNull();
  });

  it("steps through the readings from the keyboard", () => {
    const { container } = draw();
    const overlay = container.querySelector("rect.ui-area-overlay")!;
    expect(overlay.getAttribute("tabindex")).toBe("0");
    fireEvent.focus(overlay);
    expect(said(container)).toMatch(/^10-13 00:00 UTC/);
    fireEvent.keyDown(overlay, { key: "ArrowLeft" });
    expect(said(container)).toMatch(/^10-12 08:00 UTC/);
    fireEvent.keyDown(overlay, { key: "Home" });
    expect(said(container)).toBe("10-11 08:00 UTC: anthropic 5h 20%");
    fireEvent.keyDown(overlay, { key: "Escape" });
    expect(container.querySelector(".ui-area-tip")).toBeNull();
  });

  it("keeps the grid and the days and says why when there is no point at all", () => {
    const { container, getByText } = draw([{ key: "anthropic:5h", label: "anthropic 5h", points: [] }]);
    expect(container.querySelectorAll("path.ui-step-line")).toHaveLength(0);
    expect(container.querySelectorAll("line.ui-area-grid")).toHaveLength(3);
    expect(container.querySelectorAll("text.ui-area-day").length).toBeGreaterThan(0);
    expect(container.querySelector("rect.ui-area-overlay")).toBeNull();
    expect(getByText("No quota history yet")).toBeTruthy();
  });

  it("respects the domain: a reading before it starts at the left edge, nothing after it is drawn", () => {
    const { container } = draw([
      {
        key: "anthropic:5h",
        label: "anthropic 5h",
        points: [
          { at: FROM - 3 * DAY, value: 0.9 },
          { at: FROM - 2 * DAY, value: 0.3 },
          { at: FROM + DAY, value: 1.4 },
          { at: TO + DAY, value: 0.5 },
        ],
      },
    ]);
    const coords = pathPoints(container.querySelector("path.ui-step-line")?.getAttribute("d") ?? "");
    // Only the last reading before the window holds into it; the one before that never shows.
    expect(coords[0][0]).toBeCloseTo(LEFT);
    expect(coords[0][1]).toBeGreaterThan(coords[coords.length - 1][1]);
    expect(Math.max(...coords.map(([px]) => px))).toBeCloseTo(RIGHT);
    for (const [px, py] of coords) {
      expect(px).toBeGreaterThanOrEqual(LEFT - 0.001);
      expect(px).toBeLessThanOrEqual(RIGHT + 0.001);
      expect(py).toBeGreaterThanOrEqual(16 - 0.001);
      expect(py).toBeLessThanOrEqual(160 - 22 + 0.001);
    }
    // Only the in-window reading is a dot, where its time falls; its value is clamped to the top.
    const dots = [...container.querySelectorAll("circle.ui-step-dot")];
    expect(dots).toHaveLength(1);
    expect(Number(dots[0].getAttribute("cx"))).toBeCloseTo(xOf(FROM + DAY));
    expect(Number(dots[0].getAttribute("cy"))).toBeCloseTo(16);
  });

  it("keys the series under the chart: a solid swatch for the first, a dashed one for the rest", () => {
    const { getByRole } = draw();
    const legend = getByRole("list", { name: "Quota used, last 7 days, series" });
    const items = within(legend).getAllByRole("listitem");
    expect(items.map((item) => item.querySelector(".ui-share-label")?.textContent)).toEqual(["anthropic 5h", "anthropic 7d"]);
    expect(items[0].querySelector(".ui-chart-swatch.ui-chart-s1")).not.toBeNull();
    expect(items[1].querySelector(".ui-step-key-dash.ui-chart-muted")).not.toBeNull();
    expect(items[0].querySelector(".ui-share-value")?.textContent).toBe("60% latest, stale");
    expect(items[1].querySelector(".ui-share-value")?.textContent).toBe("41% latest");
  });
});
