import { describe, expect, it } from "vitest";
import { render } from "@testing-library/react";
import { Ring, type RingTrack } from "./Ring";
import { readState } from "./state-map";

function arcs(container: Element): Element[] {
  return [...container.querySelectorAll(".ui-ring-arc")];
}

function track(state: string, used: number, measured = true, name = "7d"): RingTrack {
  return { domain: "quota", state, name, used, measured };
}

describe("Ring", () => {
  /**
   * The reason the primitive takes a badge's props and not a tone: an arc coloured by its caller
   * would be a second author for a state's colour, and the ring and the badge on the same fact
   * would disagree the first time the map moved a row.
   */
  it("colours each arc in exactly the tone the map gives its state", () => {
    const { container } = render(
      <Ring label="claude" tracks={[track("exhausted", 0.97), track("warn", 0.8, true, "5h")]} />,
    );
    const drawn = arcs(container);
    expect(drawn).toHaveLength(2);
    expect(drawn[0].classList.contains(`ui-ring-${readState("quota", "exhausted")!.tone}`)).toBe(true);
    expect(drawn[1].classList.contains(`ui-ring-${readState("quota", "warn")!.tone}`)).toBe(true);
  });

  /**
   * An unmeasured track draws no arc at all and a dashed groove instead. An untouched quota and an
   * unreadable one are different facts, and a zero-length arc would make them one picture.
   */
  it("draws an unmeasured track as a dashed groove with no arc", () => {
    const { container } = render(
      <Ring label="gemini" tracks={[track("unmeasured", 0, false), track("unmeasured", 0, false, "5h")]} />,
    );
    expect(arcs(container)).toHaveLength(0);
    expect(container.querySelectorAll(".ui-ring-unmeasured")).toHaveLength(2);
  });

  /** Near the cap and at the cap are different facts; the arc must keep them apart. */
  it("does not draw a high reading as a full ring", () => {
    const { container } = render(<Ring label="claude" tracks={[track("exhausted", 0.96)]} />);
    const [drawn, gap] = arcs(container)[0].getAttribute("stroke-dasharray")!.split(" ").map(Number);
    expect(drawn).toBeLessThan(gap);
  });

  /** The drawing is hidden; the sentence is the reading, and it names every window with its figure. */
  it("says the whole reading in one sentence and hides the drawing", () => {
    const { container, getByText } = render(
      <Ring label="claude" tracks={[track("ok", 0.46), track("unmeasured", 0, false, "5h")]} />,
    );
    expect(container.querySelector("svg")!.getAttribute("aria-hidden")).toBe("true");
    getByText(`claude: 7d 46% ${readState("quota", "ok")!.label}, 5h ${readState("quota", "unmeasured")!.label}`);
  });
});
