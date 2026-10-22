import { describe, expect, it } from "vitest";
import { render } from "@testing-library/react";
import { PageHeader } from "./PageHeader";

describe("PageHeader", () => {
  it("PageHeader keeps its own rule unless it is told not to", () => {
    const { container } = render(<PageHeader title="x" />);
    const header = container.querySelector("header") as HTMLElement;
    expect(header.classList.contains("ui-page-header")).toBe(true);
    expect(header.classList.contains("ui-page-header-unruled")).toBe(false);
  });

  it("PageHeader marks itself unruled when ruled is false", () => {
    const { container } = render(<PageHeader title="x" ruled={false} />);
    const header = container.querySelector("header") as HTMLElement;
    expect(header.classList.contains("ui-page-header")).toBe(true);
    expect(header.classList.contains("ui-page-header-unruled")).toBe(true);
  });
});
