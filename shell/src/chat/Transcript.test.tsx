import { describe, expect, it } from "vitest";
import { render, screen } from "@testing-library/react";

import Transcript from "./Transcript";
import type { Turn } from "./turns";

function turn(id: number, answeredBy: Turn["answeredBy"], sessionId: string | null = null): Turn {
  return {
    id,
    asked: `asked ${id}`,
    answer: `answer ${id}`,
    status: "completed",
    cost_usd: null,
    failed: false,
    answeredBy,
    sessionId,
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

  /**
   * The daemon drops a session that has passed its context ceiling, and the next turn runs in a
   * fresh one with no memory of anything above it. That used to happen in silence — which bites
   * hardest on a conversation picked up from the IDE, because it arrives with somebody else's
   * context already filling the window.
   */
  it("says where a conversation started over", () => {
    render(
      <Transcript
        turns={[turn(1, "cloud", "s-one"), turn(2, "cloud", "s-one"), turn(3, "cloud", "s-two")]}
        loaded
      />,
    );

    expect(screen.getAllByText(/started over|restarted here/i)).toHaveLength(1);
  });

  it("claims no restart on turns that never said which session they ran in", () => {
    render(<Transcript turns={[turn(1, "cloud"), turn(2, "cloud")]} loaded />);

    expect(screen.queryByText(/restarted here/i)).toBeNull();
  });

  /**
   * One line, not two. A model change already says "no memory of what is above" and always comes
   * with a new session, so drawing both would report one event as two.
   */
  it("does not stack a restart on top of a model change", () => {
    render(<Transcript turns={[turn(1, "local", "s-one"), turn(2, "cloud", "s-two")]} loaded />);

    expect(screen.getAllByText(/no memory of what is above/i)).toHaveLength(1);
    expect(screen.queryByText(/restarted here/i)).toBeNull();
  });
});
