import { describe, expect, it } from "vitest";
import { render, screen } from "@testing-library/react";

import { renderWithRouter } from "../test/harness";
import { StatCard } from "./StatCard";

describe("StatCard", () => {
  it("an unread value is a hidden dash plus 'not read' text and shows the unread detail", () => {
    const { container } = render(<StatCard label="Waiting" value={undefined} unread="the daemon did not answer" />);

    const dash = container.querySelector('.ui-stat-value [aria-hidden="true"]');
    expect(dash?.textContent).toBe("—");
    expect(container.querySelector(".ui-stat-value .sr-only")?.textContent).toBe("not read");
    expect(screen.getByText("the daemon did not answer")).toBeDefined();
  });

  it("a card with `to` is a link named by its label", async () => {
    const { container } = await renderWithRouter(<StatCard label="Waiting" value={3} to="/fleet" />);

    const link = screen.getByRole("link", { name: "Waiting" });
    expect(link.getAttribute("href")).toBe("/fleet");
    expect(link.classList).toContain("ui-stat-link");
    expect(container.querySelector("article.ui-stat")).not.toBeNull();
  });

  it("is named by its visible label through aria-labelledby, never by an aria-label", async () => {
    const { container } = render(<StatCard label="Waiting" value={3} detail="since noon" />);

    expect(container.querySelector("[aria-label]")).toBeNull();
    const card = screen.getByRole("article", { name: "Waiting" });
    const labelId = card.getAttribute("aria-labelledby");
    expect(document.getElementById(labelId ?? "")?.textContent).toBe("Waiting");
    // The figure is read as the card's description, so the name stays the label alone.
    const describedBy = (card.getAttribute("aria-describedby") ?? "").split(" ");
    expect(describedBy.map((id) => document.getElementById(id)?.textContent).join(" ")).toContain("3");

    const linked = await renderWithRouter(<StatCard label="Failed" value={1} to="/runs" />);
    expect(linked.container.querySelector("[aria-label]")).toBeNull();
    expect(screen.getByRole("link", { name: "Failed" }).getAttribute("aria-labelledby")).not.toBeNull();
  });
});
