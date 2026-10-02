// §spec calendario-local
import { useId, useState } from "react";
import { ChevronDownIcon } from "lucide-react";
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
import { UI_LOCALE } from "../lib/locale";
import { Badge, Button, ConfirmButton, ErrorNote, Modal, Quiet, RefusalNote, Row, Rows, TimeField } from "../ui";
import { placementOf, slotStamp, type Slot } from "./slot";

/**
 * The selected day: what is on it, what you can do to it, and a form to add to
 * it — as a modal over the grid, opened by clicking the day.
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
 * **A modal, not a panel under the grid.** As a panel it was always there,
 * always the height of a form, and on a month view it sat below the fold — so
 * the day you had just clicked answered somewhere you could not see. Opened on
 * the click, the answer lands where the eye already is, and the grid gets the
 * whole page back. Only a click (or Enter) opens it: arrowing across the grid
 * and dropping a dragged chip still only move the selection, because a dialog
 * that sprang up on every arrow press would make the keyboard useless.
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
  open: boolean;
  onOpenChange: (open: boolean) => void;
}

export function DaySheet({ slot, occurrences, now, config, open, onOpenChange }: DaySheetProps) {
  const hours = dayHours(slot.day);
  const working = isWorkingDay(slot.day, config);
  const today =
    slot.day.getFullYear() === now.getFullYear() &&
    slot.day.getMonth() === now.getMonth() &&
    slot.day.getDate() === now.getDate();
  const marked = today || !working || hours.short || hours.long;
  /* One occurrence open at a time: its controls are the only ones on screen, so
     the rest of the day still reads as a column of times and names. */
  const [expanded, setExpanded] = useState<string | null>(null);

  return (
    <Modal
      open={open}
      onOpenChange={onOpenChange}
      size="md"
      title={slot.day.toLocaleDateString(UI_LOCALE, {
        weekday: "long",
        day: "numeric",
        month: "long",
        year: "numeric",
      })}
      description={
        marked ? (
          <span className="calendar-sheet-marks">
            {today && <Badge tone="info">today</Badge>}
            {!working && <Badge tone="off">not a working day</Badge>}
            {hours.short && <Badge tone="paused">{hours.hours}h — short day</Badge>}
            {hours.long && <Badge tone="paused">{hours.hours}h — long day</Badge>}
          </span>
        ) : undefined
      }
      /* The draft sits in the footer, not under the list: the footer stays put
         while the body scrolls, so adding to a busy day never means scrolling
         past it first. */
      footer={<DraftEventForm slot={slot} config={config} />}
    >
      {occurrences.length === 0 ? (
        <Quiet says="nothing on this day." />
      ) : (
        <Rows label="Occurrences on this day" className="calendar-occurrence-list">
          {occurrences.map((occurrence) => {
            const key = occurrenceKey(occurrence.event_id, occurrence.occurrence_local);
            return (
              <OccurrenceRow
                key={key}
                occurrence={occurrence}
                expanded={expanded === key}
                onToggle={() => setExpanded(expanded === key ? null : key)}
              />
            );
          })}
        </Rows>
      )}
    </Modal>
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
 *
 * **And the day is not asked at all.** The sheet is opened FROM a day, so the
 * only thing left to choose is the time in it: the day comes from the slot,
 * always, and the control is a {@link TimeField}. A date field here offered to
 * put the event on a day other than the one the heading names.
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
  const date = seed.slice(0, 10);

  const [title, setTitle] = useState("");
  const [seenSeed, setSeenSeed] = useState(seed);
  const [time, setTime] = useState(seed.slice(11, 16));
  const [duration, setDuration] = useState(30);
  const [repeats, setRepeats] = useState(false);
  const [freq, setFreq] = useState<"daily" | "weekly" | "monthly">("weekly");

  if (seed !== seenSeed) {
    setSeenSeed(seed);
    setTime(seed.slice(11, 16));
  }

  const stamp = stampFromInput(`${date}T${time}`);
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
    <form
      className="calendar-draft"
      aria-label="New event"
      onSubmit={(event) => {
        event.preventDefault();
        if (canSubmit) submit();
      }}
    >
      <div className="calendar-draft-line">
        <input
          className="calendar-draft-title"
          value={title}
          onChange={(event) => setTitle(event.target.value)}
          placeholder="Add an event to this day"
          aria-label="Title"
        />
        <Button type="submit" disabled={!canSubmit}>
          Add to calendar
        </Button>
      </div>
      <div className="calendar-draft-line calendar-draft-when">
        <span className="calendar-draft-inline">
          <span aria-hidden="true">At</span>
          <TimeField
            label="Starts"
            value={time}
            onChange={setTime}
            within={
              config === undefined
                ? undefined
                : { from: config.working_hours_start, to: config.working_hours_end }
            }
          />
        </span>
        <label className="calendar-draft-inline">
          <span>for</span>
          <input
            className="calendar-draft-minutes"
            type="number"
            min={1}
            value={duration}
            onChange={(event) => setDuration(Number(event.target.value))}
            aria-label="Duration (minutes)"
          />
          <span>min</span>
        </label>
        <label className="calendar-draft-inline calendar-draft-repeats">
          <input type="checkbox" checked={repeats} onChange={(event) => setRepeats(event.target.checked)} />
          <span>Repeats</span>
        </label>
        {repeats && (
          <label className="calendar-draft-inline">
            <span>every</span>
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

/** `15` → `"15 min"`, `90` → `"1 h 30"`, `120` → `"2 h"`. */
function lengthOf(minutes: number): string {
  if (minutes < 60) return `${minutes} min`;
  const hours = Math.floor(minutes / 60);
  const rest = minutes % 60;
  return rest === 0 ? `${hours} h` : `${hours} h ${String(rest).padStart(2, "0")}`;
}

/**
 * One line of the day's agenda — clock, name, length — that opens onto its
 * controls.
 *
 * **Closed by default.** Every occurrence used to carry all four controls at
 * once, a date field among them, so five meetings were twenty controls and the
 * day could not be read as a day. The line is the read; the controls are the
 * act, and they appear only under the one line being acted on.
 */
export function OccurrenceRow({
  occurrence,
  expanded,
  onToggle,
}: {
  occurrence: EventOccurrence;
  expanded: boolean;
  onToggle: () => void;
}) {
  const placement = placementOf(occurrence);
  const tools = useId();

  return (
    <Row current={expanded} className="calendar-occurrence">
      <button
        type="button"
        className="calendar-occurrence-head"
        aria-expanded={expanded}
        aria-controls={tools}
        onClick={onToggle}
      >
        <span className="calendar-occurrence-clock">{placement.clock}</span>
        <span className="calendar-occurrence-name">
          <span className="calendar-occurrence-title">{occurrence.title}</span>
          {occurrence.source === "proposal" && <Badge tone="shadow">proposed</Badge>}
          {placement.moved && (
            <span className="calendar-occurrence-moved">
              moved from {occurrence.occurrence_local.replace("T", " ").slice(0, 16)}
            </span>
          )}
        </span>
        <span className="calendar-occurrence-length">
          {lengthOf(occurrenceMinutes(occurrence.starts_at, occurrence.ends_at))}
        </span>
        <ChevronDownIcon className="calendar-occurrence-chevron" strokeWidth={1.5} aria-hidden="true" />
      </button>
      {expanded && (
        <div id={tools} className="calendar-occurrence-tools">
          <OccurrenceActions occurrence={occurrence} />
        </div>
      )}
    </Row>
  );
}

/**
 * Move, skip, or delete the whole series — the controls an open
 * {@link OccurrenceRow} shows — addressed by `occurrence_local`, never by an
 * occurrence id, because there is no such thing.
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
  const [asking, setAsking] = useState(false);
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
    <>
      <div className="calendar-occurrence-controls">
        <div className="calendar-move">
          <input
            type="datetime-local"
            value={moveTo}
            onChange={(event) => setMoveTo(event.target.value)}
            aria-label={`Move ${occurrence.title} to`}
          />
          <Button disabled={move.isPending} onClick={submitMove}>
            Move
          </Button>
        </div>
        <div className="calendar-occurrence-drop">
          <ConfirmButton
            label="Skip this occurrence"
            confirmLabel="Skip it"
            variant="quiet"
            disabled={cancel.isPending}
            onConfirm={() =>
              cancel.mutate({ eventId: occurrence.event_id, occurrenceLocal: occurrence.occurrence_local })
            }
          />
          <Button variant="danger" onClick={() => setAsking(true)}>
            Delete series
          </Button>
        </div>
      </div>
      {cancel.isError && <OccurrenceError error={cancel.error} what="not skipped" />}
      {move.isError && <OccurrenceError error={move.error} what="not moved" />}
      {/*
        The most destructive write on the page, so it is asked as a question over the page rather
        than armed in place: the modal names the series and says what goes with it, and Cancel,
        Escape and a click outside all leave it standing. It stays open on a refusal, so the
        reason is read where the decision was made.
      */}
      <Modal
        open={asking}
        onOpenChange={(open) => {
          setAsking(open);
          if (!open) deleteSeries.reset();
        }}
        title="Delete the whole series?"
        description={
          <>
            Every occurrence of <strong>{occurrence.title}</strong> goes with it — past and future,
            moved or not. Skipping only this one is the other button.
          </>
        }
        footer={
          <>
            <Button onClick={() => setAsking(false)}>Cancel</Button>
            <Button
              variant="danger-solid"
              disabled={deleteSeries.isPending}
              onClick={() => deleteSeries.mutate(occurrence.event_id, { onSuccess: () => setAsking(false) })}
            >
              Delete every occurrence
            </Button>
          </>
        }
      >
        {deleteSeries.isError && <OccurrenceError error={deleteSeries.error} what="the series was not deleted" />}
      </Modal>
    </>
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
