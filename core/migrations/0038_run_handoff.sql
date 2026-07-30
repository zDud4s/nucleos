-- Keeps context-pressure transitions inspectable after the process that observed them is gone.
--
-- A handoff needs both the last known fill and an explicit path to the run that continued the
-- work; without those durable facts, continuation would be indistinguishable from an unrelated
-- run and the reason for ending the original would disappear.
ALTER TABLE runs ADD COLUMN context_fill INTEGER;
ALTER TABLE runs ADD COLUMN successor_run_id INTEGER REFERENCES runs(id);
