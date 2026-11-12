import { describe, expect, it } from "vitest";
import { render, screen } from "@testing-library/react";
import { Count } from "./Count";
import { Panel } from "./Panel";

describe("Count", () => {
  /**
   * The one thing this primitive exists to settle.
   *
   * Every page in the app puts a number in a heading's corner, and four of them
   * had drawn it as a pill — the same shape `.ui-badge` gives a *state*, `.ui-who`
   * gives a specialist and `.ui-limit` used to give a ceiling. Four classes of
   * thing in one shape means none of them can be told from another without being
   * read, which is the opposite of what a badge is for. A count is a reading.
   */
  it("a count is bare text and never a pill", () => {
    const { container } = render(
      <Panel title="Wheel requests" aside={<Count n={7} />}>
        <p>a row</p>
      </Panel>,
    );

    const count = screen.getByText("7");
    expect(count.tagName).toBe("SPAN");
    expect(count.className).toBe("ui-count");

    // Not a badge, and not wearing one either: no pill anywhere around it.
    expect(container.querySelector(".ui-badge")).toBeNull();
    expect(count.closest(".ui-badge")).toBeNull();
    expect(count.closest(".ui-who")).toBeNull();
  });

  it("says nothing at all when nobody has managed to ask", () => {
    // Seven of the nine copies had written this line by hand. It is the opposite
    // of `StatCard`, where an unread figure becomes an em dash — there the
    // figure is the tile's whole reason to exist, here it qualifies a title that
    // says the same thing without it.
    const { container } = render(<Count n={undefined} />);
    expect(container.firstChild).toBeNull();
  });

  it("still reports an empty list", () => {
    // Zero is an answer. A heading that loses its count the moment the list
    // empties reads as a count that failed rather than as a list that is empty.
    render(<Count n={0} noun="proposal" />);
    expect(screen.getByText("0 proposals")).toBeTruthy();
  });

  it("is the bare figure when the heading beside it already says the noun", () => {
    render(<Count n={12} />);
    expect(screen.getByText("12")).toBeTruthy();
  });

  it("drops the s for exactly one", () => {
    // The inline ternary two pages were each maintaining on their own.
    render(<Count n={1} noun="hit" />);
    expect(screen.getByText("1 hit")).toBeTruthy();
  });

  it("adds the s for everything that is not one", () => {
    render(<Count n={3} noun="hit" />);
    expect(screen.getByText("3 hits")).toBeTruthy();
  });

  it("takes an irregular plural when -s would be wrong", () => {
    render(<Count n={4} noun="person" plural="people" />);
    expect(screen.getByText("4 people")).toBeTruthy();
  });

  it("takes a word that does not inflect at all", () => {
    // "untriaged" is an adjective standing in for a noun, and it is the same
    // word however many there are.
    render(<Count n={7} noun="untriaged" plural="untriaged" />);
    expect(screen.getByText("7 untriaged")).toBeTruthy();
  });

  it("sits in a panel's aside, which is where all nine copies sat", () => {
    render(
      <Panel title="Sessions" aside={<Count n={2} noun="session" />}>
        the list
      </Panel>,
    );

    const aside = document.querySelector(".ui-panel-aside");
    expect(aside?.textContent).toBe("2 sessions");
  });
});
