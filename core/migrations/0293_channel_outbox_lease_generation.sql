-- The lease generation of every outbox row (contract 1.1, finding rel-channels#1): it rises by one
-- on each lease, the poll hands it to the adapter with the action, and every ack echoes it back.
-- An ack whose generation no longer matches the row is the late answer of a lease that expired
-- and was handed out again, so it is ignored instead of settling or re-pending the row under the
-- delivery that holds it now. Existing rows start at 0; their next lease is generation 1.
ALTER TABLE channel_outbox ADD COLUMN lease_generation INTEGER NOT NULL DEFAULT 0;
