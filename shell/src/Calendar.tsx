import { useCallback, useEffect, useRef, useState } from "react";
import {
  cancelCalendarOccurrence, createCalendarEvent, deleteCalendarEvent, getCalendarBusy,
  getCalendarConfig, getCalendarEvents, listPendingNotifications,
  type CalendarConfigView, type CalendarOccurrence, type ConnectionState,
  type NewCalendarEvent, type PendingNotification,
} from "./api";
import { relativeTime } from "./derive";
import { Badge, Button, ConfirmButton, ErrorNote, Panel, Teach } from "./ui";

/** How far ahead the agenda reaches. Two weeks matches the horizon the daemon proposes slots in. */
const HORIZON_DAYS = 14;

const REFRESH_MS = 30_000;

interface CalendarProps {
  token: string | null;
  connection: ConnectionState;
}

/**
 * A local wall-clock string the daemon accepts, from a `datetime-local` input.
 *
 * The input already yields `2026-08-03T09:00`, which is the right shape minus seconds — and it is
 * right for the right reason: it carries no offset, which is exactly what the daemon stores. The
 * zone travels separately, so that a weekly 09:00 stays 09:00 when the clocks change.
 */
function withSeconds(value: string): string {
  return value.length === 16 ? `${value}:00` : value;
}

function timeOfDay(iso: string): string {
  return new Date(iso).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
}

function dayHeading(iso: string): string {
  return new Date(iso).toLocaleDateString([], {
    weekday: "long",
    day: "numeric",
    month: "long",
  });
}

/** Groups occurrences under a day heading, preserving the daemon's chronological order. */
function byDay(occurrences: CalendarOccurrence[]): [string, CalendarOccurrence[]][] {
  const days = new Map<string, CalendarOccurrence[]>();
  for (const occurrence of occurrences) {
    const key = dayHeading(occurrence.starts_at);
    const existing = days.get(key);
    if (existing) existing.push(occurrence);
    else days.set(key, [occurrence]);
  }
  return [...days.entries()];
}

interface EventFormProps {
  busy: boolean;
  defaultTz: string;
  onSubmit: (event: NewCalendarEvent) => void;
  onCancel: () => void;
}

function EventForm({ busy, defaultTz, onSubmit, onCancel }: EventFormProps) {
  const [title, setTitle] = useState("");
  const [startsAt, setStartsAt] = useState("");
  const [minutes, setMinutes] = useState(60);
  const [freq, setFreq] = useState<"" | "daily" | "weekly" | "monthly">("");

  const incomplete = title.trim() === "" || startsAt === "" || minutes <= 0;

  return (
    <form
      className="form-grid"
      onSubmit={(submitted) => {
        submitted.preventDefault();
        if (incomplete || busy) return;
        onSubmit({
          title: title.trim(),
          starts_at_local: withSeconds(startsAt),
          duration_minutes: minutes,
          tz: defaultTz,
          ...(freq === "" ? {} : { freq }),
        });
      }}
    >
      <label>
        What
        <input value={title} onChange={(event) => setTitle(event.target.value)} />
      </label>
      <label>
        Starts
        <input
          type="datetime-local"
          value={startsAt}
          onChange={(event) => setStartsAt(event.target.value)}
        />
      </label>
      <label>
        Minutes
        <input
          type="number"
          min={1}
          value={minutes}
          onChange={(event) => setMinutes(Number(event.target.value))}
        />
      </label>
      <label>
        Repeats
        <select
          value={freq}
          onChange={(event) => setFreq(event.target.value as typeof freq)}
        >
          <option value="">never</option>
          <option value="daily">daily</option>
          <option value="weekly">weekly</option>
          <option value="monthly">monthly</option>
        </select>
      </label>
      <div className="form-actions">
        <Button type="submit" disabled={incomplete || busy}>Add</Button>
        <Button variant="ghost" onClick={onCancel} disabled={busy}>Cancel</Button>
      </div>
    </form>
  );
}

/**
 * The calendar tab.
 *
 * Deliberately an agenda list rather than a month grid. The question this pillar answers is "am I
 * reachable", which is a question about the next few hours — a grid optimises for a different one,
 * and would be the larger half of this feature by weight.
 */
export default function Calendar({ token, connection }: CalendarProps) {
  const [occurrences, setOccurrences] = useState<CalendarOccurrence[] | null>(null);
  const [config, setConfig] = useState<CalendarConfigView | null>(null);
  const [busyNow, setBusyNow] = useState<boolean | null>(null);
  const [held, setHeld] = useState<PendingNotification[]>([]);
  const [creating, setCreating] = useState(false);
  const [working, setWorking] = useState(false);
  const [failed, setFailed] = useState<string | null>(null);
  const timer = useRef<number | null>(null);

  const usable = connection === "connected" && token !== null;

  const refresh = useCallback(async () => {
    if (token === null) return;
    const from = new Date();
    const to = new Date(from.getTime() + HORIZON_DAYS * 24 * 60 * 60 * 1000);
    const [found, busy, pending] = await Promise.all([
      getCalendarEvents(token, from, to),
      getCalendarBusy(token),
      listPendingNotifications(token),
    ]);
    setOccurrences(found);
    setBusyNow(busy);
    setHeld(pending);
  }, [token]);

  useEffect(() => {
    if (!usable) return;
    void refresh();
    void getCalendarConfig(token).then(setConfig);
    timer.current = window.setInterval(() => void refresh(), REFRESH_MS);
    return () => {
      if (timer.current !== null) window.clearInterval(timer.current);
    };
  }, [usable, token, refresh]);

  const add = useCallback(
    async (event: NewCalendarEvent) => {
      if (token === null) return;
      setWorking(true);
      setFailed(null);
      const created = await createCalendarEvent(token, event);
      if (created.ok) {
        setCreating(false);
        await refresh();
      } else {
        setFailed(created.reason);
      }
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

  if (!usable) {
    return (
      <Panel title="Calendar">
        <p className="muted">The daemon is not reachable, so the calendar cannot be read.</p>
      </Panel>
    );
  }

  const stillHeld = held.filter((notification) => notification.delivered_at === null);

  return (
    <>
      <Panel
        title="Calendar"
        aside={
          busyNow === null ? undefined : busyNow
            ? <Badge tone="paused">busy — notifications are waiting</Badge>
            : <Badge tone="active">free</Badge>
        }
      >
        <Teach title="What this calendar is for">
          It decides one thing: whether a mail notification reaches you now or waits. It never
          changes how a message was classified — being in a meeting makes you unreachable, not the
          message less important. Times are stored as a wall clock in{" "}
          {config?.default_tz ?? "your zone"}, so a repeating event keeps its hour when the clocks
          change.
        </Teach>

        {failed !== null && <ErrorNote>{failed}</ErrorNote>}

        <div className="form-actions">
          <Button onClick={() => { setCreating((open) => !open); setFailed(null); }}>
            {creating ? "Close" : "Add an event"}
          </Button>
        </div>

        {creating && (
          <EventForm
            busy={working}
            defaultTz={config?.default_tz ?? ""}
            onSubmit={(event) => void add(event)}
            onCancel={() => setCreating(false)}
          />
        )}

        {occurrences === null && <p className="muted">Reading the agenda…</p>}
        {occurrences !== null && occurrences.length === 0 && (
          <p className="muted">
            Nothing in the next {HORIZON_DAYS} days. With an empty calendar every notification
            arrives immediately, which is exactly what happened before this tab existed.
          </p>
        )}

        {occurrences !== null && byDay(occurrences).map(([day, entries]) => (
          <section key={day} className="day">
            <h3>{day}</h3>
            <ul className="rows">
              {entries.map((occurrence) => (
                <li key={`${occurrence.event_id}-${occurrence.occurrence_local}`} className="row">
                  <span className="when">
                    {timeOfDay(occurrence.starts_at)}–{timeOfDay(occurrence.ends_at)}
                  </span>
                  <span className="what">{occurrence.title}</span>
                  {occurrence.source === "proposal" && (
                    // Provenance, not state — so it stays out of the Badge vocabulary, which is
                    // the single shared language for what something IS doing.
                    <span className="muted" title="Created from a proposal you approved">
                      from a proposal
                    </span>
                  )}
                  <span className="actions">
                    <Button
                      variant="ghost"
                      size="sm"
                      disabled={working}
                      onClick={() => void skipOne(occurrence)}
                    >
                      Skip this one
                    </Button>
                    <ConfirmButton
                      variant="danger"
                      size="sm"
                      confirmLabel="Delete the whole series?"
                      disabled={working}
                      onConfirm={() => void removeSeries(occurrence.event_id)}
                    >
                      Delete
                    </ConfirmButton>
                  </span>
                </li>
              ))}
            </ul>
          </section>
        ))}
      </Panel>

      <Panel
        title="Held notifications"
        aside={stillHeld.length > 0 ? `${stillHeld.length} waiting` : undefined}
      >
        <Teach title="Nothing is dropped">
          A notification that arrives while you are busy waits here, and goes out when the calendar
          opens. Delivered ones stay listed on purpose: "did the calendar swallow something?" has to
          be a question with an answer.
        </Teach>
        {held.length === 0 && <p className="muted">Nothing has ever been held back.</p>}
        <ul className="rows">
          {held.map((notification) => (
            <li key={notification.id} className="row">
              <span className="what">{notification.summary}</span>
              {notification.delivered_at === null ? (
                <Badge tone="pending">held {relativeTime(notification.queued_at)}</Badge>
              ) : (
                <span className="muted">
                  delivered {relativeTime(notification.delivered_at)}
                </span>
              )}
            </li>
          ))}
        </ul>
      </Panel>
    </>
  );
}
