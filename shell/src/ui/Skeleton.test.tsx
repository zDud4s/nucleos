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

  it("draws card blocks for the cards variant, hidden from assistive tech", () => {
    render(<Skeleton label="Loading cards" variant="cards" count={4} />);
    const el = screen.getByRole("status");
    expect(el.getAttribute("aria-busy")).toBe("true");
    expect(el.textContent).toContain("Loading cards");
    expect(el.className).toContain("ui-skeleton-cards");
    expect(el.querySelectorAll(".ui-skeleton-card").length).toBe(4);
    expect(el.querySelector(".ui-skeleton-grid")?.getAttribute("aria-hidden")).toBe("true");
  });

  it("defaults the cards variant to three blocks", () => {
    render(<Skeleton label="Loading" variant="cards" />);
    expect(screen.getByRole("status").querySelectorAll(".ui-skeleton-card").length).toBe(3);
  });
});
