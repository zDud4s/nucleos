-- Autonomy trees spec B2: a question to the owner may carry a default action that the sweep
-- applies when the deadline passes with no answer. `default_action` is one of the two admitted
-- kinds (`relaunch_fresh_with_diagnosis`, `split_remaining_into_new_nodes`); `default_due` is
-- RFC 3339 text; `default_evidence` is what the feed line shows beside the question. All three are
-- NULL for a question that always waits for the owner. An applied default is the row's own answer
-- with origin `auto_default`, so the caps count from the answered rows and need no counter.
ALTER TABLE decisions ADD COLUMN default_action TEXT;
ALTER TABLE decisions ADD COLUMN default_due TEXT;
ALTER TABLE decisions ADD COLUMN default_evidence TEXT;
CREATE INDEX decisions_default_due ON decisions (default_due) WHERE default_action IS NOT NULL AND status = 'open';
