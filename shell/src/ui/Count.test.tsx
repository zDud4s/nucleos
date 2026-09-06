import { describe, expect, it } from "vitest";
import { render, screen } from "@testing-library/react";
import { Count } from "./Count";
import { Panel } from "./Panel";

describe("Count", () => {
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
