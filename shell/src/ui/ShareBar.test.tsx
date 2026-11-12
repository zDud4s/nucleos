import { describe, expect, it } from "vitest";
import { render } from "@testing-library/react";

import { ShareBar, type ShareItem } from "./ShareBar";

const usd = (value: number) => `$${value.toFixed(2)}`;

const ITEMS: ShareItem[] = [
  { key: "alpha", label: "alpha", value: 3 },
  { key: "beta", label: "beta", value: 0 },
  { key: "gamma", label: "gamma", value: 1 },
  { key: "judge", label: "Judge", value: 0.001, className: "ui-chart-judge" },
];

describe("ShareBar", () => {
  it("names the bar and splits it in proportion, gaps taken out first", () => {
    const { container } = render(<ShareBar items={ITEMS.slice(0, 3)} format={usd} label="Spend by project" />);
    const svg = container.querySelector("svg")!;
    expect(svg.getAttribute("role")).toBe("img");
    expect(svg.getAttribute("aria-label")).toBe("Spend by project");
    expect(svg.getAttribute("viewBox")).toBe("0 0 1000 8");
    expect(svg.getAttribute("preserveAspectRatio")).toBe("none");

    const rects = [...container.querySelectorAll("rect.ui-share-seg")];
    expect(rects).toHaveLength(2);
    // 998 units after one 2-unit gap: 3/4 and 1/4 of it.
    expect(Number(rects[0].getAttribute("width"))).toBeCloseTo(748.5);
    expect(Number(rects[1].getAttribute("x"))).toBeCloseTo(750.5);
    expect(Number(rects[1].getAttribute("width"))).toBeCloseTo(249.5);
  });

  it("colours items by position over all of them, as the chart's keys do", () => {
    const { container } = render(<ShareBar items={ITEMS} format={usd} label="Spend by project" />);
    const classes = [...container.querySelectorAll("rect.ui-share-seg")].map((rect) => rect.getAttribute("class"));
    // beta is zero and omitted, but gamma keeps the third colour, not the second.
    expect(classes).toEqual(["ui-share-seg ui-chart-s1", "ui-share-seg ui-chart-s3", "ui-share-seg ui-chart-judge"]);
  });

  it("lists every non-zero item with its value and rounded percent", () => {
    const { container } = render(<ShareBar items={ITEMS} format={usd} label="Spend by project" />);
    const keys = [...container.querySelectorAll(".ui-share-keys li")];
    expect(keys.map((key) => key.querySelector(".ui-share-label")?.textContent)).toEqual(["alpha", "gamma", "Judge"]);
    expect(keys.map((key) => key.querySelector(".ui-share-pct")?.textContent)).toEqual(["75%", "25%", "<1%"]);
    expect(keys[0].querySelector(".ui-share-value")?.textContent).toBe("$3.00 75%");
  });

  it("renders nothing when the total is zero", () => {
    const { container } = render(
      <ShareBar items={[{ key: "a", label: "a", value: 0 }]} format={usd} label="Spend by project" />,
    );
    expect(container.innerHTML).toBe("");
    const none = render(<ShareBar items={[]} format={usd} label="Spend by project" />);
    expect(none.container.innerHTML).toBe("");
  });

  it("never writes an inline style", () => {
    const { container } = render(<ShareBar items={ITEMS} format={usd} label="Spend by project" />);
    expect(container.querySelector("[style]")).toBeNull();
  });
});
