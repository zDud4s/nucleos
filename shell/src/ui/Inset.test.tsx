import { describe, expect, it } from "vitest";
import { render, screen } from "@testing-library/react";
import { Inset } from "./Inset";

describe("Inset", () => {
  it("is a plain box by default, so it is never invalid markup by default", () => {
    const { container } = render(<Inset>a group within this panel</Inset>);

    const box = container.firstElementChild;
    expect(box?.tagName).toBe("DIV");
    expect(screen.getByText("a group within this panel")).toBeTruthy();
  });

  it("is a list item when the page has already opened the list", () => {
    // Nineteen of the thirty sites are an `li`. A box that rendered a `div`
    // inside a `ul` would look identical and stop being an item to anyone
    // listening rather than looking.
    render(
      <ul aria-label="Proposals">
        <Inset as="li">waiting on you</Inset>
      </ul>,
    );

    const items = screen.getByRole("list", { name: "Proposals" }).querySelectorAll("li");
    expect(items).toHaveLength(1);
    expect(items[0]?.textContent).toBe("waiting on you");
  });

  it("keeps the shared recipe when a page marks one box out from its siblings", () => {
    // `ap-row-selected` is the only page modifier in the app, and it adds a rule
    // rather than replacing the box. If the primitive's own class were dropped
    // in favour of the caller's, adoption would silently return that page to a
    // hand-written card.
    const { container } = render(<Inset className="ap-row-selected">the project</Inset>);

    const box = container.firstElementChild;
    expect(box?.classList.contains("ui-panel-inset")).toBe(true);
    expect(box?.classList.contains("ap-row-selected")).toBe(true);
  });

  it("puts nothing between itself and its content", () => {
    // It is one box carrying its own padding, not a head/body pair — a wrapper
    // in here would be the second rank the system says does not exist, and the
    // padding would be paid twice.
    const { container } = render(
      <Inset>
        <p>only child</p>
      </Inset>,
    );

    const box = container.firstElementChild;
    expect(box?.children).toHaveLength(1);
    expect(box?.firstElementChild?.tagName).toBe("P");
  });

  it("marks the one you are on with an edge, not a fill", () => {
    render(
      <Inset as="li" current>
        req #399
      </Inset>,
    );

    // Six pages reached for the brand colour here because the neutral answer did
    // not exist: the surface ladder has four rungs in dark and three in light,
    // where --surface and --surface-raised are both #ffffff. An edge crosses both.
    const box = screen.getByRole("listitem");
    expect(box.className).toContain("ui-panel-inset");
    expect(box.className).toContain("ui-current");
  });

  it("leaves the mark off when it is not the one", () => {
    render(<Inset as="li">req #402</Inset>);
    expect(screen.getByRole("listitem").className).not.toContain("ui-current");
  });

  it("stands on its own, with its own name, when it is an article", () => {
    render(
      <Inset as="article" label="slot 1 — job 41">
        job 41
      </Inset>,
    );

    // The fleet slot card could not adopt this component until `article` and a
    // name existed: fourteen tests grab the page by exactly that label.
    expect(screen.getByRole("article", { name: "slot 1 — job 41" })).toBeTruthy();
  });
});
