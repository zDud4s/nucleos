import { describe, expect, it } from "vitest";
import { render, screen } from "@testing-library/react";

import Transcript from "./Transcript";
import type { Turn } from "./turns";

function turn(id: number, answeredBy: Turn["answeredBy"]): Turn {
  return {
    id,
    asked: `asked ${id}`,
    answer: `answer ${id}`,
    status: "completed",
    cost_usd: null,
    failed: false,
    answeredBy,
  };
}

describe("Transcript", () => {
  it("says the two directions differently, because they are not the same thing", () => {
    render(<Transcript turns={[turn(1, "cloud"), turn(2, "local"), turn(3, "cloud")]} loaded />);

    // → local re-reads the conversation from the database each turn; → cloud starts on a session
    // that was just forgotten. Writing "memory restarts" on both would be tidier and false on one.
    expect(screen.getByText(/re-reads the recent turns/)).toBeTruthy();
    expect(screen.getByText(/starts here with no memory/)).toBeTruthy();
  });

  it("draws no mark when nobody changed model", () => {
    render(<Transcript turns={[turn(1, "cloud"), turn(2, "cloud")]} loaded />);

    expect(screen.queryByText(/starts here with no memory/)).toBeNull();
    expect(screen.queryByText(/re-reads the recent turns/)).toBeNull();
  });

  it("draws no mark against a turn whose model nothing knows", () => {
    // Turns predating the column carry no model. A mark between one of those and a known one would
    // be asserting a change that nothing recorded.
    render(<Transcript turns={[turn(1, null), turn(2, "local")]} loaded />);

    expect(screen.queryByText(/re-reads the recent turns/)).toBeNull();
  });

  it("tells an unread conversation apart from an empty one", () => {
    const { rerender } = render(<Transcript turns={[]} loaded={false} />);
    expect(screen.getByText(/Reading the conversation/)).toBeTruthy();

    rerender(<Transcript turns={[]} loaded />);
    expect(screen.queryByText(/Reading the conversation/)).toBeNull();
    expect(screen.getByText(/Nothing said yet/)).toBeTruthy();
  });

  it("renders a reply as text and never as markup", () => {
    // A model's output is not this shell's to execute.
    const shouty: Turn = { ...turn(1, "cloud"), answer: "<img src=x onerror=alert(1)>" };
    const { container } = render(<Transcript turns={[shouty]} loaded />);

    expect(container.querySelector("img")).toBeNull();
    expect(screen.getByText("<img src=x onerror=alert(1)>")).toBeTruthy();
  });
});
