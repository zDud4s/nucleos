// §spec calendario-local
import { useEffect, useState } from "react";
import { useNavigate, useSearch } from "@tanstack/react-router";
import { DaySheet } from "../calendar/DaySheet";
import { MonthGrid } from "../calendar/MonthGrid";
import { WeekGrid } from "../calendar/WeekGrid";
import { dateKeyOf, groupByLocalDay, placementOf, type Slot } from "../calendar/slot";
import { isApiRefusal } from "../data/client";
import {
  useBusy,
  useCalendarConfig,
  useCalendarEvents,
  useMoveOccurrence,
  type CalendarConfigView,
  type EventOccurrence,
} from "../data/calendar";
import { moveFromDrop } from "../calendar/drag";
import { isHeld, usePendingNotifications, type PendingNotification } from "../data/feed";
import { dayBounds, monthMatrix, weekOf } from "../lib/calendar-grid";
import { UI_LOCALE } from "../lib/locale";
import {
  Badge,
  Button,
  ErrorNote,
  PageHeader,
  Panel,
  Quiet,
  RefusalNote,
  RelativeTime,
  Row,
  Rows,
  Section,
} from "../ui";
import "./calendar.css";

/**
 * Calendar — a month or a week, the day you have selected, and the
 * notifications the calendar held back while you looked busy.
 *
 * **The grid selects and the sheet acts.** These were two disconnected halves:
 * a grid with no click on it, and below it a flat list of every occurrence in
 * the six-week window with four controls each. Now a cell selects a day, a
 * week slot selects a day and an hour, and `DaySheet` is the one place
 * anything is done — including drafting, which design §6.14 asks for "inline
 * no slot clicado".
 *
 * **Both views, because the month cannot answer the questions the week can.**
 * Overlap lanes, the working-hours wash and a rule at the current minute all
 * need a vertical time axis; §6.14 asks for all three, and
 * `lib/calendar-grid.ts` had the arithmetic for every one of them written and
 * table-tested with no caller. `WeekGrid` is that caller.
 *
 * **The DST badge is computed here, not asked for.** `GET /calendar/config`
 * carries no such field (`data/calendar.ts`'s header), so `dayHours` reads it
 * off the day itself via `lib/calendar-grid.ts`'s `dayBounds`/`hoursInSpan` —
 * a local calendar day whose real span is 23 or 25 hours, not 24.
 *
 * **Dragging moves an occurrence**, which design §6.14's Notes deferred to v2
 * and the owner has since called in. It is an addition and never a
 * replacement: the `datetime-local` in `DaySheet` is still how an exact time
 * is named, and is the only way for anyone not using a mouse. What a drop
 * means lives in `calendar/drag.ts`, away from either grid.
 */

type View = "month" | "week";

/**
 * The two things about this page that are part of the location.
 *
 * Which view, and which day it is looking at. Everything else — the draft in
 * progress, whether the recurrence toggle is open — is genuinely transient and
 * has no business in a URL.
 *
 * It earns its place twice over. A calendar you can link to is worth having on
 * its own, and it is the only way the four surfaces worth photographing are
 * reachable at all: the screenshot harness navigates, and the week of a
 * transition is otherwise four clicks deep in state no `?path=` can express.
 * The inspector reached the same conclusion for the same reason — its own
 * shots became a reload rather than a click the harness had to fake.
 */
export interface CalendarSearch {
  view?: View;
  /** `"YYYY-MM-DD"` — the day to open on and select. */
  on?: string;
}

export function validateCalendarSearch(search: Record<string, unknown>): CalendarSearch {
  const view = search.view === "week" || search.view === "month" ? search.view : undefined;
  const on = typeof search.on === "string" && /^\d{4}-\d{2}-\d{2}$/.test(search.on) ? search.on : undefined;
  return { ...(view === undefined ? {} : { view }), ...(on === undefined ? {} : { on }) };
}

/**
 * `"2026-03-29"` as a local day, or today.
 *
 * Split and constructed rather than passed to `new Date("2026-03-29")`, which
 * ECMAScript reads as **UTC midnight** for the date-only form — one time zone
 * west of Greenwich and the calendar would open on the 28th.
 */
function dayFromSearch(on: string | undefined): Date {
  if (on === undefined) return new Date();
  const [year, month, day] = on.split("-").map(Number);
  return new Date(year, month - 1, day);
}

/**
 * A clock of its own, at design §6.14's 60 s.
 *
 * The page had none: `now` was a bare `new Date()` evaluated during render, so
 * the "now" reading advanced only when something *else* re-rendered the page
 * — in practice the busy poll, which ran at 3 s and was therefore quietly
 * acting as the clock. Two things were wrong with that. The reading was
 * hostage to an unrelated query's cadence, and the cadence itself was ten
 * times what the design asked for. With this here, `useBusy` could be slowed
 * to the 30 s §6.14 pairs it with.
 *
 * A minute is the resolution the reading actually has: the month draws a
 * progress track and the week a rule, and neither can show a second.
 */
export function useMinuteClock(period = 60_000): Date {
  const [now, setNow] = useState(() => new Date());
  useEffect(() => {
    const timer = setInterval(() => setNow(new Date()), period);
    return () => clearInterval(timer);
  }, [period]);
  return now;
}

function startOfMonth(date: Date): Date {
  return new Date(date.getFullYear(), date.getMonth(), 1);
}

/**
 * The RFC 3339 window the visible grid needs.
 *
 * The month asks for the full six weeks `monthMatrix` draws rather than the
 * calendar month, since the grid always shows a little of the months either
 * side; the week asks for its seven days. Both are built from `dayBounds`, so
 * the bounds a query asks for and the bounds a column is drawn against are the
 * same arithmetic.
 */
export function visibleWindow(anchor: Date, view: View): { from: string; to: string } {
  const days = view === "week" ? weekOf(anchor) : monthMatrix(anchor).flat();
  const [from] = dayBounds(days[0]);
  const [, to] = dayBounds(days[days.length - 1]);
  return { from: from.toISOString(), to: to.toISOString() };
}

/**
 * What the page says it is showing, and how much of it.
 *
 * Counted over the period the label NAMES, which it was not: the query covers
 * the six weeks the grid draws, and the headline counted all of them while
 * saying "this month" — up to twelve days of other months folded into the
 * number. A month grid that overstates its own month is a small lie told on
 * every page load.
 */
export function headline(
  rows: EventOccurrence[],
  anchor: Date,
  view: View,
  config: CalendarConfigView | undefined,
): string | undefined {
  if (config === undefined) return undefined;
  const inPeriod =
    view === "week"
      ? rows
      : rows.filter((row) => placementOf(row).day.getMonth() === anchor.getMonth());
  const noun = inPeriod.length === 1 ? "occurrence" : "occurrences";
  const period = view === "week" ? "this week" : "this month";
  return `${inPeriod.length} ${noun} ${period} — working hours ${config.working_hours_start}–${config.working_hours_end}, ${config.default_tz}`;
}

export function Calendar() {
  const now = useMinuteClock();
  /*
    Validated HERE, and not merely at the route.

    `useSearch({ strict: false })` is unvalidated by definition — it hands back
    whatever is in the location, and `as CalendarSearch` is a claim rather than
    a check. The route's own `validateSearch` covers the app, but this
    component is also mounted directly by tests and by the preview harness, and
    a `?on=yesterday` reaching `dayFromSearch` unfiltered is a `RangeError` out
    of `toISOString` — a white screen behind an error boundary, from a URL.
    Running the page's own validator on the way in costs nothing and makes the
    component correct wherever it is mounted.
  */
  const search = validateCalendarSearch(useSearch({ strict: false }) as Record<string, unknown>);
  const navigate = useNavigate();

  const [view, setView] = useState<View>(search.view ?? "month");
  const [anchor, setAnchor] = useState(() => {
    const day = dayFromSearch(search.on);
    return (search.view ?? "month") === "week" ? day : startOfMonth(day);
  });
  const [selected, setSelected] = useState<Slot>(() => ({ day: dayFromSearch(search.on), hour: null }));

  /**
   * Keep the location saying what is on screen.
   *
   * `replace`, so that paging through a year leaves one entry in the history
   * rather than twelve — the rail's back gesture should return you to the page
   * you came from, not to August.
   */
  function publish(nextView: View, day: Date) {
    void navigate({
      to: "/calendar",
      replace: true,
      search: validateCalendarSearch({ view: nextView, on: dateKeyOf(day) }),
    });
  }

  /**
   * Select a day — and say so in the location.
   *
   * Paging published and selecting did not, which left `?on=` naming a day the
   * page had stopped showing the moment anybody clicked a cell. A URL that is
   * right until you touch the page is worse than one that was never there:
   * copying the link would hand somebody a different day from the one on
   * screen. The hour deliberately stays out of it — it seeds a draft and is
   * gone the moment the draft is submitted.
   */
  function select(slot: Slot) {
    setSelected(slot);
    publish(view, slot.day);
  }

  /**
   * The occurrence currently under the hand, and what a drop does with it.
   *
   * Held here rather than in either grid because the grids are peers and the
   * mutation is the page's: `DaySheet` owns the same write for its
   * `datetime-local`, and two components asking for `useMoveOccurrence`
   * separately would be two caches to invalidate.
   *
   * React state and not only `dataTransfer`, because the payload has to be
   * readable during `dragover` — which is where a drop target decides whether
   * to light up — and every browser deliberately blanks `dataTransfer` on
   * that event so a page cannot read what it is not yet holding. The transfer
   * still carries the key, so the drag is a real drag to the operating system
   * rather than a mousedown this page is pretending about.
   */
  const [dragging, setDragging] = useState<EventOccurrence | null>(null);
  const move = useMoveOccurrence();

  function drop(day: Date, hour: number | null) {
    const occurrence = dragging;
    setDragging(null);
    if (occurrence === null) return;

    const next = moveFromDrop(occurrence, day, hour);
    // `null` is a drop that changes nothing — picked up and put back.
    if (next === null) return;

    move.mutate({
      eventId: occurrence.event_id,
      // The ORIGINAL local start, always. Never `next.toLocal`.
      occurrenceLocal: occurrence.occurrence_local,
      toLocal: next.toLocal,
      durationMinutes: next.durationMinutes,
    });
    select({ day, hour });
  }

  const drag = { dragging, onDragStart: setDragging, onDragEnd: () => setDragging(null), onDrop: drop };

  const { from, to } = visibleWindow(anchor, view);
  const events = useCalendarEvents(from, to);
  const busy = useBusy();
  const config = useCalendarConfig();
  const rows = events.data ?? [];
  const byDay = groupByLocalDay(rows);

  /**
   * Move the grid, and take the selection with it.
   *
   * A selection left behind is the defect this avoids: page to December and
   * the sheet would still be showing a day in August, with a heading that
   * disagrees with everything on screen. The first of the period is the
   * honest landing place — except when that period contains today, which is
   * the day somebody paging back to *now* means.
   */
  function goTo(next: Date, nextView: View = view) {
    setAnchor(next);
    const days = nextView === "week" ? weekOf(next) : monthMatrix(next).flat();
    const today = days.find((day) => dateKeyOf(day) === dateKeyOf(now));
    const first =
      nextView === "week" ? days[0] : (days.find((day) => day.getMonth() === next.getMonth()) ?? days[0]);
    const landing = today ?? first;
    setSelected({ day: landing, hour: null });
    publish(nextView, landing);
  }

  function switchTo(nextView: View) {
    setView(nextView);
    setAnchor(nextView === "week" ? selected.day : startOfMonth(selected.day));
    publish(nextView, selected.day);
  }

  function step(direction: -1 | 1) {
    goTo(
      view === "week"
        ? new Date(anchor.getFullYear(), anchor.getMonth(), anchor.getDate() + 7 * direction)
        : new Date(anchor.getFullYear(), anchor.getMonth() + direction, 1),
    );
  }

  const label =
    view === "week"
      ? weekLabel(weekOf(anchor))
      : anchor.toLocaleDateString(UI_LOCALE, { month: "long", year: "numeric" });

  return (
    <>
      {/* The page's kind, not a link: this IS the calendar, and a crumb that points at
          the page you are on is a door to the room you are standing in. It is here so
          that the heading can be spent on the period, which is the thing that changes. */}
      <p className="mb-2 text-xs text-text-faint">Calendar</p>
      <PageHeader
        /*
          "August 2026" and not "Calendar". Paging is the gesture this page is used
          through — Prev, Next, Month, Week — and after every one of them the largest
          words on the screen said the same thing they said before the press. The period
          is what the press changed, so the period takes the rank.
        */
        title={label}
        headline={headline(rows, anchor, view, config.data)}
        actions={
          <div className="calendar-nav">
            <BusyIndicator busy={busy.data?.busy} />
            <div className="calendar-views" role="group" aria-label="Calendar view">
              <button
                type="button"
                className={view === "month" ? "calendar-view calendar-view-on" : "calendar-view"}
                aria-pressed={view === "month"}
                onClick={() => switchTo("month")}
              >
                Month
              </button>
              <button
                type="button"
                className={view === "week" ? "calendar-view calendar-view-on" : "calendar-view"}
                aria-pressed={view === "week"}
                onClick={() => switchTo("week")}
              >
                Week
              </button>
            </div>
            <Button title={view === "week" ? "Previous week" : "Previous month"} onClick={() => step(-1)}>
              ‹ Prev
            </Button>
            <Button
              title="Back to today"
              onClick={() => goTo(view === "week" ? new Date() : startOfMonth(new Date()))}
            >
              Today
            </Button>
            <Button title={view === "week" ? "Next week" : "Next month"} onClick={() => step(1)}>
              Next ›
            </Button>
          </div>
        }
      />

      <Panel>
        {events.isError && rows.length === 0 && <EventsError error={events.error} />}
        {events.data === undefined && !events.isError && <p className="calendar-loading">reading the month…</p>}
        {events.data !== undefined &&
          (view === "week" ? (
            <WeekGrid
              anchor={anchor}
              occurrences={rows}
              now={now}
              config={config.data}
              selected={selected}
              onSelect={select}
              drag={drag}
            />
          ) : (
            <MonthGrid
              anchor={anchor}
              occurrences={rows}
              now={now}
              config={config.data}
              selected={selected}
              onSelect={select}
              drag={drag}
            />
          ))}
        {move.isError && <MoveError error={move.error} />}
      </Panel>

      {/* Untitled, because the sheet inside already names the day it is showing — with
          the marks that qualify it — as its own heading. "Selected day" above that was a
          label for a thing the next line said better, and it was the same label on every
          day of the year. */}
      <Panel>
        <DaySheet
          slot={selected}
          occurrences={byDay.get(dateKeyOf(selected.day)) ?? []}
          now={now}
          config={config.data}
        />
      </Panel>

      <HeldNotifications />
    </>
  );
}

/** "17–23 August 2026", collapsing whatever the two ends already share. */
function weekLabel(days: Date[]): string {
  const first = days[0];
  const last = days[days.length - 1];
  const sameMonth = first.getMonth() === last.getMonth() && first.getFullYear() === last.getFullYear();
  const tail = last.toLocaleDateString(UI_LOCALE, { day: "numeric", month: "long", year: "numeric" });
  const head = sameMonth
    ? String(first.getDate())
    : first.toLocaleDateString(UI_LOCALE, { day: "numeric", month: "long" });
  return `${head}–${tail}`;
}

function EventsError({ error }: { error: unknown }) {
  if (isApiRefusal(error)) return <RefusalNote refusal={error} />;
  return <ErrorNote>the núcleo did not answer — nothing is known about this month</ErrorNote>;
}

/**
 * A refusal on a drag, said next to the grid it happened on.
 *
 * Reported here and not inside the block that was dragged, because after a
 * failed move the block is back where it started and there is nothing left on
 * screen pointing at it — a message attached to it would appear in whichever
 * cell the person was no longer looking at. The `DaySheet` reports the same
 * write beside its own control, where there IS something to attach it to.
 */
function MoveError({ error }: { error: unknown }) {
  if (isApiRefusal(error)) return <RefusalNote refusal={error} />;
  return <ErrorNote>the núcleo did not answer — nothing was moved</ErrorNote>;
}

/* ------------------------------------------------------------------ busy -- */

/**
 * Whether this machine currently reads as busy — `GET /calendar/busy` never
 * refuses, so there is no error branch here, only "not answered yet".
 */
export function BusyIndicator({ busy }: { busy: boolean | undefined }) {
  if (busy === undefined) return null;
  // Being busy is a fact with no verdict attached, not an Awaiting-You Amber summons.
  return <Badge tone={busy ? "info" : "off"}>{busy ? "busy right now" : "free right now"}</Badge>;
}

/* ----------------------------------------------------- held notifications -- */

/**
 * What the calendar has held back because you looked busy, and what it held
 * and later let through — kept apart, never merged, because they answer
 * different questions: "what is waiting" and "did anything get swallowed".
 *
 * `usePendingNotifications` is reused from `data/feed.ts` rather than
 * declared again here.
 */
export function HeldNotifications() {
  const pending = usePendingNotifications();
  const rows = pending.data ?? [];
  const held = rows.filter(isHeld);
  const released = rows.filter((row) => !isHeld(row));

  return (
    <Panel title="Held notifications" variant="dim">
      <p className="calendar-note">
        calendar.rs fails OPEN by design — a database it cannot read answers "not busy" rather than
        staying silent, since silence here is a message that never arrived.
      </p>
      {pending.isError && rows.length === 0 && <NotificationsError error={pending.error} />}
      {pending.data !== undefined && rows.length === 0 && <Quiet says="nothing has been held." />}
      {/*
        `level={3}`, because the `Panel` above already titles this region with
        an `h2`. A section heading announced as a sibling of the panel that
        contains it tells a screen reader the opposite of what the page means.
      */}
      <div className="calendar-sections">
        {held.length > 0 && (
          <Section label="held right now" level={3}>
            <Rows label="Held notifications" className="calendar-notifications">
              {held.map((row) => (
                <NotificationRow key={row.id} row={row} />
              ))}
            </Rows>
          </Section>
        )}
        {released.length > 0 && (
          <Section label="held, then let through" level={3}>
            <Rows label="Released notifications" className="calendar-notifications">
              {released.map((row) => (
                <NotificationRow key={row.id} row={row} />
              ))}
            </Rows>
          </Section>
        )}
      </div>
    </Panel>
  );
}

function NotificationsError({ error }: { error: unknown }) {
  if (isApiRefusal(error)) return <RefusalNote refusal={error} />;
  return <ErrorNote>the núcleo did not answer — nothing is known about held notifications</ErrorNote>;
}

function NotificationRow({ row }: { row: PendingNotification }) {
  return (
    <Row className="calendar-notification">
      <span className="calendar-notification-kind">{row.kind}</span>
      <p className="calendar-notification-summary">{row.summary}</p>
      <RelativeTime at={row.queued_at} />
    </Row>
  );
}
