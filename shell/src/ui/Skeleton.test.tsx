import { describe, expect, it } from "vitest";
import { render, screen } from "@testing-library/react";
import { Skeleton } from "./Skeleton";

describe("Skeleton", () => {
  it("is a busy status region that names what is loading", () => {
    render(<Skeleton label="Loading teams" />);
    const el = screen.getByRole("status");
    expect(el.getAttribute("aria-busy")).toBe("true");
    expect(el.textContent).toContain("Loading teams");
  });

  it("reserves its height through a custom property, so the content does not jump", () => {
    render(<Skeleton label="Loading" lines={3} />);
    const el = screen.getByRole("status");
    expect(el.style.getPropertyValue("--skeleton-lines")).toBe("3");
    expect(el.className).toContain("ui-skeleton");
  });
});
