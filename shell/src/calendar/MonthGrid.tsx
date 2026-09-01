// §spec calendario-local
import { useEffect, useRef, useState, type KeyboardEvent as ReactKeyboardEvent } from "react";
import {
  dayHours,
  isWorkingDay,
  occurrenceKey,
  type CalendarConfigView,
  type EventOccurrence,
} from "../data/calendar";
import { monthMatrix, nowFraction, sameDay, weekdayLabels } from "../lib/calendar-grid";
import { dateKeyOf, groupByLocalDay, placementOf, type Slot } from "./slot";

/**
 * The month: six weeks always, a heading row, and at most three chips a day.
 *
 * **The heading row is not decoration.** `monthMatrix` is Monday-first and the
 * grid it draws said so nowhere — seven unlabelled columns, and the only way
 * to know which one was Monday was to find today and count. That is the defect
 * this component was split out to fix, and it is why the labels come from
 * `weekdayLabels` in the reader's own locale rather than from a literal.
 *
 * **Three chips and a count, because the six rows are a promise.**
 * `monthMatrix` returns six weeks even when five would do, and its own header
 * explains why: a grid that changes height as you page through the year makes
 * the whole view jump under the cursor. The cells then defeated that with an
 * unbounded list — a day with nine events grew its row and the month jumped
 * anyway, one row at a time. Capping the chips is what makes the promise the
 * arithmetic already keeps true on screen as well.
 *
 * Pure and prop-driven — no query of its own — so the placement can be tested
 * without a daemon or a router behind it.
 */

/** Three, per design §6.14. The fourth line of a cell is the count, not an event. */
const MAX_CHIPS = 3;

export interface MonthGridProps {
  anchor: Date;
  occurrences: EventOccurrence[];
  now: Date;
  /** Read for the working-weekday dimming; absent until `/calendar/config` answers. */
  config: CalendarConfigView | undefined;
  selected: Slot;
  onSelect: (slot: Slot) => void;
}

/**
 * How far one key moves the selection, in days.
 *
 * The ARIA grid pattern's own set. `Home` and `End` are handled apart because
 * they move to a *position* in the week rather than by a fixed number of days.
 */
const KEY_STEPS: Record<string, number> = {
  ArrowLeft: -1,
  ArrowRight: 1,
  ArrowUp: -7,
  ArrowDown: 7,
};

export function MonthGrid({ anchor, occurrences, now, config, selected, onSelect }: MonthGridProps) {
  const weeks = monthMatrix(anchor);
  const days = weeks.flat();
  const byDay = groupByLocalDay(occurrences);
  const headings = weekdayLabels();
  const label = anchor.toLocaleDateString(undefined, { month: "long", year: "numeric" });

  const grid = useRef<HTMLDivElement>(null);
  /*
    Bumped on every keyboard move, and nothing else. The effect below reads it
    rather than the selected day, because focus must follow the ARROW KEYS and
    must NOT be yanked around by a click or by the page paging — moving focus
    on a mouse selection steals it from whatever the person was actually using.
  */
  const [moves, setMoves] = useState(0);

  useEffect(() => {
    if (moves === 0) return;
    grid.current
      ?.querySelector<HTMLButtonElement>(`[data-day="${dateKeyOf(selected.day)}"]`)
      ?.focus();
  }, [moves, selected.day]);

  /**
   * The keyboard half of `role="grid"`.
   *
   * The grid declared the role and implemented none of it: no click, no focus,
   * no keys — a promise to assistive technology that the markup did not keep.
   * Arrow keys move a day and a week, `Home`/`End` go to the ends of the row.
   *
   * **Clamped to the six weeks on screen**, rather than paging the month.
   * Arrowing off the edge and having the whole grid change under you is a
   * different gesture, and it belongs to `‹ Prev` / `Next ›` where a person can
   * see it. The window already carries a few days of the neighbouring months,
   * so the edges are reachable either way.
   */
  function onKeyDown(event: ReactKeyboardEvent<HTMLDivElement>) {
    const at = days.findIndex((day) => dateKeyOf(day) === dateKeyOf(selected.day));
    if (at === -1) return;

    const step = KEY_STEPS[event.key];
    let next = step === undefined ? -1 : at + step;
    if (event.key === "Home") next = at - (at % 7);
    if (event.key === "End") next = at - (at % 7) + 6;
    if (next < 0 || next >= days.length) return;

    event.preventDefault();
    onSelect({ day: days[next], hour: null });
    setMoves((count) => count + 1);
  }

  return (
    <div className="calendar-month">
      <div className="calendar-weekdays" aria-hidden="true">
        {headings.map((heading) => (
          <span className="calendar-weekday" key={heading.long}>
            {heading.short}
          </span>
        ))}
      </div>

      <div className="calendar-grid" role="grid" aria-label={label} ref={grid} onKeyDown={onKeyDown}>
        {/*
          The heading row is drawn outside the grid and hidden from the
          accessibility tree, and the columns carry their weekday in each
          cell's own label instead. A `columnheader` row would be the textbook
          answer and would read worse here: every cell already has to announce
          its date, so a screen reader would say "Wednesday" and then "20
          August 2026" as two separate stops on the way into one box.
        */}
        {weeks.map((week) => (
          <div className="calendar-row" role="row" key={dateKeyOf(week[0])}>
            {week.map((day) => (
              <DayCell
                key={dateKeyOf(day)}
                day={day}
                inMonth={day.getMonth() === anchor.getMonth()}
                occurrences={byDay.get(dateKeyOf(day)) ?? []}
                now={now}
                working={isWorkingDay(day, config)}
                selected={sameDay(day, selected.day)}
                onSelect={onSelect}
              />
            ))}
          </div>
        ))}
      </div>
    </div>
  );
}

interface DayCellProps {
  day: Date;
  inMonth: boolean;
  occurrences: EventOccurrence[];
  now: Date;
  working: boolean;
  selected: boolean;
  onSelect: (slot: Slot) => void;
}

/**
 * One day.
 *
 * **A button, not a div.** The grid used to be inert — it declared
 * `role="gridcell"` and had no click, no focus and no keyboard, which is a
 * promise to assistive technology that the markup did not keep. A cell is now
 * the control that selects its day, so the sheet below can carry the actions
 * instead of a list of every occurrence in the window.
 *
 * **Four facts, four different shapes**, because they stack and a reader has
 * to tell which are true at once: outside the month is dimmed *ink*, a
 * non-working day is a sunken *surface*, today is a filled date *pill*, and
 * the selection is a *ring*. Two of them expressed the same way — which is
 * what an earlier draft did, dimming both the outside days and the weekends —
 * makes a Saturday in the next month indistinguishable from either.
 */
function DayCell({ day, inMonth, occurrences, now, working, selected, onSelect }: DayCellProps) {
  const hours = dayHours(day);
  const today = sameDay(day, now);
  const shown = occurrences.slice(0, MAX_CHIPS);
  const hidden = occurrences.length - shown.length;

  const classes = ["calendar-day"];
  if (!inMonth) classes.push("calendar-day-outside");
  if (!working) classes.push("calendar-day-closed");
  if (today) classes.push("calendar-day-today");
  if (selected) classes.push("calendar-day-selected");

  /*
    The whole cell's accessible name, because the visible content is a bare
    number: "20" alone tells a screen reader nothing about which month it is
    in, whether it is today, or how much is on it. Assembled here rather than
    from the DOM so the sentence stays one sentence.
  */
  const said = [
    day.toLocaleDateString(undefined, { weekday: "long", day: "numeric", month: "long" }),
    today ? "today" : null,
    working ? null : "not a working day",
    hours.short ? `short day, ${hours.hours} hours` : null,
    hours.long ? `long day, ${hours.hours} hours` : null,
    occurrences.length === 1 ? "1 occurrence" : `${occurrences.length} occurrences`,
  ]
    .filter((part) => part !== null)
    .join(", ");

  return (
    <div className="calendar-cell" role="gridcell" aria-selected={selected}>
      <button
        type="button"
        className={classes.join(" ")}
        /* A roving tabindex: one stop for the whole grid, and the arrows move
           within it. Forty-two tab stops to cross a month is technically
           reachable and unusable, which is what the plain buttons gave. */
        tabIndex={selected ? 0 : -1}
        data-day={dateKeyOf(day)}
        aria-label={said}
        aria-pressed={selected}
        onClick={() => onSelect({ day, hour: null })}
      >
        <span className="calendar-day-head">
          <span className="calendar-day-number">{day.getDate()}</span>
          {(hours.short || hours.long) && (
            <span className="calendar-day-dst" title={`This local day is ${hours.hours} hours long`}>
              {hours.hours}h
            </span>
          )}
        </span>

        {today && <NowLine day={day} now={now} />}

        <span className="calendar-day-events">
          {shown.map((occurrence) => (
            <Chip
              key={occurrenceKey(occurrence.event_id, occurrence.occurrence_local)}
              occurrence={occurrence}
            />
          ))}
          {hidden > 0 && <span className="calendar-day-more">+{hidden} more</span>}
        </span>
      </button>
    </div>
  );
}

/**
 * One occurrence in a month cell: the hour, then the title.
 *
 * **The hour first, and there had better be one.** The chips carried the title
 * alone, so a day with three of them was three names in no stated order at no
 * stated time — the one question a month grid is asked that it could not
 * answer. The clock comes out of `occurrence_local` as text, never through a
 * `Date`: see `clockOfStamp`.
 *
 * A proposal is marked by tone rather than by an extra word. `shadow` is the
 * app's "deciding, not acting", which is exactly what an unapproved calendar
 * proposal is, and it means the chip does not have to spend a third of a
 * narrow cell on the label `PROPOSED`.
 *
 * A moved occurrence carries a mark of its own, because nothing else in the
 * app ever said so: the wire has no flag for it and `placementOf` is what
 * works it out. Knowing an event is not where its series put it is the
 * difference between "I misremembered" and "something moved this".
 */
function Chip({ occurrence }: { occurrence: EventOccurrence }) {
  const proposal = occurrence.source === "proposal";
  const { clock, moved } = placementOf(occurrence);
  const classes = ["calendar-chip"];
  if (proposal) classes.push("calendar-chip-proposal");
  if (moved) classes.push("calendar-chip-moved");
  return (
    <span className={classes.join(" ")}>
      <span className="calendar-chip-clock">{clock}</span>
      <span className="calendar-chip-title">{occurrence.title}</span>
      {proposal && <span className="calendar-visually-hidden">proposed</span>}
      {moved && <span className="calendar-visually-hidden">moved</span>}
    </span>
  );
}

/**
 * How far through today has already passed — absent on every other day.
 *
 * Exported because it is one of the components design §6.14 names, and tested
 * on its own. In the month it is a thin track under the date; the week draws
 * the same reading as a rule across the column (`WeekGrid`'s `NowRule`).
 *
 * **Not the accent.** It was, and the accent is the one brand colour — the
 * wordmark, links, the focus ring — never a state and never a reading. `info`
 * is the tone for "a fact with no verdict attached", which is precisely what
 * the time is.
 */
export function NowLine({ day, now }: { day: Date; now: Date }) {
  const fraction = nowFraction(now, day);
  if (fraction === null) return null;
  const percent = Math.round(fraction * 100);
  return (
    <span className="calendar-now-line" role="img" aria-label={`${percent}% through today`}>
      <span className="calendar-now-line-fill" style={{ width: `${percent}%` }} />
    </span>
  );
}
