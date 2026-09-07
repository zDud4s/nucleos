// §spec calendario-local
import { useState } from "react";
import { isApiRefusal } from "../data/client";
import {
  dayHours,
  isWorkingDay,
  occurrenceKey,
  useCancelOccurrence,
  useCreateEvent,
  useDeleteSeries,
  useMoveOccurrence,
  type CalendarConfigView,
  type CreateEventRequest,
  type EventOccurrence,
} from "../data/calendar";
import { inputFromStamp, occurrenceMinutes, stampFromInput } from "../lib/calendar-grid";
import { Badge, Button, ConfirmButton, ErrorNote, RefusalNote } from "../ui";
import { placementOf, slotStamp, type Slot } from "./slot";

/**
 * The selected day: what is on it, what you can do to it, and a form to add to
 * it.
 *
 * **This is the half of the redesign that is not the grid.** The page used to
 * draw a month you could not touch and then, below it, a flat list of every
 * occurrence in the six-week window — each with four controls, none with any
 * spatial context. So the grid showed without letting you act, the list let
 * you act without showing where, and a busy month was a wall. The grid selects
 * a day; this reads whichever day that is. One day at a time is also what
 * makes the controls affordable: four per occurrence is fine for a Tuesday and
 * unreadable for forty.
 *
 * The draft form lives here rather than in a panel of its own because design
 * §6.14 asks for it "inline no slot clicado" — an event drafted from this
 * sheet starts on the day you are looking at, and in the week view at the hour
 * you clicked.
 */

export interface DaySheetProps {
  slot: Slot;
  occurrences: EventOccurrence[];
  now: Date;
  config: CalendarConfigView | undefined;
}

export function DaySheet({ slot, occurrences, now, config }: DaySheetProps) {
  const hours = dayHours(slot.day);
  const working = isWorkingDay(slot.day, config);
  const today =
    slot.day.getFullYear() === now.getFullYear() &&
    slot.day.getMonth() === now.getMonth() &&
    slot.day.getDate() === now.getDate();

  return (
    <div className="calendar-sheet">
      <div className="calendar-sheet-head">
        <h3 className="calendar-sheet-title">
          {slot.day.toLocaleDateString(undefined, {
            weekday: "long",
            day: "numeric",
            month: "long",
            year: "numeric",
          })}
        </h3>
        <div className="calendar-sheet-marks">
          {today && <Badge tone="info">today</Badge>}
          {!working && <Badge tone="off">not a working day</Badge>}
          {hours.short && <Badge tone="paused">{hours.hours}h — short day</Badge>}
          {hours.long && <Badge tone="paused">{hours.hours}h — long day</Badge>}
        </div>
      </div>

      {occurrences.length === 0 ? (
        <p className="calendar-empty">nothing on this day.</p>
      ) : (
        <ul className="ui-rows calendar-occurrence-list">
          {occurrences.map((occurrence) => (
            <li className="ui-rows-row" key={occurrenceKey(occurrence.event_id, occurrence.occurrence_local)}>
              <OccurrenceActions occurrence={occurrence} />
            </li>
          ))}
        </ul>
      )}

      <DraftEventForm slot={slot} config={config} />
    </div>
  );
}

/* -------------------------------------------------------------- new event -- */

/** `"09:00"` → `9`. The hour a day-level draft opens at; 9 when the config has not answered. */
function workStartHour(config: CalendarConfigView | undefined): number {
  const parsed = /^(\d{1,2}):/.exec(config?.working_hours_start ?? "");
  const hour = parsed === null ? 9 : Number(parsed[1]);
  return hour >= 0 && hour <= 23 ? hour : 9;
}

/**
 * A one-off or a recurring series, starting from the selected slot.
 *
 * Recurrence is offered behind its own toggle, collapsed by default, and
 * `freq` is genuinely absent from the request — not sent as `null` — when the
 * toggle is off: {@link CreateEventRequest}'s header explains why that
 * distinction is the one the daemon actually reads.
 *
 * **The start is seeded, not typed.** It used to be an empty `datetime-local`
 * a person filled in by hand while looking at a grid that already said which
 * day they meant. Re-seeding when the selection moves — and only then — is
 * done with the documented "adjust state while rendering" pattern rather than
 * an effect, so a half-typed title survives clicking to another day while the
 * date underneath it follows the grid.
 */
export function DraftEventForm({
  slot,
  config,
}: {
  slot: Slot;
  config: CalendarConfigView | undefined;
}) {
  const create = useCreateEvent();
  const seed = inputFromStamp(slotStamp(slot, workStartHour(config)));

  const [title, setTitle] = useState("");
  const [seenSeed, setSeenSeed] = useState(seed);
  const [start, setStart] = useState(seed);
  const [duration, setDuration] = useState(30);
  const [repeats, setRepeats] = useState(false);
  const [freq, setFreq] = useState<"daily" | "weekly" | "monthly">("weekly");

  if (seed !== seenSeed) {
    setSeenSeed(seed);
    setStart(seed);
  }

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
      <p className="calendar-draft-lead">New event</p>
      <div className="calendar-draft-fields">
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
      </div>
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
export function daemonProse(error: { code: string; detail: string }): Record<string, string> {
  const detail = error.detail.trim();
  if (detail === "" || detail === error.code) return {};
  if (detail.split(/\s+/).length < 3) return {};
  return { [error.code]: detail };
}

/* --------------------------------------------------------- occurrences -- */

/**
 * Skip, move, or delete the whole series — addressed by `occurrence_local`,
 * never by an occurrence id, because there is no such thing.
 *
 * **`occurrence_local` is the ORIGINAL start and stays the original after a
 * move**, which is what makes moving the same occurrence twice relocate it
 * instead of forging a second exception row. The move control is therefore
 * seeded from where the occurrence *is now* — `placementOf`'s instant — while
 * the request still addresses it by where it *was*. Seeding the control from
 * `occurrence_local` would have made the second move offer to undo the first.
 */
export function OccurrenceActions({ occurrence }: { occurrence: EventOccurrence }) {
  const cancel = useCancelOccurrence();
  const move = useMoveOccurrence();
  const deleteSeries = useDeleteSeries();
  const placement = placementOf(occurrence);
  const [moveTo, setMoveTo] = useState(() => inputFromStamp(localInput(placement.startsAt)));

  function submitMove() {
    const stamp = stampFromInput(moveTo);
    if (stamp === null) return;
    move.mutate({
      eventId: occurrence.event_id,
      // Never `moveTo`, and never the current instant: the identity is the original.
      occurrenceLocal: occurrence.occurrence_local,
      toLocal: stamp,
      durationMinutes: occurrenceMinutes(occurrence.starts_at, occurrence.ends_at),
    });
  }

  return (
    <div className="calendar-occurrence-actions">
      <p className="calendar-occurrence-title">
        <span className="calendar-occurrence-clock">{placement.clock}</span>
        <span className="calendar-occurrence-name">
          {occurrence.title}
          {occurrence.source === "proposal" && <Badge tone="shadow">proposed</Badge>}
          {placement.moved && (
            <span className="calendar-occurrence-moved">
              moved from {occurrence.occurrence_local.replace("T", " ").slice(0, 16)}
            </span>
          )}
        </span>
      </p>
      <div className="calendar-occurrence-controls">
        <ConfirmButton
          label="Skip this occurrence"
          confirmLabel="Skip it"
          variant="quiet"
          disabled={cancel.isPending}
          onConfirm={() =>
            cancel.mutate({ eventId: occurrence.event_id, occurrenceLocal: occurrence.occurrence_local })
          }
        />
        <div className="calendar-move">
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
        </div>
        <details className="calendar-occurrence-more">
          <summary aria-label="More occurrence actions">…</summary>
          <ConfirmButton
            label="Delete whole series"
            confirmLabel="Delete every occurrence"
            variant="danger"
            disabled={deleteSeries.isPending}
            onConfirm={() => deleteSeries.mutate(occurrence.event_id)}
          />
        </details>
      </div>
      {cancel.isError && <OccurrenceError error={cancel.error} what="not skipped" />}
      {move.isError && <OccurrenceError error={move.error} what="not moved" />}
      {deleteSeries.isError && <OccurrenceError error={deleteSeries.error} what="the series was not deleted" />}
    </div>
  );
}

/** An instant as the `"YYYY-MM-DDTHH:MM:SS"` this machine would call it. */
function localInput(at: Date): string {
  const pad = (value: number) => String(value).padStart(2, "0");
  return (
    `${at.getFullYear()}-${pad(at.getMonth() + 1)}-${pad(at.getDate())}` +
    `T${pad(at.getHours())}:${pad(at.getMinutes())}:00`
  );
}

function OccurrenceError({ error, what }: { error: unknown; what: string }) {
  if (isApiRefusal(error)) return <RefusalNote refusal={error} sentences={daemonProse(error)} />;
  return <ErrorNote>the núcleo did not answer — {what}</ErrorNote>;
}
