import { describe, expect, it } from "vitest";
import { render } from "@testing-library/react";

import { BarChart, seriesClass, stackDay, type BarDatum, type BarKey } from "./BarChart";

/**
 * The rules the spec (§9.10) inherits from `Sparkline`: a mandatory accessible name, a `<title>`
 * per bar and per segment, no inline style anywhere, and an empty window that still draws axes.
 */

const KEYS: BarKey[] = [
  { key: "alpha", label: "alpha" },
  { key: "beta", label: "beta" },
  { key: "judge", label: "Judge", className: "ui-chart-judge" },
];

const DATA: BarDatum[] = [
  { day: "2026-10-15", parts: [{ key: "alpha", value: 1 }, { key: "beta", value: 2 }] },
  { day: "2026-10-16", parts: [{ key: "alpha", value: 0 }, { key: "judge", value: 0.5 }] },
  { day: "2026-10-17", parts: [{ key: "beta", value: 1 }], partial: true },
];

const usd = (value: number) => `$${value.toFixed(2)}`;

function draw(data: BarDatum[] = DATA) {
  return render(
    <BarChart data={data} keys={KEYS} label="Cost per day, last 3 days" format={usd} empty="No runs in this window" />,
  );
}

describe("BarChart", () => {
  it("names the chart with its label", () => {
    const { container } = draw();
    expect(container.querySelector("svg")?.getAttribute("aria-label")).toBe("Cost per day, last 3 days");
    expect(container.querySelector("svg")?.getAttribute("role")).toBe("img");
  });

  it("gives every day a title and every drawn segment a title of its own", () => {
    const { container } = draw();

    const days = [...container.querySelectorAll("g.ui-bar-day")];
    expect(days).toHaveLength(3);
    for (const day of days) expect(day.firstElementChild?.tagName.toLowerCase()).toBe("title");
    expect(days[0].firstElementChild?.textContent).toBe("2026-10-15: $3.00");

    // A zero part draws nothing, so it has nothing to title: 2 + 1 + 1 segments.
    const segments = [...container.querySelectorAll("rect.ui-bar")];
    expect(segments).toHaveLength(4);
    for (const rect of segments) {
      const group = rect.parentElement;
      expect(group?.firstElementChild?.tagName.toLowerCase()).toBe("title");
    }
    const said = segments.map((rect) => rect.parentElement?.firstElementChild?.textContent);
    expect(said).toContain("2026-10-16 · Judge: $0.50");
  });

  it("marks the day in progress, in its class and in its title", () => {
    const { container } = draw();
    const days = [...container.querySelectorAll("g.ui-bar-day")];
    expect(days[2].getAttribute("class")).toContain("ui-bar-partial");
    expect(days[2].firstElementChild?.textContent).toContain("day in progress");
    expect(days[0].getAttribute("class")).not.toContain("ui-bar-partial");
  });

  it("colours by class, never inline, and never builds a stylesheet", () => {
    const { container } = draw();
    expect(container.innerHTML).not.toMatch(/style=/);
    expect(container.querySelector("style")).toBeNull();

    const judge = [...container.querySelectorAll("rect.ui-bar")].find((rect) =>
      rect.getAttribute("class")?.includes("ui-chart-judge"),
    );
    expect(judge).toBeDefined();
    expect(container.querySelector("rect.ui-chart-s1")).not.toBeNull();
    expect(container.querySelector("li .ui-chart-swatch.ui-chart-judge")).not.toBeNull();
  });

  it("stacks in key order, the first key at the bottom", () => {
    const { container } = draw();
    const first = [...container.querySelectorAll("g.ui-bar-day")][0];
    const [alpha, beta] = [...first.querySelectorAll("rect.ui-bar")];
    expect(alpha.getAttribute("class")).toContain("ui-chart-s1");
    expect(beta.getAttribute("class")).toContain("ui-chart-s2");
    // SVG y grows downwards: the bottom segment starts lower on screen.
    expect(Number(alpha.getAttribute("y"))).toBeGreaterThan(Number(beta.getAttribute("y")));
  });

  it("draws the axes and says why when the window is empty", () => {
    const empty: BarDatum[] = [
      { day: "2026-10-16", parts: [] },
      { day: "2026-10-17", parts: [{ key: "alpha", value: 0 }], partial: true },
    ];
    const { container, getByText } = draw(empty);
    expect(container.querySelectorAll("rect.ui-bar")).toHaveLength(0);
    expect(container.querySelectorAll(".ui-chart-axis").length).toBeGreaterThanOrEqual(2);
    expect(getByText("No runs in this window")).toBeTruthy();
    expect(container.innerHTML).not.toMatch(/style=/);
  });
});

describe("stackDay", () => {
  it("accumulates positive parts in key order and drops zeros", () => {
    expect(
      stackDay(
        [
          { key: "beta", value: 2 },
          { key: "alpha", value: 1 },
          { key: "judge", value: 0 },
        ],
        KEYS,
      ),
    ).toEqual([
      { key: "alpha", value: 1, from: 0, to: 1 },
      { key: "beta", value: 2, from: 1, to: 3 },
    ]);
  });
});

describe("seriesClass", () => {
  it("uses a key's own class, else cycles the eight categorical ones", () => {
    expect(seriesClass(KEYS, "judge")).toBe("ui-chart-judge");
    expect(seriesClass(KEYS, "beta")).toBe("ui-chart-s2");
    const many = Array.from({ length: 10 }, (_, i) => ({ key: `k${i}`, label: `k${i}` }));
    expect(seriesClass(many, "k8")).toBe("ui-chart-s1");
    // A key the legend does not know is "other", never series 1's colour.
    expect(seriesClass(KEYS, "stranger")).toBe("ui-chart-other");
  });
});
