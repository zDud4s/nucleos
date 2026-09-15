import { describe, expect, it } from "vitest";
import { render, screen } from "@testing-library/react";
import { SlotPips } from "./SlotPips";
import { StateBadge, type StateBadgeProps } from "./StateBadge";

/** Each pip's tone in order, `free` for a hollow one. */
function tones(container: Element): string[] {
  return [...container.querySelectorAll(".ui-pip")].map((pip) => suffix(pip, "ui-pip-"));
}

function suffix(element: Element, prefix: string): string {
  return [...element.classList].find((name) => name.startsWith(prefix))!.slice(prefix.length);
}

const implementing: StateBadgeProps = { domain: "job", state: "implementing" };

describe("SlotPips", () => {
  it("draws one pip per slot of the limit: lit for each held, hollow for each free", () => {
    const { container } = render(
      <SlotPips held={[implementing, { domain: "slot", state: "orphaned" }]} limit={4} />,
    );
    expect(tones(container)).toEqual(["active", "danger", "free", "free"]);
  });

  /**
   * The reason the primitive takes a badge's props and not a tone. A pip lit by a caller's choice
   * would be a second author for a state's colour, and the first time the map moved a row — as it
   * moved `conflicted` from red to Held Ember — the rack and the card would disagree.
   */
  it("lights a pip in exactly the tone a badge handed the same reading wears", () => {
    const readings: StateBadgeProps[] = [
      { domain: "job", state: "awaiting_approval" },
      { domain: "job", state: "waiting" },
      { domain: "run", state: "interrupted" },
      { domain: "run", state: "completed" },
      { domain: "job_item", state: "conflicted" },
      { domain: "slot", state: "unknown" },
      { domain: "slot", state: "orphaned" },
    ];
    for (const reading of readings) {
      const pip = render(<SlotPips held={[reading]} limit={1} />).container.querySelector(".ui-pip")!;
      const badge = render(<StateBadge {...reading} />).container.querySelector(".ui-badge")!;
      expect(suffix(pip, "ui-pip-"), `${reading.domain}.${reading.state}`).toBe(suffix(badge, "ui-badge-"));
    }
  });

  it("admits a word it has no reading for in Switched Off Grey, and says the word", () => {
    const { container } = render(<SlotPips held={[{ domain: "run", state: "hibernating" }]} limit={2} />);
    expect(tones(container)).toEqual(["off", "free"]);
    expect(screen.getByText("1 of 2 slots held (hibernating), room for 1")).toBeDefined();
  });

  it("past eight slots draws only what is held, and writes out of how many in the daemon's face", () => {
    const { container } = render(<SlotPips held={[implementing, implementing, implementing]} limit={20} />);
    expect(tones(container)).toEqual(["active", "active", "active"]);
    expect(screen.getByText("3/20").className).toBe("ui-pips-count");
    expect(screen.getByText("3 of 20 slots held (implementing, implementing, implementing), room for 17")).toBeDefined();

    // Eight is still every pip, and no count: the row says it by itself.
    const eight = render(<SlotPips held={[]} limit={8} />).container;
    expect(tones(eight)).toEqual(Array(8).fill("free"));
    expect(eight.querySelector(".ui-pips-count")).toBeNull();
  });

  it("hides the drawing from assistive tech and says it in a sentence instead", () => {
    const { container } = render(<SlotPips held={[implementing]} limit={3} />);
    expect(container.querySelector(".ui-pips-row")?.getAttribute("aria-hidden")).toBe("true");
    const said = screen.getByText("1 of 3 slots held (implementing), room for 2");
    expect(said.className).toBe("sr-only");
    expect(said.closest("[aria-hidden]")).toBeNull();
  });

  it("says nothing about room when there is none, and draws what is held under a lowered ceiling", () => {
    const full = render(<SlotPips held={[implementing, { domain: "run", state: "running" }]} limit={2} />);
    expect(within(full.container)).toBe("2 of 2 slots held (implementing, running)");

    // A ceiling lowered below what is held: every held slot is still a lamp, and none is free.
    const over = render(<SlotPips held={[implementing, implementing, implementing]} limit={2} />);
    expect(tones(over.container)).toEqual(["active", "active", "active"]);

    const empty = render(<SlotPips held={[]} limit={2} />);
    expect(within(empty.container)).toBe("0 of 2 slots held, room for 2");
  });
});

/** The sentence a container's pips say, read off the one element that says it. */
function within(container: Element): string {
  return container.querySelector(".sr-only")?.textContent ?? "";
}
