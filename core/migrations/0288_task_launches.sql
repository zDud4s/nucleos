-- Automatic launches of a ticket (spec 2026-11-06-tickets-globais). `launch_attempts` counts the
-- runs the daemon started for the ticket (capped at 2); `last_launch_at` is when it last did. Past
-- the cap the ticket is parked into an ownerless decision and `parked_at` is set: a parked ticket
-- stays `open` and claimable, and claiming it clears `parked_at`. Times are `wave::stamp` texts
-- bound from Rust. No CHECK constraints (the rule of 0143).
ALTER TABLE task_claims ADD COLUMN launch_attempts INTEGER NOT NULL DEFAULT 0;
ALTER TABLE task_claims ADD COLUMN last_launch_at TEXT;
ALTER TABLE task_claims ADD COLUMN parked_at TEXT;
