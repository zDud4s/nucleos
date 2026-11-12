import { describe, expect, it, vi } from "vitest";
import { screen, within } from "@testing-library/react";
import { Crumb } from "./Crumb";
import { renderWithRouter } from "../test/harness";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

/**
 * The one thing a crumb must not do is read the arrow out loud.
 *
 * Three surfaces had grown three glyphs — `←`, `·` and `‹` — and each of them was
 * inside its link, so a screen reader announced "left arrow Teams" or "single left
 * angle quote back to nucleos". The glyph is `aria-hidden` now, and this is the test
 * that says so: the accessible name is the words, exactly, with nothing before them.
 */
describe("Crumb", () => {
  it("a crumb is one link behind one silent glyph", async () => {
    const { container } = await renderWithRouter(
      <Crumb to="/teams" here="Team run">
        Teams
      </Crumb>,
    );

    // Exactly `Teams` — not `← Teams`, which is what an unhidden glyph would
    // fold into the name.
    const link = screen.getByRole("link", { name: "Teams" });
    expect(link.getAttribute("href")).toBe("/teams");

    // Drawn, and drawn inside the link, so the arrow sits where it is aimed.
    const mark = container.querySelector(".ui-crumb-mark");
    expect(mark?.textContent).toBe("←");
    expect(mark?.getAttribute("aria-hidden")).toBe("true");
    expect(link.contains(mark)).toBe(true);

    // The page's own kind. Present, and not a second door: a reader who is
    // already here does not need a link to here.
    const crumb = container.querySelector(".ui-crumb") as HTMLElement;
    expect(within(crumb).getByText("Team run")).toBeDefined();
    expect(within(crumb).getAllByRole("link")).toHaveLength(1);
  });

  it("says nothing after the link when the page has no kind to add", async () => {
    const { container } = await renderWithRouter(<Crumb to="/teams">Teams</Crumb>);

    expect(container.querySelector(".ui-crumb-here")).toBeNull();
    expect(screen.getByRole("link", { name: "Teams" }).getAttribute("href")).toBe("/teams");
  });
});
