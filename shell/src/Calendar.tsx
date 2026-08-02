import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  cancelCalendarOccurrence, createCalendarEvent, deleteCalendarEvent, getCalendarBusy,
  getCalendarConfig, getCalendarEvents, listPendingNotifications,
  type CalendarConfigView, type CalendarOccurrence, type ConnectionState,
  type NewCalendarEvent, type PendingNotification,
} from "./api";
import {
  dayBounds, hourMarks, hoursInSpan, localStamp, monthMatrix, nowFraction, overlapLanes,
  placeInDay, sameDay, weekOf,
} from "./calendar-grid";
import { relativeTime } from "./derive";
import { Badge, Button, ConfirmButton, ErrorNote } from "./ui";

type View = "month" | "week";

/** How tall one hour is in the week grid. Sized so a 45-minute block still fits its title. */
const HOUR_PX = 44;

/** Where the week view scrolls to on open — early enough to catch an 08:00, late enough to skip the night. */
const OPENING_HOUR = 7;

/** Chips a month cell shows before it collapses the rest into a count. */
const CHIPS_PER_CELL = 3;

const REFRESH_MS = 30_000;
/** The now line runs on its own clock: tying it to the 30s data refresh would make it twitch, not glide. */
const NOW_TICK_MS = 60_000;

const WEEKDAYS = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
const MINUTES_IN_DAY = 1440;

interface CalendarProps {
  token: string | null;
  connection: ConnectionState;
}

function monthTitle(anchor: Date): string {
  return anchor.toLocaleDateString([], { month: "long", year: "numeric" });
}

/** "3 – 9 August 2026", dropping the month from the first date when both ends share it. */
function weekTitle(days: Date[]): string {
  const first = days[0];
  const last = days[days.length - 1];
  const sameMonth = first.getMonth() === last.getMonth();
  const from = first.toLocaleDateString(
    [],
    sameMonth ? { day: "numeric" } : { day: "numeric", month: "short" },
  );
  const to = last.toLocaleDateString([], { day: "numeric", month: "long", year: "numeric" });
  return `${from} – ${to}`;
}

function clockOf(iso: string): string {
  return new Date(iso).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
}

/** Minutes past midnight, for the working-hours wash. `09:00` → 540. */
function minutesOf(clock: string): number {
  const [hour, minute] = clock.split(":").map(Number);
  return (hour || 0) * 60 + (minute || 0);
}

export default function Calendar({ token, connection }: CalendarProps) {
  const [view, setView] = useState<View>("week");
  const [anchor, setAnchor] = useState(() => new Date());
  const [now, setNow] = useState(() => new Date());
  const [occurrences, setOccurrences] = useState<CalendarOccurrence[] | null>(null);
  const [config, setConfig] = useState<CalendarConfigView | null>(null);
  const [busyNow, setBusyNow] = useState<boolean | null>(null);
  const [held, setHeld] = useState<PendingNotification[]>([]);
  const [draft, setDraft] = useState<{ day: Date; hour: number } | null>(null);
  const [working, setWorking] = useState(false);
  const [failed, setFailed] = useState<string | null>(null);
  const scroller = useRef<HTMLDivElement | null>(null);
  const opened = useRef(false);

  const usable = connection === "connected" && token !== null;
  const weeks = useMemo(() => (view === "month" ? monthMatrix(anchor) : []), [view, anchor]);
  const days = useMemo(
    () => (view === "week" ? weekOf(anchor) : weeks.flat()),
    [view, anchor, weeks],
  );

  const refresh = useCallback(async () => {
    if (token === null || days.length === 0) return;
    const [from] = dayBounds(days[0]);
    const [, to] = dayBounds(days[days.length - 1]);
    const [found, busy, pending] = await Promise.all([
      getCalendarEvents(token, from, to),
      getCalendarBusy(token),
      listPendingNotifications(token),
    ]);
    setOccurrences(found);
    setBusyNow(busy);
    setHeld(pending);
  }, [token, days]);

  useEffect(() => {
    if (!usable) return;
    void refresh();
    const timer = window.setInterval(() => void refresh(), REFRESH_MS);
    return () => window.clearInterval(timer);
  }, [usable, refresh]);

  useEffect(() => {
    if (!usable || token === null) return;
    void getCalendarConfig(token).then(setConfig);
  }, [usable, token]);

  useEffect(() => {
    const timer = window.setInterval(() => setNow(new Date()), NOW_TICK_MS);
    return () => window.clearInterval(timer);
  }, []);

  // Scrolled once, on the first paint of the week grid. Doing it on every render would yank the
  // view back to 07:00 each time the 30-second refresh landed, mid-read.
  useEffect(() => {
    if (view !== "week" || opened.current || scroller.current === null) return;
    scroller.current.scrollTop = OPENING_HOUR * HOUR_PX;
    opened.current = true;
  }, [view, occurrences]);

  const move = (direction: -1 | 1) => {
    setAnchor((current) =>
      view === "week"
        ? new Date(current.getFullYear(), current.getMonth(), current.getDate() + 7 * direction)
        : new Date(current.getFullYear(), current.getMonth() + direction, 1),
    );
  };

  const byDay = useMemo(() => {
    const map = new Map<string, CalendarOccurrence[]>();
    for (const occurrence of occurrences ?? []) {
      const start = new Date(occurrence.starts_at);
      const key = `${start.getFullYear()}-${start.getMonth()}-${start.getDate()}`;
      const existing = map.get(key);
      if (existing) existing.push(occurrence);
      else map.set(key, [occurrence]);
    }
    return map;
  }, [occurrences]);

  const onDay = useCallback(
    (day: Date) => byDay.get(`${day.getFullYear()}-${day.getMonth()}-${day.getDate()}`) ?? [],
    [byDay],
  );

  const add = useCallback(
    async (event: NewCalendarEvent) => {
      if (token === null) return;
      setWorking(true);
      setFailed(null);
      const created = await createCalendarEvent(token, event);
      if (created.ok) {
        setDraft(null);
        await refresh();
      } else {
        setFailed(created.reason);
      }
      setWorking(false);
    },
    [token, refresh],
  );

  const skipOne = useCallback(
    async (occurrence: CalendarOccurrence) => {
      if (token === null) return;
      setWorking(true);
      const cancelled = await cancelCalendarOccurrence(
        token,
        occurrence.event_id,
        occurrence.occurrence_local,
      );
      if (!cancelled) setFailed("that occurrence could not be cancelled");
      await refresh();
      setWorking(false);
    },
    [token, refresh],
  );

  const removeSeries = useCallback(
    async (id: number) => {
      if (token === null) return;
      setWorking(true);
      if (!(await deleteCalendarEvent(token, id))) setFailed("the event could not be deleted");
      await refresh();
      setWorking(false);
    },
    [token, refresh],
  );

  if (!usable) {
    return (
      <section className="cal">
        <p className="cal-flat">The daemon is not reachable, so the calendar cannot be read.</p>
      </section>
    );
  }

  const stillHeld = held.filter((notification) => notification.delivered_at === null);
  const workFrom = minutesOf(config?.working_hours_start ?? "09:00");
  const workTo = minutesOf(config?.working_hours_end ?? "18:00");

  return (
    <section className="cal">
      <header className="cal-head">
        <h2 className="cal-title">{view === "week" ? weekTitle(days) : monthTitle(anchor)}</h2>

        <div className="cal-switch" role="group" aria-label="Calendar view">
          {(["month", "week"] as View[]).map((option) => (
            <button
              key={option}
              type="button"
              className="cal-switch-btn"
              aria-pressed={view === option}
              onClick={() => setView(option)}
            >
              {option}
            </button>
          ))}
        </div>

        <div className="cal-nav">
          <button type="button" className="cal-step" aria-label="Previous" onClick={() => move(-1)}>
            ‹
          </button>
          <button type="button" className="cal-today" onClick={() => setAnchor(new Date())}>
            Today
          </button>
          <button type="button" className="cal-step" aria-label="Next" onClick={() => move(1)}>
            ›
          </button>
          {/* Clicking a slot is the fast way in, and it is mouse-only. This is the same door for
              anyone on a keyboard: it opens the draft on the next whole hour from now. */}
          <button
            type="button"
            className="cal-today"
            onClick={() => {
              setFailed(null);
              setDraft({ day: new Date(), hour: Math.min(23, now.getHours() + 1) });
            }}
          >
            New event
          </button>
        </div>

        {/* The answer to "am I reachable" — loud when busy, almost absent when not. */}
        <p className="cal-now" aria-live="polite">
          {busyNow === null ? null : busyNow ? (
            <Badge tone="paused">busy</Badge>
          ) : (
            <span className="cal-free">free</span>
          )}
        </p>
      </header>

      {failed !== null && <ErrorNote>{failed}</ErrorNote>}

      {/* Exceptions dominate; the normal disappears. One quiet line when nothing waits, a raised
          surface the moment something does. */}
      {stillHeld.length > 0 ? (
        <div className="cal-held" role="status">
          <strong>
            {stillHeld.length} notification{stillHeld.length === 1 ? "" : "s"} waiting
          </strong>
          <span>held while you are busy — they go out when the calendar opens</span>
          <ul>
            {stillHeld.slice(0, 3).map((notification) => (
              <li key={notification.id}>
                {notification.summary}
                <em>{relativeTime(notification.queued_at)}</em>
              </li>
            ))}
          </ul>
        </div>
      ) : (
        <p className="cal-quiet">
          {held.length === 0
            ? "Nothing has ever been held back."
            : `Nothing waiting. ${held.length} delivered after a meeting ended.`}
        </p>
      )}

      {draft !== null && (
        <DraftEvent
          day={draft.day}
          hour={draft.hour}
          tz={config?.default_tz ?? ""}
          busy={working}
          onSubmit={add}
          onCancel={() => setDraft(null)}
        />
      )}

      {occurrences === null ? (
        <p className="cal-flat">Reading the agenda…</p>
      ) : view === "week" ? (
        <div className="cal-week-scroll" ref={scroller}>
          <div className="cal-week">
            {/* The empty head cell is load-bearing: without it the hour labels position from the
                top of the grid and the first few sit inside the day-name band. */}
            <div className="cal-gutter" aria-hidden="true">
              <div className="cal-col-head" />
              <div className="cal-gutter-body" style={{ height: `${24 * HOUR_PX}px` }}>
                {Array.from({ length: 24 }, (_, hour) => (
                  <span key={hour} style={{ top: `${hour * HOUR_PX}px` }}>
                    {hour === 0 ? "" : String(hour).padStart(2, "0")}
                  </span>
                ))}
              </div>
            </div>
            {days.map((day) => (
              <WeekColumn
                key={day.toDateString()}
                day={day}
                now={now}
                events={onDay(day)}
                workFrom={workFrom}
                workTo={workTo}
                working={config?.working_weekdays ?? WEEKDAYS.slice(0, 5)}
                busy={working}
                onPick={(hour) => {
                  setFailed(null);
                  setDraft({ day, hour });
                }}
                onSkip={skipOne}
                onDelete={removeSeries}
              />
            ))}
          </div>
        </div>
      ) : (
        <div className="cal-month" role="grid" aria-label={monthTitle(anchor)}>
          <div className="cal-month-head" role="row">
            {WEEKDAYS.map((label) => (
              <span key={label} role="columnheader">
                {label}
              </span>
            ))}
          </div>
          {weeks.map((week) => (
            <div className="cal-month-row" role="row" key={week[0].toDateString()}>
              {week.map((day) => (
                <MonthCell
                  key={day.toDateString()}
                  day={day}
                  month={anchor.getMonth()}
                  today={now}
                  working={config?.working_weekdays ?? WEEKDAYS.slice(0, 5)}
                  events={onDay(day)}
                  onPick={() => {
                    setFailed(null);
                    setDraft({ day, hour: 9 });
                  }}
                  onOpenWeek={() => {
                    setAnchor(day);
                    setView("week");
                  }}
                />
              ))}
            </div>
          ))}
        </div>
      )}
    </section>
  );
}

interface WeekColumnProps {
  day: Date;
  now: Date;
  events: CalendarOccurrence[];
  workFrom: number;
  workTo: number;
  /** Weekday names the daemon counts as working days, from `/calendar/config`. */
  working: string[];
  busy: boolean;
  onPick: (hour: number) => void;
  onSkip: (occurrence: CalendarOccurrence) => void;
  onDelete: (id: number) => void;
}

function WeekColumn({
  day, now, events, workFrom, workTo, working, busy, onPick, onSkip, onDelete,
}: WeekColumnProps) {
  const [start, end] = dayBounds(day);
  const hours = hoursInSpan(start, end);
  const marks = hourMarks(start, end);
  const line = nowFraction(now, day);
  const isToday = sameDay(now, day);
  const name = WEEKDAYS[(day.getDay() + 6) % 7];
  // A day off is dimmed whole rather than washed hour by hour: the working-hours band inside it
  // would be a statement about a day that has none.
  const isOff = !working.some((entry) => entry.toLowerCase().startsWith(name.toLowerCase()));

  const lanes = overlapLanes(
    events,
    (event) => new Date(event.starts_at),
    (event) => new Date(event.ends_at),
  );

  return (
    <div className={`cal-col${isToday ? " is-today" : ""}${isOff ? " is-off" : ""}`}>
      <div className="cal-col-head">
        <span className="cal-dow">{name}</span>
        <span className="cal-dom">{day.getDate()}</span>
        {/* The two days a year this column is not 24 hours tall. Saying so costs a badge; not
            saying so costs someone discovering their meetings sit an hour off the ruler. */}
        {hours !== 24 && (
          <span className="cal-dst" title="Daylight saving change">
            {hours} h
          </span>
        )}
      </div>

      <div
        className="cal-col-body"
        style={{ height: `${hours * HOUR_PX}px` }}
        onClick={(clicked) => {
          if (busy) return;
          const box = clicked.currentTarget.getBoundingClientRect();
          const fraction = (clicked.clientY - box.top) / box.height;
          onPick(Math.max(0, Math.min(hours - 1, Math.floor(fraction * hours))));
        }}
      >
        {/* Outside working hours. Not a rule and not a block — a hint about where the agent may
            propose, which is a different question from whether you are busy. A day that is not a
            working day at all is dimmed as a whole instead. */}
        {!isOff && (
          <>
            <div className="cal-offhours" style={{ top: 0, height: `${(workFrom / MINUTES_IN_DAY) * 100}%` }} />
            <div className="cal-offhours" style={{ top: `${(workTo / MINUTES_IN_DAY) * 100}%`, bottom: 0 }} />
          </>
        )}

        {marks.map((mark) => (
          <div
            key={mark.fraction}
            className="cal-hourline"
            style={{ top: `${mark.fraction * 100}%` }}
          />
        ))}

        {line !== null && (
          <div className="cal-nowline" style={{ top: `${line * 100}%` }} aria-hidden="true" />
        )}

        {events.map((event) => {
          const place = placeInDay(new Date(event.starts_at), new Date(event.ends_at), start, end);
          if (place === null) return null;
          const lane = lanes.get(event) ?? { lane: 0, lanes: 1 };
          const width = 100 / lane.lanes;
          // Under ~40 pixels the title and the time cannot both fit, and a half-clipped clock is
          // worse than no clock: the title is the part you cannot infer from the position.
          const short = place.height * hours * HOUR_PX < 40;
          return (
            <article
              key={`${event.event_id}-${event.occurrence_local}`}
              className={
                `cal-block${event.source === "proposal" ? " is-proposed" : ""}` +
                `${short ? " is-short" : ""}`
              }
              style={{
                top: `${place.top * 100}%`,
                height: `${place.height * 100}%`,
                left: `${lane.lane * width}%`,
                width: `${width}%`,
              }}
              onClick={(clicked) => clicked.stopPropagation()}
            >
              <b>{event.title}</b>
              <time>{clockOf(event.starts_at)}</time>
              <span className="cal-block-do">
                <Button variant="ghost" size="sm" disabled={busy} onClick={() => onSkip(event)}>
                  Skip
                </Button>
                <ConfirmButton
                  variant="danger"
                  size="sm"
                  confirmLabel="Delete series?"
                  disabled={busy}
                  onConfirm={() => onDelete(event.event_id)}
                >
                  Delete
                </ConfirmButton>
              </span>
            </article>
          );
        })}
      </div>
    </div>
  );
}

interface MonthCellProps {
  day: Date;
  month: number;
  today: Date;
  working: string[];
  events: CalendarOccurrence[];
  onPick: () => void;
  onOpenWeek: () => void;
}

function MonthCell({ day, month, today, working, events, onPick, onOpenWeek }: MonthCellProps) {
  const outside = day.getMonth() !== month;
  const isToday = sameDay(today, day);
  const name = WEEKDAYS[(day.getDay() + 6) % 7];
  // Dimmed on the same rule as the week view. Two grids disagreeing about which days are yours
  // would make the quieter one look like a rendering bug.
  const isOff = !working.some((entry) => entry.toLowerCase().startsWith(name.toLowerCase()));
  const shown = events.slice(0, CHIPS_PER_CELL);
  const rest = events.length - shown.length;

  return (
    <div
      className={
        `cal-cell${outside ? " is-outside" : ""}${isToday ? " is-today" : ""}` +
        `${isOff ? " is-off" : ""}`
      }
      role="gridcell"
      onClick={onPick}
    >
      <span className="cal-cell-num">{day.getDate()}</span>
      {shown.map((event) => (
        <span
          key={`${event.event_id}-${event.occurrence_local}`}
          className={`cal-chip${event.source === "proposal" ? " is-proposed" : ""}`}
          title={event.title}
        >
          <time>{clockOf(event.starts_at)}</time>
          {event.title}
        </span>
      ))}
      {rest > 0 && (
        <button
          type="button"
          className="cal-more"
          onClick={(clicked) => {
            clicked.stopPropagation();
            onOpenWeek();
          }}
        >
          {rest} more
        </button>
      )}
    </div>
  );
}

interface DraftEventProps {
  day: Date;
  hour: number;
  tz: string;
  busy: boolean;
  onSubmit: (event: NewCalendarEvent) => void;
  onCancel: () => void;
}

/**
 * The one raised surface over the grid, and therefore the one place glass belongs here.
 *
 * Not a modal: choosing a title while the day you just clicked is hidden behind a scrim is a worse
 * version of the same form.
 */
function DraftEvent({ day, hour, tz, busy, onSubmit, onCancel }: DraftEventProps) {
  const [title, setTitle] = useState("");
  const [minutes, setMinutes] = useState(60);
  const [freq, setFreq] = useState<"" | "daily" | "weekly" | "monthly">("");
  const field = useRef<HTMLInputElement | null>(null);

  useEffect(() => {
    field.current?.focus();
  }, []);

  const when = day.toLocaleDateString([], { weekday: "long", day: "numeric", month: "long" });

  return (
    <form
      className="cal-draft"
      onSubmit={(submitted) => {
        submitted.preventDefault();
        if (title.trim() === "" || busy) return;
        onSubmit({
          title: title.trim(),
          starts_at_local: localStamp(day, hour),
          duration_minutes: minutes,
          tz: tz === "" ? undefined : tz,
          ...(freq === "" ? {} : { freq }),
        });
      }}
      onKeyDown={(pressed) => {
        if (pressed.key === "Escape") onCancel();
      }}
    >
      <p className="cal-draft-when">
        {when}, {String(hour).padStart(2, "0")}:00
      </p>
      <input
        ref={field}
        className="cal-draft-title"
        value={title}
        placeholder="What is it?"
        onChange={(typed) => setTitle(typed.target.value)}
      />
      <label>
        <span>Minutes</span>
        <input
          type="number"
          min={1}
          value={minutes}
          onChange={(typed) => setMinutes(Number(typed.target.value))}
        />
      </label>
      <label>
        <span>Repeats</span>
        <select value={freq} onChange={(chosen) => setFreq(chosen.target.value as typeof freq)}>
          <option value="">never</option>
          <option value="daily">daily</option>
          <option value="weekly">weekly</option>
          <option value="monthly">monthly</option>
        </select>
      </label>
      <div className="cal-draft-do">
        <Button type="submit" variant="approve" disabled={title.trim() === "" || busy}>
          Add
        </Button>
        <Button variant="ghost" onClick={onCancel} disabled={busy}>
          Cancel
        </Button>
      </div>
    </form>
  );
}
