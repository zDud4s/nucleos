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

  /**
   * Zero is an answer; `undefined` is not one yet. A dash or a "0" printed while
   * the list is still in flight is a claim about a list that has not come back,
   * and on this app's poll interval it is the number a person sees first.
   */
  it("says nothing at all until the list has answered", () => {
    const { container } = render(<Count n={undefined} />);
    expect(container.innerHTML).toBe("");
  });

  it("shows a real zero", () => {
    render(<Count n={0} />);
    expect(screen.getByText("0").className).toBe("ui-count");
  });
});
