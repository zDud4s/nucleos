import { useState } from "react";
import { isApiRefusal } from "../data/client";
import {
  dayHours,
  occurrenceKey,
  useBusy,
  useCalendarConfig,
  useCalendarEvents,
  useCancelOccurrence,
  useCreateEvent,
  useDeleteSeries,
  useMoveOccurrence,
  type CalendarConfigView,
  type CreateEventRequest,
  type EventOccurrence,
} from "../data/calendar";
import { isHeld, usePendingNotifications, type PendingNotification } from "../data/feed";
import {
  dayBounds,
  inputFromStamp,
  monthMatrix,
  nowFraction,
  occurrenceMinutes,
  sameDay,
  stampFromInput,
} from "../lib/calendar-grid";
import { Badge, Button, ConfirmButton, ErrorNote, PageHeader, Panel, RefusalNote, RelativeTime } from "../ui";
import "./calendar.css";

/**
 * Calendar — the month grid, a form for a new event, per-occurrence actions,
 * and the notifications the calendar has held back while you looked busy.
 *
 * **Month only, on purpose.** Week view and drag-to-move are v1 parity gaps
 * (design §6.14), both built on the exact same `lib/calendar-grid.ts` helpers
 * this page already uses — a small follow-up, not a rewrite. `OccurrenceActions`
 * moves an occurrence through a `datetime-local` input instead.
 *
 * **The DST badge is computed here, not asked for.** `GET /calendar/config`
 * carries no such field (`data/calendar.ts`'s header), so `dayHours` reads it
 * off the day itself via `lib/calendar-grid.ts`'s `dayBounds`/`hoursInSpan` —
 * a local calendar day whose real span is 23 or 25 hours, not 24.
 */

function monthLabel(anchor: Date): string {
  return anchor.toLocaleDateString(undefined, { month: "long", year: "numeric" });
}

function startOfMonth(date: Date): Date {
  return new Date(date.getFullYear(), date.getMonth(), 1);
}

/**
 * The RFC 3339 window one month's grid needs — the full six weeks
 * `monthMatrix` draws, not merely the calendar month, since the grid always
 * shows a little of the month either side.
 */
function monthWindow(anchor: Date): { from: string; to: string } {
  const weeks = monthMatrix(anchor);
  const firstDay = weeks[0][0];
  const lastDay = weeks[weeks.length - 1][6];
  const [from] = dayBounds(firstDay);
  const [, to] = dayBounds(lastDay);
  return { from: from.toISOString(), to: to.toISOString() };
}

function headline(rows: EventOccurrence[], config: CalendarConfigView | undefined): string | undefined {
  if (config === undefined) return undefined;
  const noun = rows.length === 1 ? "occurrence" : "occurrences";
  return `${rows.length} ${noun} this month — working hours ${config.working_hours_start}–${config.working_hours_end}, ${config.default_tz}`;
}

export function Calendar() {
  const [anchor, setAnchor] = useState(() => startOfMonth(new Date()));
  const now = new Date();
  const { from, to } = monthWindow(anchor);

  const events = useCalendarEvents(from, to);
  const busy = useBusy();
  const config = useCalendarConfig();
  const rows = events.data ?? [];

  return (
    <>
      <PageHeader
        title="Calendar"
        headline={headline(rows, config.data)}
        actions={<BusyIndicator busy={busy.data?.busy} />}
      />

      <Panel title={monthLabel(anchor)} aside={<MonthNav anchor={anchor} onChange={setAnchor} />}>
        {events.isError && rows.length === 0 && <EventsError error={events.error} />}
        {events.data === undefined && !events.isError && <p className="calendar-loading">reading the month…</p>}
        {events.data !== undefined && <CalendarGrid anchor={anchor} occurrences={rows} now={now} />}
      </Panel>

      <Panel title="New event">
        <DraftEventForm />
      </Panel>

      {rows.length > 0 && (
        <Panel title="This month's occurrences" variant="dim">
          <ul className="calendar-occurrence-list">
            {rows.map((occurrence) => (
              <li key={occurrenceKey(occurrence.event_id, occurrence.occurrence_local)}>
                <OccurrenceActions occurrence={occurrence} />
              </li>
            ))}
          </ul>
        </Panel>
      )}

      <HeldNotifications />
    </>
  );
}

function MonthNav({ anchor, onChange }: { anchor: Date; onChange: (next: Date) => void }) {
  return (
    <div className="calendar-nav">
      <Button
        title="Previous month"
        onClick={() => onChange(new Date(anchor.getFullYear(), anchor.getMonth() - 1, 1))}
      >
        ‹ Prev
      </Button>
      <Button title="Next month" onClick={() => onChange(new Date(anchor.getFullYear(), anchor.getMonth() + 1, 1))}>
        Next ›
      </Button>
    </div>
  );
}

function EventsError({ error }: { error: unknown }) {
  if (isApiRefusal(error)) return <RefusalNote refusal={error} />;
  return <ErrorNote>the núcleo did not answer — nothing is known about this month</ErrorNote>;
}

/* ------------------------------------------------------------------ grid -- */

export interface CalendarGridProps {
  anchor: Date;
  occurrences: EventOccurrence[];
  now: Date;
}

/**
 * The month, six weeks always, each day carrying its own occurrences and its
 * own DST reading.
 *
 * Pure and prop-driven — no query of its own — so the arithmetic can be
 * tested without a daemon or a router behind it.
 */
export function CalendarGrid({ anchor, occurrences, now }: CalendarGridProps) {
  const weeks = monthMatrix(anchor);
  const byDay = groupByLocalDay(occurrences);

  return (
    <div className="calendar-grid" role="grid" aria-label={monthLabel(anchor)}>
      {weeks.map((week) => (
        <div className="calendar-week" role="row" key={dateKeyOf(week[0])}>
          {week.map((day) => (
            <DayCell
              key={dateKeyOf(day)}
              day={day}
              inMonth={day.getMonth() === anchor.getMonth()}
              occurrences={byDay.get(dateKeyOf(day)) ?? []}
              now={now}
            />
          ))}
        </div>
      ))}
    </div>
  );
}

/**
 * A day's own `"YYYY-MM-DD"`, built from the same local getters `monthMatrix`
 * used to construct the day — never re-parsed through a `Date`, so a grid
 * cell's key can never disagree with the box that was placed into it.
 */
function dateKeyOf(day: Date): string {
  const pad = (value: number) => String(value).padStart(2, "0");
  return `${day.getFullYear()}-${pad(day.getMonth() + 1)}-${pad(day.getDate())}`;
}

/**
 * Occurrences grouped by `occurrence_local`'s own date, not by `starts_at`.
 * `occurrence_local` is the daemon's ORIGINAL local wall clock — the ground
 * truth for which day an occurrence belongs on — so matching against it never
 * has to reckon with this machine's own time zone at all.
 */
function groupByLocalDay(occurrences: EventOccurrence[]): Map<string, EventOccurrence[]> {
  const map = new Map<string, EventOccurrence[]>();
  for (const occurrence of occurrences) {
    const key = occurrence.occurrence_local.slice(0, 10);
    const list = map.get(key);
    if (list === undefined) map.set(key, [occurrence]);
    else list.push(occurrence);
  }
  return map;
}

function DayCell({
  day,
  inMonth,
  occurrences,
  now,
}: {
  day: Date;
  inMonth: boolean;
  occurrences: EventOccurrence[];
  now: Date;
}) {
  const hours = dayHours(day);
  const today = sameDay(day, now);
  const classes = ["calendar-day"];
  if (!inMonth) classes.push("calendar-day-outside");
  if (today) classes.push("calendar-day-today");

  return (
    <div className={classes.join(" ")} role="gridcell">
      <div className="calendar-day-head">
        <span className="calendar-day-number">{day.getDate()}</span>
        {hours.short && (
          <Badge tone="paused" className="calendar-day-dst">
            {hours.hours}h — short day
          </Badge>
        )}
        {hours.long && (
          <Badge tone="paused" className="calendar-day-dst">
            {hours.hours}h — long day
          </Badge>
        )}
      </div>
      {today && <NowLine day={day} now={now} />}
      <ul className="calendar-day-events">
        {occurrences.map((occurrence) => (
          <li className="calendar-day-event" key={occurrenceKey(occurrence.event_id, occurrence.occurrence_local)}>
            {occurrence.source === "proposal" && <span className="calendar-day-event-proposal">proposed</span>}
            {occurrence.title}
          </li>
        ))}
      </ul>
    </div>
  );
}

/** How far through today has already passed — absent on every other day. */
export function NowLine({ day, now }: { day: Date; now: Date }) {
  const fraction = nowFraction(now, day);
  if (fraction === null) return null;
  return (
    <div
      className="calendar-now-line"
      role="img"
      aria-label={`${Math.round(fraction * 100)}% through today`}
    >
      <span className="calendar-now-line-fill" style={{ width: `${Math.round(fraction * 100)}%` }} />
    </div>
  );
}

/* ------------------------------------------------------------------ busy -- */

/**
 * Whether this machine currently reads as busy — `GET /calendar/busy` never
 * refuses, so there is no error branch here, only "not answered yet".
 */
export function BusyIndicator({ busy }: { busy: boolean | undefined }) {
  if (busy === undefined) return null;
  return <Badge tone={busy ? "pending" : "off"}>{busy ? "busy right now" : "free right now"}</Badge>;
}

/* -------------------------------------------------------------- new event -- */

/**
 * A one-off or a recurring series.
 *
 * Recurrence is offered behind its own toggle, collapsed by default, and
 * `freq` is genuinely absent from the request — not sent as `null` — when
 * the toggle is off: {@link CreateEventRequest}'s header explains why that
 * distinction is the one the daemon actually reads.
 */
export function DraftEventForm() {
  const create = useCreateEvent();
  const [title, setTitle] = useState("");
  const [start, setStart] = useState("");
  const [duration, setDuration] = useState(30);
  const [repeats, setRepeats] = useState(false);
  const [freq, setFreq] = useState<"daily" | "weekly" | "monthly">("weekly");

  const stamp = stampFromInput(start);
  const canSubmit = title.trim() !== "" && stamp !== null && duration > 0 && !create.isPending;

  function submit() {
    if (stamp === null || title.trim() === "") return;
    const input: CreateEventRequest = {
      title: title.trim(),
      starts_at_local: stamp,
      duration_minutes: duration,
      // Absent, not null, for a one-off — `JSON.stringify` drops the key.
      freq: repeats ? freq : undefined,
    };
    create.mutate(input, { onSuccess: () => setTitle("") });
  }

  return (
    <form className="calendar-draft" onSubmit={(event) => event.preventDefault()}>
      <label className="calendar-draft-field">
        <span>Title</span>
        <input value={title} onChange={(event) => setTitle(event.target.value)} aria-label="Title" />
      </label>
      <label className="calendar-draft-field">
        <span>Starts</span>
        <input
          type="datetime-local"
          value={start}
          onChange={(event) => setStart(event.target.value)}
          aria-label="Starts"
        />
      </label>
      <label className="calendar-draft-field">
        <span>Duration (minutes)</span>
        <input
          type="number"
          min={1}
          value={duration}
          onChange={(event) => setDuration(Number(event.target.value))}
          aria-label="Duration (minutes)"
        />
      </label>
      <label className="calendar-draft-repeats">
        <input type="checkbox" checked={repeats} onChange={(event) => setRepeats(event.target.checked)} />
        <span>Repeats</span>
      </label>
      {repeats && (
        <label className="calendar-draft-field">
          <span>Every</span>
          <select
            value={freq}
            aria-label="Repeat frequency"
            onChange={(event) => setFreq(event.target.value as "daily" | "weekly" | "monthly")}
          >
            <option value="daily">day</option>
            <option value="weekly">week</option>
            <option value="monthly">month</option>
          </select>
        </label>
      )}
      <Button disabled={!canSubmit} onClick={submit}>
        Add to calendar
      </Button>
      {create.isSuccess && (
        <p className="calendar-outcome" role="status">
          added — event {create.data.id}
        </p>
      )}
      {create.isError && <DraftError error={create.error} />}
    </form>
  );
}

function DraftError({ error }: { error: unknown }) {
  if (isApiRefusal(error)) return <RefusalNote refusal={error} sentences={daemonProse(error)} />;
  return <ErrorNote>the núcleo did not answer — nothing was added</ErrorNote>;
}

/**
 * The daemon's own sentence, when it really sent one — every refusal `POST
 * /calendar/events` makes is bare prose written on purpose (`data/calendar.ts`
 * carries the list), worth quoting rather than replaced by generic copy.
 */
function daemonProse(error: { code: string; detail: string }): Record<string, string> {
  const detail = error.detail.trim();
  if (detail === "" || detail === error.code) return {};
  if (detail.split(/\s+/).length < 3) return {};
  return { [error.code]: detail };
}

/* --------------------------------------------------------- occurrences -- */

/**
 * Skip, move, or delete the whole series — addressed by `occurrence_local`,
 * never by an occurrence id, because there is no such thing.
 */
export function OccurrenceActions({ occurrence }: { occurrence: EventOccurrence }) {
  const cancel = useCancelOccurrence();
  const move = useMoveOccurrence();
  const deleteSeries = useDeleteSeries();
  const [moveTo, setMoveTo] = useState(() => inputFromStamp(occurrence.occurrence_local));

  function submitMove() {
    const stamp = stampFromInput(moveTo);
    if (stamp === null) return;
    move.mutate({
      eventId: occurrence.event_id,
      occurrenceLocal: occurrence.occurrence_local,
      toLocal: stamp,
      durationMinutes: occurrenceMinutes(occurrence.starts_at, occurrence.ends_at),
    });
  }

  return (
    <div className="calendar-occurrence-actions">
      <p className="calendar-occurrence-title">
        {occurrence.title}
        <span className="calendar-occurrence-when">{occurrence.occurrence_local}</span>
      </p>
      <div className="calendar-occurrence-controls">
        <ConfirmButton
          label="Skip this occurrence"
          confirmLabel="Skip it"
          disabled={cancel.isPending}
          onConfirm={() =>
            cancel.mutate({ eventId: occurrence.event_id, occurrenceLocal: occurrence.occurrence_local })
          }
        />
        <label className="calendar-move-field">
          <span>Move to</span>
          <input
            type="datetime-local"
            value={moveTo}
            onChange={(event) => setMoveTo(event.target.value)}
            aria-label={`Move ${occurrence.title} to`}
          />
        </label>
        <Button disabled={move.isPending} onClick={submitMove}>
          Move
        </Button>
        <ConfirmButton
          label="Delete whole series"
          confirmLabel="Delete every occurrence"
          variant="danger"
          disabled={deleteSeries.isPending}
          onConfirm={() => deleteSeries.mutate(occurrence.event_id)}
        />
      </div>
      {cancel.isError && <OccurrenceError error={cancel.error} what="not skipped" />}
      {move.isError && <OccurrenceError error={move.error} what="not moved" />}
      {deleteSeries.isError && <OccurrenceError error={deleteSeries.error} what="the series was not deleted" />}
    </div>
  );
}

function OccurrenceError({ error, what }: { error: unknown; what: string }) {
  if (isApiRefusal(error)) return <RefusalNote refusal={error} sentences={daemonProse(error)} />;
  return <ErrorNote>the núcleo did not answer — {what}</ErrorNote>;
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
    <Panel title="Held notifications">
      <p className="calendar-note">
        `calendar.rs` fails OPEN by design — a database it cannot read answers "not busy" rather than
        staying silent, since silence here is a message that never arrived.
      </p>
      {pending.isError && rows.length === 0 && <NotificationsError error={pending.error} />}
      {pending.data !== undefined && rows.length === 0 && <p className="calendar-empty">nothing has been held.</p>}
      {held.length > 0 && (
        <>
          <p className="calendar-subhead">held right now</p>
          <ul className="calendar-notifications" aria-label="Held notifications">
            {held.map((row) => (
              <NotificationRow key={row.id} row={row} />
            ))}
          </ul>
        </>
      )}
      {released.length > 0 && (
        <>
          <p className="calendar-subhead">held, then let through</p>
          <ul className="calendar-notifications" aria-label="Released notifications">
            {released.map((row) => (
              <NotificationRow key={row.id} row={row} />
            ))}
          </ul>
        </>
      )}
    </Panel>
  );
}

function NotificationsError({ error }: { error: unknown }) {
  if (isApiRefusal(error)) return <RefusalNote refusal={error} />;
  return <ErrorNote>the núcleo did not answer — nothing is known about held notifications</ErrorNote>;
}

function NotificationRow({ row }: { row: PendingNotification }) {
  return (
    <li className="calendar-notification">
      <span className="calendar-notification-kind">{row.kind}</span>
      <p className="calendar-notification-summary">{row.summary}</p>
      <RelativeTime at={row.queued_at} />
    </li>
  );
}
