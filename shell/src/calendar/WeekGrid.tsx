// §spec calendario-local
import {
  dayHours,
  isWorkingDay,
  occurrenceKey,
  workingSpan,
  type CalendarConfigView,
  type EventOccurrence,
} from "../data/calendar";
import {
  dayBounds,
  hourMarks,
  hourSlots,
  nowFraction,
  overlapLanes,
  placeInDay,
  sameDay,
  weekOf,
} from "../lib/calendar-grid";
import { dateKeyOf, groupByLocalDay, placementOf, type DragHandlers, type Slot } from "./slot";

/**
 * The week, with an hour axis — which is the whole reason it exists.
 *
 * The month answers *what days have something on them*. Everything design
 * §6.14 asks for that the month could not carry needs a vertical time axis to
 * be drawn against: two meetings at the same hour side by side, the working
 * day washed in behind, a rule at the current minute, and a click that means
 * "nine in the morning on Wednesday" rather than "Wednesday". This view is
 * where `lib/calendar-grid.ts`'s `weekOf`, `hourMarks`, `hourSlots`,
 * `placeInDay` and `overlapLanes` get their first caller — five exports that
 * were written, table-tested and then used by nothing at all.
 *
 * **Each column measures itself.** `dayBounds`/`hoursInSpan` are asked per day
 * rather than once for the week, so the two transition days of the year get
 * 23 or 25 bands and their events are placed against their own span. The
 * gutter's labels come from the week's FIRST day, which is an ordinary
 * 24-hour day in every week that contains a transition — Sunday is the day
 * that changes and Monday leads the row — so on those weeks the short day's
 * lines sit a little off the gutter's. That is the truth rather than a
 * rendering fault, it is at most one twenty-fourth of the column, and the
 * badge in that column's heading is what names it. Forcing every column onto
 * a shared 24 would draw a 23-hour day as though it had 24 hours in it, which
 * is the error `hourMarks` was written to avoid.
 *
 * **A block can be dragged to another hour or another day**, which the design
 * deferred to v2 and the owner has since called in. This is the view where
 * that gesture can mean something exact — a month cell can only name a day —
 * and what a drop means lives in `drag.ts` rather than here. It adds to the
 * `datetime-local` in `DaySheet` and replaces nothing: dragging is a mouse
 * gesture, and the control remains the only way to name a time without one.
 */

export interface WeekGridProps {
  anchor: Date;
  occurrences: EventOccurrence[];
  now: Date;
  config: CalendarConfigView | undefined;
  selected: Slot;
  onSelect: (slot: Slot) => void;
  drag: DragHandlers;
}

export function WeekGrid({
  anchor,
  occurrences,
  now,
  config,
  selected,
  onSelect,
  drag,
}: WeekGridProps) {
  const days = weekOf(anchor);
  const byDay = groupByLocalDay(occurrences);
  /* The gutter is labelled from the first day of the row — see the header. */
  const [gutterStart, gutterEnd] = dayBounds(days[0]);
  const marks = hourMarks(gutterStart, gutterEnd);

  return (
    <div className="calendar-week-view">
      <div className="calendar-week-heads">
        <span className="calendar-gutter-head" />
        {days.map((day) => (
          <DayHead
            key={dateKeyOf(day)}
            day={day}
            now={now}
            working={isWorkingDay(day, config)}
            selected={sameDay(day, selected.day)}
            onSelect={onSelect}
          />
        ))}
      </div>

      <div className="calendar-week-body">
        <div className="calendar-gutter" aria-hidden="true">
          {marks.map((mark) => (
            <span
              className="calendar-gutter-hour"
              key={mark.hour}
              style={{ top: `${mark.fraction * 100}%` }}
            >
              {String(mark.hour).padStart(2, "0")}
            </span>
          ))}
        </div>

        {days.map((day) => (
          <DayColumn
            key={dateKeyOf(day)}
            day={day}
            occurrences={byDay.get(dateKeyOf(day)) ?? []}
            now={now}
            config={config}
            selected={selected}
            onSelect={onSelect}
            drag={drag}
          />
        ))}
      </div>
    </div>
  );
}

function DayHead({
  day,
  now,
  working,
  selected,
  onSelect,
}: {
  day: Date;
  now: Date;
  working: boolean;
  selected: boolean;
  onSelect: (slot: Slot) => void;
}) {
  const hours = dayHours(day);
  const classes = ["calendar-week-head"];
  if (!working) classes.push("calendar-week-head-closed");
  if (sameDay(day, now)) classes.push("calendar-week-head-today");
  if (selected) classes.push("calendar-week-head-selected");

  return (
    <button
      type="button"
      className={classes.join(" ")}
      aria-pressed={selected}
      onClick={() => onSelect({ day, hour: null })}
    >
      <span className="calendar-week-head-name">
        {day.toLocaleDateString(undefined, { weekday: "short" })}
      </span>
      <span className="calendar-week-head-number">{day.getDate()}</span>
      {(hours.short || hours.long) && (
        <span className="calendar-day-dst" title={`This local day is ${hours.hours} hours long`}>
          {hours.hours}h
        </span>
      )}
    </button>
  );
}

function DayColumn({
  day,
  occurrences,
  now,
  config,
  selected,
  onSelect,
  drag,
}: {
  day: Date;
  occurrences: EventOccurrence[];
  now: Date;
  config: CalendarConfigView | undefined;
  selected: Slot;
  onSelect: (slot: Slot) => void;
  drag: DragHandlers;
}) {
  const [start, end] = dayBounds(day);
  const slots = hourSlots(start, end);
  const marks = hourMarks(start, end);
  const working = isWorkingDay(day, config);
  const span = workingSpan(day, config);
  /*
    The wash is placed by the same function that places the events, against the
    same bounds. A second piece of arithmetic for the band would be a second
    thing that can disagree with the blocks drawn on top of it — and on a
    23-hour day, disagree visibly.
  */
  const wash = span === null ? null : placeInDay(span[0], span[1], start, end);

  /*
    Lanes over the whole column, not per hour: `overlapLanes` clusters by
    genuine overlap, so a 09:00 and a 15:00 both keep the full width and only
    the meetings that really collide are narrowed. That is the behaviour its
    own header promises and the reason the second pass in it exists.
  */
  const lanes = overlapLanes(
    occurrences,
    (occurrence) => placementOf(occurrence).startsAt,
    (occurrence) => placementOf(occurrence).endsAt,
  );

  const classes = ["calendar-column"];
  if (!working) classes.push("calendar-column-closed");

  return (
    <div className={classes.join(" ")}>
      {wash !== null && (
        <span
          className="calendar-work-wash"
          aria-hidden="true"
          style={{ top: `${wash.top * 100}%`, height: `${wash.height * 100}%` }}
        />
      )}

      {marks.map((mark, index) => (
        <span
          className="calendar-hour-line"
          aria-hidden="true"
          key={`${mark.hour}-${index}`}
          style={{ top: `${mark.fraction * 100}%` }}
        />
      ))}

      {/*
        An empty hour is a control, because design §6.14 asks for the draft form
        "inline no slot clicado" and a grid you can only read cannot offer one.
        Underneath the events rather than over them: a slot that swallowed the
        click on a meeting would make the events unreachable.
      */}
      {slots.map((slot, index) => {
        const classes = ["calendar-slot"];
        if (selected.hour === slot.hour && sameDay(day, selected.day)) {
          classes.push("calendar-slot-selected");
        }
        if (drag.dragging !== null) classes.push("calendar-slot-takes");
        return (
          <button
            type="button"
            key={`${slot.hour}-${index}`}
            className={classes.join(" ")}
            style={{ top: `${slot.top * 100}%`, height: `${slot.height * 100}%` }}
            aria-label={`${day.toLocaleDateString(undefined, {
              weekday: "long",
              day: "numeric",
              month: "long",
            })} at ${String(slot.hour).padStart(2, "0")}:00`}
            onClick={() => onSelect({ day, hour: slot.hour })}
            /* `preventDefault` on dragover is the opt-in: the event's default
               action is to refuse the drop. */
            onDragOver={(event) => {
              if (drag.dragging === null) return;
              event.preventDefault();
            }}
            onDrop={(event) => {
              if (drag.dragging === null) return;
              event.preventDefault();
              /*
                The band's OWN hour, read back from the instant by `hourSlots`
                — not the index. On the day the clocks go back there are two
                bands calling themselves 02, and both mean 02:00; on the day
                they go forward the eleventh band is 12, not 11.
              */
              drag.onDrop(day, slot.hour);
            }}
          />
        );
      })}

      {occurrences.map((occurrence) => (
        <Block
          key={occurrenceKey(occurrence.event_id, occurrence.occurrence_local)}
          occurrence={occurrence}
          dayStart={start}
          dayEnd={end}
          lane={lanes.get(occurrence) ?? { lane: 0, lanes: 1 }}
          onSelect={onSelect}
          day={day}
          drag={drag}
        />
      ))}

      <NowRule day={day} now={now} />
    </div>
  );
}

/** One occurrence, placed by its own instants and narrowed by its cluster. */
function Block({
  occurrence,
  dayStart,
  dayEnd,
  lane,
  day,
  onSelect,
  drag,
}: {
  occurrence: EventOccurrence;
  dayStart: Date;
  dayEnd: Date;
  lane: { lane: number; lanes: number };
  day: Date;
  onSelect: (slot: Slot) => void;
  drag: DragHandlers;
}) {
  const placement = placementOf(occurrence);
  const box = placeInDay(placement.startsAt, placement.endsAt, dayStart, dayEnd);
  if (box === null) return null;

  const held =
    drag.dragging !== null &&
    occurrenceKey(drag.dragging.event_id, drag.dragging.occurrence_local) ===
      occurrenceKey(occurrence.event_id, occurrence.occurrence_local);

  const width = 100 / lane.lanes;
  const classes = ["calendar-block"];
  if (occurrence.source === "proposal") classes.push("calendar-block-proposal");
  if (placement.moved) classes.push("calendar-block-moved");
  if (held) classes.push("calendar-block-held");

  return (
    <button
      type="button"
      className={classes.join(" ")}
      style={{
        top: `${box.top * 100}%`,
        height: `${box.height * 100}%`,
        left: `${lane.lane * width}%`,
        width: `${width}%`,
      }}
      onClick={() => onSelect({ day, hour: placement.startsAt.getHours() })}
      draggable
      onDragStart={(event) => {
        event.dataTransfer.setData(
          "text/plain",
          occurrenceKey(occurrence.event_id, occurrence.occurrence_local),
        );
        event.dataTransfer.effectAllowed = "move";
        drag.onDragStart(occurrence);
      }}
      onDragEnd={() => drag.onDragEnd()}
    >
      <span className="calendar-block-clock">{placement.clock}</span>
      <span className="calendar-block-title">{occurrence.title}</span>
      {occurrence.source === "proposal" && <span className="calendar-visually-hidden">proposed</span>}
      {placement.moved && <span className="calendar-visually-hidden">moved</span>}
    </button>
  );
}

/**
 * The current minute, as a rule across today's column only.
 *
 * `nowFraction` returns `null` off-day, which is what stops this being drawn
 * seven times across the week — its own header says so, and this is the caller
 * that makes the guarantee matter. `info` and not the accent: the time is a
 * fact with no verdict attached, and the accent is the brand, not a reading.
 */
export function NowRule({ day, now }: { day: Date; now: Date }) {
  const fraction = nowFraction(now, day);
  if (fraction === null) return null;
  return (
    <span
      className="calendar-now-rule"
      role="img"
      aria-label={`now, ${Math.round(fraction * 100)}% through today`}
      style={{ top: `${fraction * 100}%` }}
    />
  );
}
