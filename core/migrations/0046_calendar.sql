-- The calendar pillar. Local-first: there is no external provider to read, so the núcleo IS the
-- calendar rather than a mirror of one. That is why no cursor, no watermark and no sidecar appear
-- anywhere below — the three things every other ingestion table in this schema needs.
--
-- The calendar answers exactly one question for the rest of the system: may this person be
-- interrupted right now. It deliberately does NOT touch triage classes (see priority.rs's
-- `o_derivado_nunca_promove`): "I am in meetings until 18h" does not make a message less
-- important, it makes me unreachable, and conflating the two would re-decide importance from a
-- fact about my day.

CREATE TABLE calendar_events (
    id               INTEGER PRIMARY KEY,

    -- LOCAL wall-clock, with no offset, plus the IANA zone it was meant in.
    --
    -- This contradicts the rest of the schema on purpose. Budget windows and the scheduler are
    -- UTC-anchored (recorded in .ai/decisions.md) because they are machine windows; an appointment
    -- is a human commitment. A weekly 09:00 must still be 09:00 after the clocks change, and a UTC
    -- instant slides by an hour twice a year. Storing what the person MEANT, and resolving it at
    -- expansion time, is the only version of this that survives October.
    starts_at_local  TEXT    NOT NULL,
    tz               TEXT    NOT NULL,

    -- Duration, never an end instant. On a transition night "starts 01:30, ends 02:30" is either
    -- ambiguous or impossible, while "starts 01:30, lasts 60 minutes" is always exactly one
    -- interval.
    duration_minutes INTEGER NOT NULL,

    title            TEXT    NOT NULL,

    -- 'human' | 'proposal'. An event the agent put here must stay distinguishable from one you
    -- put here, or the calendar stops being evidence of anything.
    source           TEXT    NOT NULL,
    source_ref       INTEGER,

    -- Recurrence. All NULL for a one-off event, which is the common case and costs nothing.
    -- The stored subset is daily/weekly/monthly with an interval, BYDAY ('MO,WE,FR' for weekly,
    -- 'TH#3' or 'FR#-1' for monthly-by-position), and either a COUNT or an UNTIL. That covers
    -- human calendars; it is not RFC 5545 and does not pretend to be.
    freq             TEXT,
    interval_n       INTEGER,
    byday            TEXT,
    until_local      TEXT,
    count_n          INTEGER,

    created_at       TEXT    NOT NULL
);

CREATE INDEX calendar_events_by_start ON calendar_events (starts_at_local);

-- One occurrence of a series, cancelled or moved.
--
-- Identified by the occurrence's ORIGINAL local start, which is what RFC 5545 calls RECURRENCE-ID
-- and is the only identity that survives the series being edited around it. Storing an index
-- ("the 4th one") would renumber every time an earlier occurrence was cancelled.
--
-- "This and all following" needs nothing here: set `until_local` on the old rule and insert a new
-- event starting where the change begins. Falling out of the schema for free is the reason the
-- exception table only has to model two verbs.
CREATE TABLE calendar_exceptions (
    id                     INTEGER PRIMARY KEY,
    event_id               INTEGER NOT NULL REFERENCES calendar_events(id) ON DELETE CASCADE,
    occurrence_local       TEXT    NOT NULL,
    kind                   TEXT    NOT NULL,
    moved_to_local         TEXT,
    moved_duration_minutes INTEGER,
    UNIQUE (event_id, occurrence_local)
);

-- Notifications held back because the calendar said the person was busy.
--
-- This is NOT a new notification channel. A row in `feed` already IS a notification — the Telegram
-- notifier forwards every new feed entry without looking at its kind, which is why triage.rs
-- filters on the writing side. So deferring a notification means deferring the feed row, and this
-- table is where it waits. The sidecar needs no change at all.
--
-- Delivered rows are kept rather than deleted: the failure everyone fears here is the calendar
-- silently swallowing something, and a table that only holds what has not arrived yet cannot answer
-- whether that happened.
CREATE TABLE pending_notifications (
    id           INTEGER PRIMARY KEY,
    kind         TEXT NOT NULL,
    summary      TEXT NOT NULL,
    queued_at    TEXT NOT NULL,
    delivered_at TEXT
);

CREATE INDEX pending_notifications_undelivered
    ON pending_notifications (queued_at)
    WHERE delivered_at IS NULL;
