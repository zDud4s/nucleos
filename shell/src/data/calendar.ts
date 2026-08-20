import { keepPreviousData, useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { apiFetch } from "./client";
import { keys } from "./keys";
import { dayBounds, hoursInSpan } from "../lib/calendar-grid";
import { POLL } from "./poll";

/**
 * The Calendar pillar: the expanded occurrences one window at a time, whether
 * this machine currently reads as busy, the working-hours configuration, and
 * the four writes a person can make to a series or to one of its occurrences.
 *
 * **Calendar events store LOCAL wall clock plus an IANA zone, not UTC** —
 * deliberately, unlike everything else the shell writes: a weekly 09:00 has
 * to survive the clocks changing twice a year. Every write below sends a
 * local stamp with no offset (`lib/calendar-grid.ts`'s `localStamp` /
 * `stampFromInput` are what build one from a form); every read comes back
 * with BOTH the local stamp it was made from (`occurrence_local`, which
 * SURVIVES a move) and the UTC instant it currently resolves to (`starts_at`
 * / `ends_at`), and the two are never the same field doing two jobs.
 *
 * **`GET /calendar/config` carries no DST field** — design §6.14 assumed
 * otherwise. The short/long-day badge is computed locally instead, which is
 * exactly why `lib/calendar-grid.ts` exports `dayBounds` and `hoursInSpan`:
 * a day whose local span is 23 or 25 hours, not 24, is the short or long one.
 * {@link dayHours} is the one place that arithmetic happens.
 */

/** One expanded occurrence, exactly as `GET /calendar/events` serialises it. */
export interface EventOccurrence {
  /** The SERIES id — there is no separate occurrence id. */
  event_id: number;
  title: string;
  /** "human" | "proposal" — an approved calendar-event proposal gets its own style per §6.14. */
  source: string;
  /**
   * The ORIGINAL local start, `"YYYY-MM-DDTHH:MM:SS"`. Identifies the
   * occurrence together with `event_id`, and SURVIVES a move — which is what
   * makes moving the same occurrence twice relocate it rather than fork a
   * second exception row.
   */
  occurrence_local: string;
  /** RFC 3339 UTC. */
  starts_at: string;
  /** RFC 3339 UTC. */
  ends_at: string;
}

/** The identity of one occurrence — the pair `cancel`/`move` address by. */
export function occurrenceKey(eventId: number, occurrenceLocal: string): string {
  return `${eventId}:${occurrenceLocal}`;
}

/** The daemon's own default window and working hours — `GET /calendar/config`, always 200. */
export interface CalendarConfigView {
  default_tz: string;
  working_hours_start: string;
  working_hours_end: string;
  working_weekdays: string[];
}

/** `GET /calendar/busy` — always `200 {"busy": boolean}`, no error path; `calendar.rs:368-378` fails OPEN. */
export interface BusyView {
  busy: boolean;
}

/**
 * The expanded occurrences in `[from, to)` — `GET /calendar/events?from&to`.
 *
 * Both bounds REQUIRED, full RFC 3339 with an offset (`calendar.rs:669-697`);
 * a date-only string is a 400. The route does the recurrence expansion, so
 * what comes back is already flat occurrences, never a series to expand
 * client-side.
 */
export function useCalendarEvents(from: string, to: string) {
  return useQuery({
    queryKey: keys.calendar.events(from, to),
    queryFn: () =>
      apiFetch<EventOccurrence[]>(
        `/calendar/events?from=${encodeURIComponent(from)}&to=${encodeURIComponent(to)}`,
      ),
    refetchInterval: POLL.slow,
    placeholderData: keepPreviousData,
  });
}

/** Whether this machine currently reads as busy. */
export function useBusy() {
  return useQuery({
    queryKey: keys.calendar.busy,
    queryFn: () => apiFetch<BusyView>("/calendar/busy"),
    refetchInterval: POLL.fast,
  });
}

/** The calendar's own configuration. Read-only from this page. */
export function useCalendarConfig() {
  return useQuery({
    queryKey: keys.calendar.config,
    queryFn: () => apiFetch<CalendarConfigView>("/calendar/config"),
    refetchInterval: POLL.slow,
  });
}

/**
 * What `POST /calendar/events` accepts.
 *
 * Recurrence is five FLAT fields, not a nested object, and there is no
 * "never" value for `freq` — a one-off simply OMITS it. Every optional field
 * below must therefore be capable of being genuinely absent from the JSON
 * body, not merely `null`: `JSON.stringify` drops an `undefined` property,
 * which is what {@link useCreateEvent}'s callers rely on to send a one-off.
 */
export interface CreateEventRequest {
  title: string;
  /** `"YYYY-MM-DDTHH:MM:SS"` — no offset, no `Z`. */
  starts_at_local: string;
  duration_minutes: number;
  tz?: string | null;
  freq?: "daily" | "weekly" | "monthly" | null;
  interval?: number | null;
  /** `"MO,WE,FR"` | `"TH#3"` | `"FR#-1"`. */
  byday?: string | null;
  until_local?: string | null;
  /** Never both `count` and `until_local`. */
  count?: number | null;
}

export interface CreatedEvent {
  id: number;
}

/** Write a new series (or a one-off, which is a series of one). Always `source: "human"`. */
export function useCreateEvent() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (input: CreateEventRequest) =>
      apiFetch<CreatedEvent>("/calendar/events", { method: "POST", body: JSON.stringify(input) }),
    retry: false,
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: keys.calendar.all });
    },
  });
}

/** The body `cancel` and `move` share — `calendar.rs:817-877`. */
interface OccurrenceRequest {
  /** Required on both doors — the ORIGINAL local start, the identity. */
  occurrence_local: string;
  /** Move only: the NEW local datetime, a datetime and never a delta. */
  to_local?: string | null;
  /** Move only, must be > 0. */
  duration_minutes?: number | null;
}

export interface CancelOccurrenceInput {
  eventId: number;
  occurrenceLocal: string;
}

/** Skip one occurrence of a series, addressed by `occurrence_local` — there is no occurrence id. */
export function useCancelOccurrence() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: ({ eventId, occurrenceLocal }: CancelOccurrenceInput) =>
      apiFetch<void>(`/calendar/events/${eventId}/cancel`, {
        method: "POST",
        body: JSON.stringify({ occurrence_local: occurrenceLocal } satisfies OccurrenceRequest),
      }),
    retry: false,
    onSettled: () => {
      void queryClient.invalidateQueries({ queryKey: keys.calendar.all });
    },
  });
}

export interface MoveOccurrenceInput {
  eventId: number;
  occurrenceLocal: string;
  toLocal: string;
  durationMinutes: number;
}

/**
 * Move one occurrence to a new local datetime.
 *
 * `occurrence_local` stays the ORIGINAL start even after this succeeds — it
 * is what makes moving the same occurrence a second time relocate it rather
 * than write a second exception row.
 */
export function useMoveOccurrence() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: ({ eventId, occurrenceLocal, toLocal, durationMinutes }: MoveOccurrenceInput) =>
      apiFetch<void>(`/calendar/events/${eventId}/move`, {
        method: "POST",
        body: JSON.stringify({
          occurrence_local: occurrenceLocal,
          to_local: toLocal,
          duration_minutes: durationMinutes,
        } satisfies OccurrenceRequest),
      }),
    retry: false,
    onSettled: () => {
      void queryClient.invalidateQueries({ queryKey: keys.calendar.all });
    },
  });
}

/** Delete the whole series — every occurrence, past and future, not just one. */
export function useDeleteSeries() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (eventId: number) => apiFetch<void>(`/calendar/events/${eventId}`, { method: "DELETE" }),
    retry: false,
    onSettled: () => {
      void queryClient.invalidateQueries({ queryKey: keys.calendar.all });
    },
  });
}

/** Whether one local day is ordinary, short, or long, and by how much. */
export interface DayHours {
  /** 23, 24 or 25. */
  hours: number;
  short: boolean;
  long: boolean;
}

/**
 * The short/long-day fact for one local day, computed rather than asked for
 * — see this module's header for why `GET /calendar/config` cannot answer
 * this. Built on `dayBounds`/`hoursInSpan` from `lib/calendar-grid.ts`,
 * which are read-only and untouched by this slice.
 */
export function dayHours(day: Date): DayHours {
  const [start, end] = dayBounds(day);
  const hours = hoursInSpan(start, end);
  return { hours, short: hours < 24, long: hours > 24 };
}
