import { describe, expect, it } from "vitest";
import { render, screen } from "@testing-library/react";

import { renderWithRouter } from "../test/harness";
import { StatCard } from "./StatCard";

describe("StatCard", () => {
  it("an unread value is a hidden dash plus 'not read' text and shows the unread detail", () => {
    const { container } = render(<StatCard label="Waiting" value={undefined} unread="the service is unreachable" />);

    const dash = container.querySelector('.ui-stat-value [aria-hidden="true"]');
    expect(dash?.textContent).toBe("—");
    expect(container.querySelector(".ui-stat-value .sr-only")?.textContent).toBe("not read");
    expect(screen.getByText("the service is unreachable")).toBeDefined();
  });

  it("a card with `to` is a link named by its label and figure", async () => {
    const { container } = await renderWithRouter(<StatCard label="Waiting" value={3} to="/fleet" />);

    const link = screen.getByRole("link", { name: "Waiting 3" });
    expect(link.getAttribute("href")).toBe("/fleet");
    expect(link.classList).toContain("ui-stat-link");
    expect(container.querySelector("article.ui-stat")).not.toBeNull();
  });

  it("is named by label, figure and detail through aria-labelledby, never by an aria-label", async () => {
    const { container } = render(<StatCard label="Waiting" value={3} detail="since noon" />);

    expect(container.querySelector("[aria-label]")).toBeNull();
    const card = screen.getByRole("article", { name: "Waiting 3 since noon" });
    // The figure and the detail are part of the name, so neither is repeated as a description.
    expect(card.getAttribute("aria-describedby")).toBeNull();
    const ids = (card.getAttribute("aria-labelledby") ?? "").split(" ");
    expect(ids).toHaveLength(3);
    expect(document.getElementById(ids[0])?.textContent).toBe("Waiting");

    const linked = await renderWithRouter(<StatCard label="Failed" value={1} to="/runs" />);
    expect(linked.container.querySelector("[aria-label]")).toBeNull();
    expect(screen.getByRole("link", { name: "Failed 1" }).getAttribute("aria-labelledby")).not.toBeNull();
  });
});
