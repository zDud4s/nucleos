import { describe, expect, it } from "vitest";
import { render, screen } from "@testing-library/react";

import Transcript from "./Transcript";
import type { Turn } from "./turns";
import type { Said } from "../api";

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
    render(<Transcript turns={[turn(1, "cloud"), turn(2, "local"), turn(3, "cloud")]} loaded pickedUp={null} />);

    // → local re-reads the conversation from the database each turn; → cloud starts on a session
    // that was just forgotten. Writing "memory restarts" on both would be tidier and false on one.
    expect(screen.getByText(/re-reads the recent turns/)).toBeTruthy();
    expect(screen.getByText(/starts here with no memory/)).toBeTruthy();
  });

  it("draws no mark when nobody changed model", () => {
    render(<Transcript turns={[turn(1, "cloud"), turn(2, "cloud")]} loaded pickedUp={null} />);

    expect(screen.queryByText(/starts here with no memory/)).toBeNull();
    expect(screen.queryByText(/re-reads the recent turns/)).toBeNull();
  });

  it("draws no mark against a turn whose model nothing knows", () => {
    // Turns predating the column carry no model. A mark between one of those and a known one would
    // be asserting a change that nothing recorded.
    render(<Transcript turns={[turn(1, null), turn(2, "local")]} loaded pickedUp={null} />);

    expect(screen.queryByText(/re-reads the recent turns/)).toBeNull();
  });

  it("tells an unread conversation apart from an empty one", () => {
    const { rerender } = render(<Transcript turns={[]} loaded={false} pickedUp={null} />);
    expect(screen.getByText(/Reading the conversation/)).toBeTruthy();

    rerender(<Transcript turns={[]} loaded pickedUp={null} />);
    expect(screen.queryByText(/Reading the conversation/)).toBeNull();
    expect(screen.getByText(/Nothing said yet/)).toBeTruthy();
  });

  it("renders a reply as text and never as markup", () => {
    // A model's output is not this shell's to execute.
    const shouty: Turn = { ...turn(1, "cloud"), answer: "<img src=x onerror=alert(1)>" };
    const { container } = render(<Transcript turns={[shouty]} loaded pickedUp={null} />);

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
        loaded pickedUp={null}
      />,
    );

    expect(screen.getAllByText(/started over|restarted here/i)).toHaveLength(1);
  });

  it("claims no restart on turns that never said which session they ran in", () => {
    render(<Transcript turns={[turn(1, "cloud"), turn(2, "cloud")]} loaded pickedUp={null} />);

    expect(screen.queryByText(/restarted here/i)).toBeNull();
  });

  /**
   * One line, not two. A model change already says "no memory of what is above" and always comes
   * with a new session, so drawing both would report one event as two.
   */
  it("does not stack a restart on top of a model change", () => {
    render(<Transcript turns={[turn(1, "local", "s-one"), turn(2, "cloud", "s-two")]} loaded pickedUp={null} />);

    expect(screen.getAllByText(/no memory of what is above/i)).toHaveLength(1);
    expect(screen.queryByText(/restarted here/i)).toBeNull();
  });
});

describe("a conversation picked up from the editor", () => {
  const hadThere: Said[] = [
    { by_owner: true, text: "arranja o parser de datas" },
    { by_owner: false, text: "está arranjado" },
  ];

  it("shows what was said there, above what has been said here", () => {
    render(<Transcript turns={[turn(7, "cloud")]} loaded pickedUp={hadThere} />);

    expect(screen.getByText("arranja o parser de datas")).toBeTruthy();
    expect(screen.getByText("está arranjado")).toBeTruthy();
    expect(screen.getByText("answer 7")).toBeTruthy();
  });

  it("marks where it was picked up, so the two halves are not read as one thread", () => {
    render(<Transcript turns={[turn(7, "cloud")]} loaded pickedUp={hadThere} />);

    expect(screen.getByText(/picked up here/i)).toBeTruthy();
  });

  it("draws no mark on a conversation that was opened here", () => {
    render(<Transcript turns={[turn(7, "cloud")]} loaded pickedUp={null} />);

    expect(screen.queryByText(/picked up here/i)).toBeNull();
  });

  it("does not claim nothing was said when the editor's half is all there is", () => {
    // A picked-up conversation has no turns of its own until you answer in it. Saying "nothing said
    // yet" over a conversation visibly full of what you said is the wrong answer this component
    // exists to prevent.
    render(<Transcript turns={[]} loaded pickedUp={hadThere} />);

    expect(screen.queryByText("Nothing said yet.")).toBeNull();
    expect(screen.getByText("arranja o parser de datas")).toBeTruthy();
  });
});
