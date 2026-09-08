import { afterAll, beforeAll, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen, within } from "@testing-library/react";
import { MonthGrid, NowLine } from "./MonthGrid";
import type { CalendarConfigView, EventOccurrence } from "../data/calendar";
import { UI_LOCALE } from "../lib/locale";
import type { DragHandlers } from "./slot";

/**
 * The month grid.
 *
 * `dayHours` reads a real local day span, which only differs from 24 on the
 * two real transition days of a real zone's year — so this file pins the
 * process to one, through vitest's own env stub rather than `process.env`
 * (this project's `lib` carries no Node types), and only for its own duration.
 * Without it these pass in August and fail in October.
 */
beforeAll(() => {
  vi.stubEnv("TZ", "Europe/Lisbon");
});

afterAll(() => {
  vi.unstubAllEnvs();
});

const CONFIG: CalendarConfigView = {
  default_tz: "Europe/Lisbon",
  working_hours_start: "09:00",
  working_hours_end: "18:00",
  working_weekdays: ["mon", "tue", "wed", "thu", "fri"],
};

/** Lisbon is UTC+1 in August, so a 09:00 local start is 08:00Z. */
function occurrence(overrides: Partial<EventOccurrence> = {}): EventOccurrence {
  return {
    event_id: 1,
    title: "Standup",
    source: "human",
    occurrence_local: "2026-08-20T09:00:00",
    starts_at: "2026-08-20T08:00:00Z",
    ends_at: "2026-08-20T08:30:00Z",
    ...overrides,
  };
}

/** A drag that is not happening, which is the state every test but the drag ones is in. */
const NO_DRAG: DragHandlers = {
  dragging: null,
  onDragStart: () => {},
  onDragEnd: () => {},
  onDrop: () => {},
};

function august(occurrences: EventOccurrence[], overrides: Partial<Parameters<typeof MonthGrid>[0]> = {}) {
  return render(
    <MonthGrid
      anchor={new Date(2026, 7, 1)}
      occurrences={occurrences}
      now={new Date(2026, 7, 20, 12, 0)}
      config={CONFIG}
      selected={{ day: new Date(2026, 7, 20), hour: null }}
      onSelect={() => {}}
      drag={NO_DRAG}
      {...overrides}
    />,
  );
}

/**
 * The cell for one exact day.
 *
 * Matched on the day's own localised name and not on the bare number, for two
 * reasons that both bite: a six-week grid shows the 1st to the 6th TWICE — the
 * anchor month's and the next month's — so a number alone is ambiguous, and
 * these assertions run under whatever locale the machine has. This one is
 * Portuguese, which is how the first draft of this file failed on `"Thursday"`
 * while the component was perfectly correct.
 */
function said(day: Date): string {
  return day.toLocaleDateString(UI_LOCALE, { weekday: "long", day: "numeric", month: "long" });
}

function cellFor(day: Date): HTMLElement {
  const wanted = said(day);
  const found = screen
    .getAllByRole("gridcell")
    .find((cell) => (within(cell).getByRole("button").getAttribute("aria-label") ?? "").startsWith(wanted));
  if (found === undefined) throw new Error(`no cell for ${wanted}`);
  return found;
}

/** August 2026, by date. The month under test in most of this file. */
function aug(date: number): Date {
  return new Date(2026, 7, date);
}

describe("the heading row", () => {
  /**
   * Seven unlabelled columns was the state of this grid, and `monthMatrix` is
   * Monday-first — so the only way to know which column was Monday was to find
   * today and count.
   */
  it("names the seven days, Monday first", () => {
    const { container } = august([]);
    const headings = container.querySelector(".calendar-weekdays") as HTMLElement;

    // Seven of them, and the first is a real Monday's own short name in this
    // machine's locale — 1 January 2024 was a Monday. Asserting the English
    // strings would be asserting the locale rather than the order.
    const shown = [...headings.children].map((child) => child.textContent);
    expect(shown).toHaveLength(7);
    expect(shown[0]).toBe(new Date(2024, 0, 1).toLocaleDateString(UI_LOCALE, { weekday: "short" }));
    expect(shown[6]).toBe(new Date(2024, 0, 7).toLocaleDateString(UI_LOCALE, { weekday: "short" }));
  });
});

describe("chips", () => {
  it("carries the hour, and sorts the day by it", () => {
    august([
      occurrence({ event_id: 2, title: "Retro", occurrence_local: "2026-08-20T14:00:00", starts_at: "2026-08-20T13:00:00Z", ends_at: "2026-08-20T13:30:00Z" }),
      occurrence({ event_id: 1, title: "Standup" }),
    ]);

    const cell = cellFor(aug(20));
    // The clock is on screen at all, which it was not: chips carried the title
    // alone, so a day with two of them was two names at no stated time.
    expect(within(cell).getByText("09:00")).toBeDefined();
    expect(within(cell).getByText("14:00")).toBeDefined();

    // And in that order, whatever order the daemon answered in.
    const titles = within(cell)
      .getAllByText(/Standup|Retro/)
      .map((node) => node.textContent);
    expect(titles).toEqual(["Standup", "Retro"]);
  });

  /**
   * The cap is not cosmetic. `monthMatrix` returns six weeks even when five
   * would do, so that the view does not jump as you page through the year —
   * and an uncapped list of events defeated that one row at a time.
   */
  it("draws at most three, and counts the rest", () => {
    const many = [9, 10, 11, 12, 13].map((hour) =>
      occurrence({
        event_id: hour,
        title: `Meeting ${hour}`,
        occurrence_local: `2026-08-20T${hour}:00:00`,
        starts_at: `2026-08-20T${String(hour - 1).padStart(2, "0")}:00:00Z`,
        ends_at: `2026-08-20T${String(hour - 1).padStart(2, "0")}:30:00Z`,
      }),
    );
    august(many);

    const cell = cellFor(aug(20));
    expect(within(cell).getAllByText(/^Meeting /)).toHaveLength(3);
    expect(within(cell).getByText("+2 more")).toBeDefined();
    // The three shown are the earliest three, not an arbitrary three.
    expect(within(cell).getByText("Meeting 11")).toBeDefined();
    expect(within(cell).queryByText("Meeting 12")).toBeNull();
  });

  /**
   * The bug this redesign found: `expand` decides window membership with the
   * RESOLVED instant and reports the ORIGINAL local start beside it, so the
   * grid drew a moved occurrence on the day it had been moved off.
   */
  it("draws a moved occurrence on the day it was moved to", () => {
    august([
      occurrence({
        title: "Moved thing",
        occurrence_local: "2026-08-04T09:00:00",
        starts_at: "2026-08-20T14:00:00Z",
        ends_at: "2026-08-20T15:00:00Z",
      }),
    ]);

    expect(within(cellFor(aug(20))).getByText("Moved thing")).toBeDefined();
    expect(within(cellFor(aug(4))).queryByText("Moved thing")).toBeNull();
    // And it says so, which nothing in the app did before: the wire has no flag.
    expect(within(cellFor(aug(20))).getByText("moved")).toBeDefined();
  });

  it("marks a proposal without spending a third of the cell on the word", () => {
    august([occurrence({ source: "proposal", title: "Suggested" })]);
    const cell = cellFor(aug(20));
    // Said to a screen reader; carried visually by tone, per §6.14's own styling note.
    expect(within(cell).getByText("proposed")).toBeDefined();
    expect(within(cell).getByText("Suggested").parentElement?.className).toContain(
      "calendar-chip-proposal",
    );
  });
});

describe("the shapes a day can carry", () => {
  it("badges only the short day, and only that one", () => {
    render(
      <MonthGrid
        anchor={new Date(2026, 2, 1)}
        occurrences={[]}
        now={new Date(2026, 2, 15)}
        config={CONFIG}
        selected={{ day: new Date(2026, 2, 15), hour: null }}
        onSelect={() => {}}
        drag={NO_DRAG}
      />,
    );

    // Lisbon, 29 March 2026: 01:00 WET jumps to 02:00 WEST, so the day is 23 hours.
    const badges = screen.getAllByText("23h");
    expect(badges).toHaveLength(1);
    expect(within(cellFor(new Date(2026, 2, 29))).getByText("23h")).toBeDefined();
  });

  /** The other transition, which had never been rendered: `dayHours` has three outcomes. */
  it("badges the long day in October", () => {
    render(
      <MonthGrid
        anchor={new Date(2026, 9, 1)}
        occurrences={[]}
        now={new Date(2026, 9, 15)}
        config={CONFIG}
        selected={{ day: new Date(2026, 9, 15), hour: null }}
        onSelect={() => {}}
        drag={NO_DRAG}
      />,
    );

    const badges = screen.getAllByText("25h");
    expect(badges).toHaveLength(1);
    expect(within(cellFor(new Date(2026, 9, 25))).getByText("25h")).toBeDefined();
  });

  /**
   * `working_weekdays` arrived on the wire, was typed, and was read by nothing
   * on this page — the config was spent on a sentence in the headline.
   */
  it("dims the days nobody works, and only those", () => {
    august([]);
    // 22 and 23 August 2026 are the Saturday and Sunday.
    expect(within(cellFor(aug(22))).getByRole("button").className).toContain("calendar-day-closed");
    expect(within(cellFor(aug(23))).getByRole("button").className).toContain("calendar-day-closed");
    expect(within(cellFor(aug(20))).getByRole("button").className).not.toContain("calendar-day-closed");
  });

  it("says in one sentence what a cell is, rather than leaving a bare number", () => {
    august([occurrence()]);
    const label = within(cellFor(aug(20))).getByRole("button").getAttribute("aria-label") ?? "";

    // The date in the reader's own language, then the facts a bare "20" cannot carry.
    expect(label).toContain(said(aug(20)));
    expect(label).toContain("today");
    expect(label).toContain("1 occurrence");
  });

  it("says a weekend is not a working day", () => {
    august([]);
    expect(within(cellFor(aug(22))).getByRole("button").getAttribute("aria-label")).toContain(
      "not a working day",
    );
  });
});

describe("selection", () => {
  /** The grid was inert — `role="gridcell"` with no click, no focus and no keyboard. */
  it("selects the day it was clicked on", () => {
    const chosen: Date[] = [];
    august([], { onSelect: (slot) => chosen.push(slot.day) });

    fireEvent.click(within(cellFor(aug(12))).getByRole("button"));

    expect(chosen).toHaveLength(1);
    expect(chosen[0].getDate()).toBe(12);
    expect(chosen[0].getMonth()).toBe(7);
  });

  it("marks the selected day as pressed, so it is not only a colour", () => {
    august([]);
    expect(within(cellFor(aug(20))).getByRole("button").getAttribute("aria-pressed")).toBe("true");
    expect(within(cellFor(aug(19))).getByRole("button").getAttribute("aria-pressed")).toBe("false");
  });
});

/* --------------------------------------------------------------- dragging -- */

describe("dragging a chip to another day", () => {
  /** A drag with an empty `dataTransfer` is not a drag: Firefox cancels it outright. */
  it("picks the occurrence up, and puts the key on the transfer", () => {
    const picked: EventOccurrence[] = [];
    const transfer = { setData: vi.fn(), effectAllowed: "" };
    august([occurrence()], { drag: { ...NO_DRAG, onDragStart: (row) => picked.push(row) } });

    fireEvent.dragStart(screen.getByText("Standup").parentElement as HTMLElement, {
      dataTransfer: transfer,
    });

    expect(picked).toHaveLength(1);
    expect(picked[0].occurrence_local).toBe("2026-08-20T09:00:00");
    expect(transfer.setData).toHaveBeenCalledWith("text/plain", "1:2026-08-20T09:00:00");
  });

  it("drops on a day and names it, with no hour a month cell cannot know", () => {
    const dropped: { day: Date; hour: number | null }[] = [];
    august([occurrence()], {
      drag: {
        ...NO_DRAG,
        dragging: occurrence(),
        onDrop: (day, hour) => dropped.push({ day, hour }),
      },
    });

    const target = within(cellFor(aug(27))).getByRole("button");
    fireEvent.dragOver(target);
    fireEvent.drop(target);

    expect(dropped).toHaveLength(1);
    expect(dropped[0].day.getDate()).toBe(27);
    expect(dropped[0].hour).toBeNull();
  });

  /**
   * The day it already sits on is not a target. `moveFromDrop` refuses that
   * drop anyway, and not offering it is better than refusing it silently.
   */
  it("does not offer the day the occurrence is already on", () => {
    const dropped: Date[] = [];
    august([occurrence()], {
      drag: { ...NO_DRAG, dragging: occurrence(), onDrop: (day) => dropped.push(day) },
    });

    const itsOwnDay = within(cellFor(aug(20))).getByRole("button");
    expect(itsOwnDay.className).not.toContain("calendar-day-takes");
    fireEvent.drop(itsOwnDay);
    expect(dropped).toEqual([]);

    // Every other day does take it.
    expect(within(cellFor(aug(27))).getByRole("button").className).toContain("calendar-day-takes");
  });

  it("marks no day at all while nothing is being dragged", () => {
    const { container } = august([occurrence()]);
    expect(container.querySelectorAll(".calendar-day-takes")).toHaveLength(0);
  });

  it("fades the chip that is in the hand", () => {
    august([occurrence()], { drag: { ...NO_DRAG, dragging: occurrence() } });
    expect((screen.getByText("Standup").parentElement as HTMLElement).className).toContain(
      "calendar-chip-held",
    );
  });
});

/* --------------------------------------------------------------- keyboard -- */

describe("the keyboard", () => {
  /**
   * The grid declared `role="grid"` and implemented none of the pattern: no
   * click, no focus, no keys. Declaring a role and not keeping it is worse
   * than not declaring it — assistive technology announces a grid and then the
   * arrows do nothing.
   */
  it("moves a day with left and right, and a week with up and down", () => {
    const chosen: Date[] = [];
    const { container } = august([], { onSelect: (slot) => chosen.push(slot.day) });
    const grid = container.querySelector(".calendar-grid") as HTMLElement;

    fireEvent.keyDown(grid, { key: "ArrowRight" });
    expect(chosen[0].getDate()).toBe(21);

    fireEvent.keyDown(grid, { key: "ArrowLeft" });
    expect(chosen[1].getDate()).toBe(19);

    fireEvent.keyDown(grid, { key: "ArrowDown" });
    expect(chosen[2].getDate()).toBe(27);

    fireEvent.keyDown(grid, { key: "ArrowUp" });
    expect(chosen[3].getDate()).toBe(13);
  });

  it("goes to the ends of the week with Home and End", () => {
    const chosen: Date[] = [];
    const { container } = august([], { onSelect: (slot) => chosen.push(slot.day) });
    const grid = container.querySelector(".calendar-grid") as HTMLElement;

    // Thursday 20 August sits in the row Monday 17 to Sunday 23.
    fireEvent.keyDown(grid, { key: "Home" });
    expect(chosen[0].getDate()).toBe(17);

    fireEvent.keyDown(grid, { key: "End" });
    expect(chosen[1].getDate()).toBe(23);
  });

  /**
   * Clamped rather than paging. Arrowing off the edge and having the whole
   * month change underneath is a different gesture, and it belongs to the
   * buttons where a person can see it.
   */
  it("stops at the edges of the six weeks instead of paging the month", () => {
    const chosen: Date[] = [];
    const { container } = august([], {
      // The first cell of the grid: Monday 27 July.
      selected: { day: new Date(2026, 6, 27), hour: null },
      onSelect: (slot) => chosen.push(slot.day),
    });
    const grid = container.querySelector(".calendar-grid") as HTMLElement;

    fireEvent.keyDown(grid, { key: "ArrowLeft" });
    fireEvent.keyDown(grid, { key: "ArrowUp" });

    expect(chosen).toEqual([]);
  });

  /**
   * One tab stop for the whole month. Forty-two of them is technically
   * reachable and unusable, which is what plain buttons gave.
   */
  it("keeps a single tab stop, on the selected day", () => {
    const { container } = august([]);
    const stops = [...container.querySelectorAll('.calendar-day[tabindex="0"]')];

    expect(stops).toHaveLength(1);
    expect(stops[0].getAttribute("data-day")).toBe("2026-08-20");
  });
});

describe("NowLine", () => {
  it("reports how far through today has gone", () => {
    render(<NowLine day={new Date(2026, 7, 20)} now={new Date(2026, 7, 20, 6, 0)} />);
    expect(screen.getByRole("img").getAttribute("aria-label")).toBe("25% through today");
  });

  /** Absent off-day, which is what stops a week drawing seven of them. */
  it("draws nothing on any other day", () => {
    const { container } = render(
      <NowLine day={new Date(2026, 7, 19)} now={new Date(2026, 7, 20, 6, 0)} />,
    );
    expect(container.firstChild).toBeNull();
  });
});
