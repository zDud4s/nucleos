import { describe, expect, it } from "vitest";
import { EMPTY_TURN, onSegment } from "./turn-assembly";

describe("turn assembly", () => {
  it("a silent pause closes a segment and never the turn", () => {
    let state = EMPTY_TURN;
    for (const text of ["o que eu quero", "é mudar o ficheiro"]) {
      const next = onSegment(state, { text, verdict: "continues", elapsedMs: 1_000 });
      state = next.state;
      expect(next.signal).toBeNull();
    }
    expect(state.segments).toEqual(["o que eu quero", "é mudar o ficheiro"]);
  });

  it("the closing word delivers everything accumulated", () => {
    const first = onSegment(EMPTY_TURN, { text: "muda o ficheiro", verdict: "continues", elapsedMs: 1_000 });
    const next = onSegment(first.state, { text: "o outro", verdict: "closes", elapsedMs: 500 });
    expect(next.signal).toEqual({ type: "deliver", text: "muda o ficheiro o outro" });
    expect(next.state).toEqual(EMPTY_TURN);
  });

  it("a segment that was only the closing word delivers what came before it, cleanly", () => {
    const first = onSegment(EMPTY_TURN, { text: "muda o ficheiro", verdict: "continues", elapsedMs: 1_000 });
    // The core stripped the word, so the text is empty — it must not join a trailing space.
    const next = onSegment(first.state, { text: "", verdict: "closes", elapsedMs: 300 });
    expect(next.signal).toEqual({ type: "deliver", text: "muda o ficheiro" });
  });

  it("an empty segment keeps the turn accumulating", () => {
    // The core answers 204 for a segment it heard nothing in — a cough, a chair. The turn must
    // survive it: refusing here is what today's code does, and it would end a thought on a noise.
    const first = onSegment(EMPTY_TURN, { text: "isto é importante", verdict: "continues", elapsedMs: 900 });
    const next = onSegment(first.state, { text: "", verdict: "continues", elapsedMs: 200 });
    expect(next.signal).toBeNull();
    expect(next.state.segments).toEqual(["isto é importante"]);
  });

  it("a discard asks first, and a yes throws the turn away", () => {
    const said = onSegment(EMPTY_TURN, { text: "isto está errado", verdict: "continues", elapsedMs: 900 });
    const asked = onSegment(said.state, { text: "não, risca isso", verdict: "discards", elapsedMs: 400 });
    expect(asked.signal).toEqual({ type: "confirmDiscard" });
    expect(asked.state.segments).toEqual(["isto está errado"]);

    const confirmed = onSegment(asked.state, { text: "sim", verdict: "confirms", elapsedMs: 200 });
    expect(confirmed.signal).toEqual({ type: "discard" });
    expect(confirmed.state).toEqual(EMPTY_TURN);
  });

  it("anything other than a yes cancels the discard and keeps talking", () => {
    const said = onSegment(EMPTY_TURN, { text: "isto está errado", verdict: "continues", elapsedMs: 900 });
    const asked = onSegment(said.state, { text: "não, risca isso", verdict: "discards", elapsedMs: 400 });
    const carriedOn = onSegment(asked.state, { text: "não, continua", verdict: "continues", elapsedMs: 700 });
    expect(carriedOn.signal).toBeNull();
    expect(carriedOn.state.pendingDiscard).toBe(false);
    // The command that asked to discard is NOT in here, and the sentence that cancelled it is.
    expect(carriedOn.state.segments).toEqual(["isto está errado", "não, continua"]);
  });

  it("a yes nobody asked for is a word like any other", () => {
    const next = onSegment(EMPTY_TURN, { text: "sim", verdict: "confirms", elapsedMs: 200 });
    expect(next.signal).toBeNull();
    expect(next.state.segments).toEqual(["sim"]);
  });

  it("the cap waits while a discard is pending", () => {
    // Otherwise the five-minute rule delivers exactly the segments somebody just asked to throw away.
    let state = EMPTY_TURN;
    for (let i = 0; i < 9; i += 1) {
      state = onSegment(state, { text: `parte ${i}`, verdict: "continues", elapsedMs: 30_000 }).state;
    }
    const asked = onSegment(state, { text: "risca isso", verdict: "discards", elapsedMs: 30_000 });
    expect(asked.signal).toEqual({ type: "confirmDiscard" });
  });
});
