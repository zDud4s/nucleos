-- Consecutive sweep ticks in which a handoff continuation's launch key stayed reserved without a
-- run (`NodeRunError::Unresolved`). At the cap the sweep clears the reservation and starts over.
ALTER TABLE node_watchdog ADD COLUMN unresolved_ticks INTEGER NOT NULL DEFAULT 0;
