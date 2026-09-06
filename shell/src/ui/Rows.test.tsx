import { describe, expect, it } from "vitest";
import { render, screen } from "@testing-library/react";
import { Row, Rows } from "./Rows";

describe("Rows", () => {
  it("is a list, and answers to the name of what it lists", () => {
    // The hairlines are the only thing saying where this column begins and ends,
    // and a rule is not announced. Take the name away and a screen reader gets
    // "list, 2 items" with no answer to "of what".
    render(
      <Rows label="Mail queue">
        <Row>from the daemon</Row>
        <Row>from a person</Row>
      </Rows>,
    );

    const list = screen.getByRole("list", { name: "Mail queue" });
    expect(list.tagName).toBe("UL");
    expect(screen.getAllByRole("listitem")).toHaveLength(2);
  });

  it("keeps every row a direct child, which is what draws the rules", () => {
    // The separators are a 1px `gap` between the container's own children over a
    // `--border` ground. One wrapper div in between and the gap falls between
    // wrappers instead of rows: the column loses its rules and nothing errors.
    const { container } = render(
      <Rows label="Feed">
        <Row>one</Row>
        <Row>two</Row>
        <Row>three</Row>
      </Rows>,
    );

    const list = container.querySelector("ul");
    expect(list?.querySelectorAll(":scope > li")).toHaveLength(3);
  });

  it("holds a row's parts without flattening them", () => {
    // A row is a head line and whatever is under it — the four pages each stack
    // two or three blocks in one. The row is a column, not a slot.
    render(
      <Rows label="Memos">
        <Row>
          <div>00:42 · whisper</div>
          <p>what the recording said</p>
        </Row>
      </Rows>,
    );

    const row = screen.getByRole("listitem");
    expect(row.children).toHaveLength(2);
    expect(screen.getByText("what the recording said")).toBeTruthy();
  });

  it("is still a list when it is empty", () => {
    // An empty list is a list that has nothing in it, and the page's own empty
    // state goes beside it rather than inside — a `Quiet` line rendered as a row
    // would be announced as an item of the collection it says is absent.
    render(<Rows label="People">{null}</Rows>);

    expect(screen.getByRole("list", { name: "People" })).toBeTruthy();
    expect(screen.queryAllByRole("listitem")).toHaveLength(0);
  });

  it("puts a row's parts on one line when the row says so", () => {
    render(
      <Rows label="Subsystems">
        <Row layout="line">
          <span>daemon</span>
          <span>running</span>
        </Row>
      </Rows>,
    );

    // The axis is a modifier on the row, not a className the caller passes:
    // the row's fill is what keeps the hairline gap from showing through, so
    // handing out className would hand out the ability to delete a rule.
    const row = screen.getByRole("listitem");
    expect(row.className).toContain("ui-rows-row");
    expect(row.className).toContain("ui-rows-row-line");
  });

  it("stacks by default, which is what the four extracted lists do", () => {
    render(
      <Rows label="People">
        <Row>
          <span>ada</span>
        </Row>
      </Rows>,
    );

    expect(screen.getByRole("listitem").className).toBe("ui-rows-row");
  });

  it("becomes an ol when the ordering is part of what it says", () => {
    render(
      <Rows label="Items" as="ol">
        <Row>item 1 of job 41</Row>
        <Row>item 2 of job 41</Row>
      </Rows>,
    );

    expect(screen.getByRole("list", { name: "Items" }).tagName).toBe("OL");
  });
});
