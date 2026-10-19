import { render } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { MARK_PATHS, Mark, markKey } from "./marks";

describe("markKey", () => {
  it("names a knowledge node by its layer and falls back to semantic", () => {
    expect(markKey({ kind: "knowledge", layer: "working" })).toBe("k-working");
    expect(markKey({ kind: "knowledge" })).toBe("k-semantic");
  });
  it("names any other node by its kind", () => {
    expect(markKey({ kind: "project" })).toBe("project");
  });
});

describe("MARK_PATHS", () => {
  it("gives every category its own silhouette", () => {
    const paths = Object.values(MARK_PATHS);
    expect(new Set(paths).size).toBe(paths.length);
  });
});

describe("Mark", () => {
  it("draws a shape for a category and a hollow dot for a capture", () => {
    const { container } = render(
      <>
        <Mark hue="k-episodic" />
        <Mark hue="capture" />
      </>,
    );
    expect(container.querySelector("svg.unified-mark-shape path")).not.toBeNull();
    expect(container.querySelector("span.unified-mark-capture")).not.toBeNull();
  });
});
