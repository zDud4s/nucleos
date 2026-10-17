import { describe, expect, it } from "vitest";
import { render } from "@testing-library/react";

import { StepLine, type StepSeries } from "./StepLine";

const FROM = Date.parse("2026-10-10T00:00:00Z");
const TO = Date.parse("2026-10-17T00:00:00Z");
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

  it("titles every point with its series, time and value, and marks a stale one", () => {
    const { container } = draw();
    const said = [...container.querySelectorAll("g.ui-step-point > title")].map((node) => node.textContent);
    expect(said).toEqual([
      "anthropic 5h · 2026-10-11 08:00 UTC: 20%",
      "anthropic 5h · 2026-10-12 08:00 UTC: 60% (stale)",
      "anthropic 7d · 2026-10-13 00:00 UTC: 41%",
    ]);
    expect(container.querySelectorAll("g.ui-step-stale")).toHaveLength(1);
  });

  it("writes no style, inline or as an element", () => {
    const { container } = draw();
    expect(container.innerHTML).not.toMatch(/style=/);
    expect(container.querySelector("style")).toBeNull();
  });

  it("keeps the axes and says why when there is no point at all", () => {
    const { container, getByText } = draw([{ key: "anthropic:5h", label: "anthropic 5h", points: [] }]);
    expect(container.querySelectorAll("path.ui-step-line")).toHaveLength(0);
    expect(container.querySelectorAll(".ui-chart-axis").length).toBeGreaterThanOrEqual(2);
    expect(getByText("No quota history yet")).toBeTruthy();
  });

  it("keeps every point and the held tail inside the plot", () => {
    const day = 24 * 3600 * 1000;
    const { container } = draw([
      {
        key: "anthropic:5h",
        label: "anthropic 5h",
        points: [
          { at: FROM - 2 * day, value: 0.3 },
          { at: FROM + day, value: 1.4 },
          { at: TO + day, value: 0.5 },
        ],
      },
    ]);
    const inner = { w: 640 - 44 - 8, h: 160 - 8 - 22 };
    const coords = pathPoints(container.querySelector("path.ui-step-line")?.getAttribute("d") ?? "");
    expect(coords.length).toBeGreaterThan(1);
    for (const [px, py] of coords) {
      expect(px).toBeGreaterThanOrEqual(-0.001);
      expect(px).toBeLessThanOrEqual(inner.w + 0.001);
      expect(py).toBeGreaterThanOrEqual(-0.001);
      expect(py).toBeLessThanOrEqual(inner.h + 0.001);
    }
    for (const dot of container.querySelectorAll("circle.ui-step-dot")) {
      expect(Number(dot.getAttribute("cx"))).toBeLessThanOrEqual(inner.w + 0.001);
      expect(Number(dot.getAttribute("cx"))).toBeGreaterThanOrEqual(-0.001);
      expect(Number(dot.getAttribute("cy"))).toBeLessThanOrEqual(inner.h + 0.001);
    }
  });
});
