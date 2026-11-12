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

describe("Skeleton shaped variants", () => {
  it.each(["line", "block", "card", "avatar", "row", "table", "cards"] as const)(
    "%s is a busy status region with a hidden shape and a label",
    (variant) => {
      render(<Skeleton label={`Loading ${variant}`} variant={variant} />);
      const el = screen.getByRole("status");
      expect(el.getAttribute("aria-busy")).toBe("true");
      expect(el.textContent).toContain(`Loading ${variant}`);
      expect(el.className).toContain(`ui-skeleton-${variant}`);
      expect(el.querySelector("[aria-hidden='true']")).not.toBeNull();
    },
  );

  it("draws count rows", () => {
    render(<Skeleton label="Loading" variant="row" count={4} />);
    expect(screen.getByRole("status").querySelectorAll(".ui-skeleton-row").length).toBe(4);
  });

  it("draws a header plus count table rows of `columns` cells", () => {
    render(<Skeleton label="Loading" variant="table" count={2} columns={4} />);
    const el = screen.getByRole("status");
    const rows = el.querySelectorAll(".ui-skeleton-tr");
    expect(rows.length).toBe(3);
    expect(rows[0]!.querySelectorAll(".ui-skeleton-bar").length).toBe(4);
  });

  it("sizes an avatar and a block", () => {
    const { container } = render(
      <>
        <Skeleton label="a" variant="avatar" size={48} />
        <Skeleton label="b" variant="block" height="10rem" width="50%" />
      </>,
    );
    expect((container.querySelector("span.ui-skeleton-avatar") as HTMLElement).style.width).toBe("48px");
    expect((container.querySelector(".ui-skeleton-blockbox") as HTMLElement).style.height).toBe("10rem");
  });
});
