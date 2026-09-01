import { afterAll, beforeAll, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen } from "@testing-library/react";
import { WeekGrid } from "./WeekGrid";
import type { CalendarConfigView, EventOccurrence } from "../data/calendar";
import type { DragHandlers, Slot } from "./slot";

/**
 * The week grid.
 *
 * **jsdom computes no layout, so every assertion here is on the geometry this
 * component WROTE, never on geometry a browser worked out.** That is a real
 * limit and the reason `scripts/preview-shots.mjs` grew four calendar shots:
 * these tests prove the arithmetic reaches the DOM, and only the pictures
 * prove it reads. What they cover is exactly the half a screenshot cannot
 * check by itself — that a 23-hour column really got 23 bands, that two
 * colliding meetings really got half the width each.
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

/** Lisbon is UTC+1 in August: 09:00 local is 08:00Z. */
function occurrence(overrides: Partial<EventOccurrence> = {}): EventOccurrence {
  return {
    event_id: 1,
    title: "Standup",
    source: "human",
    occurrence_local: "2026-08-20T09:00:00",
    starts_at: "2026-08-20T08:00:00Z",
    ends_at: "2026-08-20T09:00:00Z",
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

/** The week of Monday 17 August 2026, which contains the Thursday the fixtures use. */
function week(
  occurrences: EventOccurrence[],
  onSelect: (slot: Slot) => void = () => {},
  drag: DragHandlers = NO_DRAG,
) {
  return render(
    <WeekGrid
      anchor={new Date(2026, 7, 20)}
      occurrences={occurrences}
      now={new Date(2026, 7, 20, 12, 0)}
      config={CONFIG}
      selected={{ day: new Date(2026, 7, 20), hour: null }}
      onSelect={onSelect}
      drag={drag}
    />,
  );
}

/** The block drawn for a title, as the element carrying the inline geometry. */
function blockFor(title: string): HTMLElement {
  return screen.getByText(title).closest("button") as HTMLElement;
}

function percent(value: string | undefined): number {
  return Number((value ?? "").replace("%", ""));
}

describe("placing a block", () => {
  it("puts an event at its own fraction of the day, with its own height", () => {
    week([occurrence()]);
    const block = blockFor("Standup");

    // 09:00–10:00 on an ordinary 24-hour day.
    expect(percent(block.style.top)).toBeCloseTo((9 / 24) * 100, 4);
    expect(percent(block.style.height)).toBeCloseTo((1 / 24) * 100, 4);
  });

  it("draws the block on the day the occurrence resolves to, not the one it was written for", () => {
    week([
      occurrence({
        title: "Moved thing",
        occurrence_local: "2026-08-18T09:00:00",
        starts_at: "2026-08-20T13:00:00Z",
        ends_at: "2026-08-20T14:00:00Z",
      }),
    ]);

    // 13:00Z is 14:00 in the room, on Thursday — and the clock says so.
    expect(screen.getByText("14:00")).toBeDefined();
    expect(percent(blockFor("Moved thing").style.top)).toBeCloseTo((14 / 24) * 100, 4);
  });
});

describe("overlap lanes", () => {
  /** `overlapLanes` had been written, table-tested, and called by nothing at all. */
  it("halves two meetings that collide, and puts them side by side", () => {
    week([
      occurrence({ event_id: 1, title: "Standup" }),
      occurrence({
        event_id: 2,
        title: "Interview",
        occurrence_local: "2026-08-20T09:30:00",
        starts_at: "2026-08-20T08:30:00Z",
        ends_at: "2026-08-20T09:30:00Z",
      }),
    ]);

    const first = blockFor("Standup");
    const second = blockFor("Interview");

    expect(percent(first.style.width)).toBeCloseTo(50, 4);
    expect(percent(second.style.width)).toBeCloseTo(50, 4);
    expect(percent(first.style.left)).toBeCloseTo(0, 4);
    expect(percent(second.style.left)).toBeCloseTo(50, 4);
  });

  /** The behaviour its own header promises: hours apart is not a collision. */
  it("leaves a morning and an afternoon at full width", () => {
    week([
      occurrence({ event_id: 1, title: "Standup" }),
      occurrence({
        event_id: 2,
        title: "Retro",
        occurrence_local: "2026-08-20T15:00:00",
        starts_at: "2026-08-20T14:00:00Z",
        ends_at: "2026-08-20T15:00:00Z",
      }),
    ]);

    expect(percent(blockFor("Standup").style.width)).toBeCloseTo(100, 4);
    expect(percent(blockFor("Retro").style.width)).toBeCloseTo(100, 4);
  });

  it("gives three at the same hour a third each", () => {
    week([
      occurrence({ event_id: 1, title: "One" }),
      occurrence({ event_id: 2, title: "Two" }),
      occurrence({ event_id: 3, title: "Three" }),
    ]);

    for (const title of ["One", "Two", "Three"]) {
      expect(percent(blockFor(title).style.width)).toBeCloseTo(100 / 3, 4);
    }
  });
});

describe("the working day", () => {
  it("washes the working hours, positioned by the same function that places the events", () => {
    const { container } = week([]);
    const washes = [...container.querySelectorAll(".calendar-work-wash")] as HTMLElement[];

    // Five working days in the config, so five washes and not seven.
    expect(washes).toHaveLength(5);
    // 09:00 to 18:00 of a 24-hour day.
    expect(percent(washes[0].style.top)).toBeCloseTo((9 / 24) * 100, 4);
    expect(percent(washes[0].style.height)).toBeCloseTo((9 / 24) * 100, 4);
  });

  it("leaves the weekend columns sunken, with no wash at all", () => {
    const { container } = week([]);
    expect(container.querySelectorAll(".calendar-column-closed")).toHaveLength(2);
  });
});

describe("a day that is not 24 hours", () => {
  /**
   * The whole reason each column measures itself instead of sharing one set of
   * marks across the row: on the week of a transition, six columns have 24
   * bands and one does not.
   */
  it("gives the short day 23 bands while its neighbours keep 24", () => {
    const { container } = render(
      <WeekGrid
        anchor={new Date(2026, 2, 29)}
        occurrences={[]}
        now={new Date(2026, 2, 25, 12, 0)}
        config={CONFIG}
        selected={{ day: new Date(2026, 2, 29), hour: null }}
        onSelect={() => {}}
        drag={NO_DRAG}
      />,
    );

    const columns = [...container.querySelectorAll(".calendar-column")];
    const counts = columns.map((column) => column.querySelectorAll(".calendar-slot").length);

    // Monday to Saturday are ordinary; Sunday 29 March is the 23-hour day.
    expect(counts).toEqual([24, 24, 24, 24, 24, 24, 23]);
  });

  it("badges that column so the compressed lines are explained rather than mysterious", () => {
    render(
      <WeekGrid
        anchor={new Date(2026, 2, 29)}
        occurrences={[]}
        now={new Date(2026, 2, 25, 12, 0)}
        config={CONFIG}
        selected={{ day: new Date(2026, 2, 29), hour: null }}
        onSelect={() => {}}
        drag={NO_DRAG}
      />,
    );

    expect(screen.getAllByText("23h")).toHaveLength(1);
  });
});

describe("the now rule", () => {
  it("draws once, on today's column only", () => {
    const { container } = week([]);
    const rules = container.querySelectorAll(".calendar-now-rule");

    expect(rules).toHaveLength(1);
    expect(percent((rules[0] as HTMLElement).style.top)).toBeCloseTo(50, 4);
  });

  it("draws nothing at all in a week that does not contain today", () => {
    const { container } = render(
      <WeekGrid
        anchor={new Date(2026, 7, 20)}
        occurrences={[]}
        now={new Date(2026, 9, 1, 12, 0)}
        config={CONFIG}
        selected={{ day: new Date(2026, 7, 20), hour: null }}
        onSelect={() => {}}
        drag={NO_DRAG}
      />,
    );

    expect(container.querySelectorAll(".calendar-now-rule")).toHaveLength(0);
  });
});

describe("dragging a block", () => {
  /** The hour comes from the BAND, which is what makes a 23-hour day come out right. */
  it("drops on a band and names that band's own hour", () => {
    const dropped: { day: Date; hour: number | null }[] = [];
    week([occurrence()], () => {}, {
      ...NO_DRAG,
      dragging: occurrence(),
      onDrop: (day, hour) => dropped.push({ day, hour }),
    });

    const friday = new Date(2026, 7, 21).toLocaleDateString(undefined, {
      weekday: "long",
      day: "numeric",
      month: "long",
    });
    const target = screen.getByLabelText(`${friday} at 15:00`);
    fireEvent.dragOver(target);
    fireEvent.drop(target);

    expect(dropped).toHaveLength(1);
    expect(dropped[0].hour).toBe(15);
    expect(dropped[0].day.getDate()).toBe(21);
  });

  it("picks the block up and puts the key on the transfer", () => {
    const picked: EventOccurrence[] = [];
    const transfer = { setData: vi.fn(), effectAllowed: "" };
    week([occurrence()], () => {}, { ...NO_DRAG, onDragStart: (row) => picked.push(row) });

    fireEvent.dragStart(blockFor("Standup"), { dataTransfer: transfer });

    expect(picked[0].occurrence_local).toBe("2026-08-20T09:00:00");
    expect(transfer.setData).toHaveBeenCalledWith("text/plain", "1:2026-08-20T09:00:00");
  });

  /**
   * Every band takes a drop — there is no hour of a day an occurrence cannot
   * be moved to — but only while something is actually in the hand.
   */
  it("offers every band while dragging, and none when not", () => {
    const { container: idle } = week([occurrence()]);
    expect(idle.querySelectorAll(".calendar-slot-takes")).toHaveLength(0);

    const { container: dragging } = week([occurrence()], () => {}, {
      ...NO_DRAG,
      dragging: occurrence(),
    });
    // Seven ordinary columns of 24 bands.
    expect(dragging.querySelectorAll(".calendar-slot-takes")).toHaveLength(7 * 24);
  });

  it("fades the block that is in the hand", () => {
    week([occurrence()], () => {}, { ...NO_DRAG, dragging: occurrence() });
    expect(blockFor("Standup").className).toContain("calendar-block-held");
  });

  /**
   * On the day the clocks go forward there is no **01** band, so a drag cannot
   * ask for an hour that does not exist on that day. That is the whole reason
   * the hour is read off the band's own instant rather than counted from the
   * index — counting would have offered 01:00 and then 22:00 twice.
   *
   * The missing hour is 01 and not 02, which is worth stating because the
   * first draft of this test asserted the wrong one: Lisbon jumps at 01:00
   * WET, so the clock goes 00:59 → 02:00 and it is one o'clock that never
   * happens.
   */
  it("offers no 01:00 band on the day the clocks go forward", () => {
    render(
      <WeekGrid
        anchor={new Date(2026, 2, 29)}
        occurrences={[]}
        now={new Date(2026, 2, 25, 12, 0)}
        config={CONFIG}
        selected={{ day: new Date(2026, 2, 29), hour: null }}
        onSelect={() => {}}
        drag={{ ...NO_DRAG, dragging: occurrence() }}
      />,
    );

    const named = (day: Date) =>
      day.toLocaleDateString(undefined, { weekday: "long", day: "numeric", month: "long" });

    expect(screen.queryByLabelText(`${named(new Date(2026, 2, 29))} at 01:00`)).toBeNull();
    // The bands either side of the hole are both there, and only one hour is missing.
    expect(screen.getByLabelText(`${named(new Date(2026, 2, 29))} at 00:00`)).toBeDefined();
    expect(screen.getByLabelText(`${named(new Date(2026, 2, 29))} at 02:00`)).toBeDefined();
    // Its neighbour, an ordinary day, has its one o'clock.
    expect(screen.getByLabelText(`${named(new Date(2026, 2, 28))} at 01:00`)).toBeDefined();
  });
});

describe("selecting", () => {
  /** Design §6.14 asks for the draft form "inline no slot clicado" — so a slot has to be clickable. */
  it("names a day AND an hour when an empty slot is clicked", () => {
    const chosen: Slot[] = [];
    week([], (slot) => chosen.push(slot));

    const thursday = new Date(2026, 7, 20).toLocaleDateString(undefined, {
      weekday: "long",
      day: "numeric",
      month: "long",
    });
    fireEvent.click(screen.getByLabelText(`${thursday} at 14:00`));

    expect(chosen).toHaveLength(1);
    expect(chosen[0].hour).toBe(14);
    expect(chosen[0].day.getDate()).toBe(20);
  });

  it("selects the block's own hour when a meeting is clicked", () => {
    const chosen: Slot[] = [];
    week([occurrence()], (slot) => chosen.push(slot));

    fireEvent.click(blockFor("Standup"));

    expect(chosen[0].hour).toBe(9);
    expect(chosen[0].day.getDate()).toBe(20);
  });

  it("selects the whole day when its heading is clicked", () => {
    const chosen: Slot[] = [];
    const { container } = week([], (slot) => chosen.push(slot));

    /*
      Scoped to the heading row rather than searched for by text: the gutter
      labels the hours 01 to 23, so a bare `getByText("18")` finds the date in
      the heading AND six o'clock in the evening.
    */
    const heads = [...container.querySelectorAll(".calendar-week-head")] as HTMLElement[];
    const tuesday = heads.find((head) =>
      head.querySelector(".calendar-week-head-number")?.textContent === "18",
    );
    fireEvent.click(tuesday as HTMLElement);

    expect(chosen[0].hour).toBeNull();
    expect(chosen[0].day.getDate()).toBe(18);
  });
});
