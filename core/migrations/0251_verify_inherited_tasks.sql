-- Task-claims phase 5: what a verify ticket needs to tell an inherited red from its own.
--
-- `verify_runs.failed_tests`: JSON `{"tests": [...], "files": [...]}` of the ids the runner printed
-- for a failed unit (`verify::failures::extract`); NULL when the runner gave none.
-- `verify_requests.inherited_task_ids`: JSON array of `task_claims.id` this ticket's red units were
-- classified as belonging to; the close notice (`task_claims::sessions::take_notices`) joins on it.
-- `verify_requests.classification`: the draft or final per-unit classification
-- (`verify::inherit::Classification`), NULL until a done ticket with failed units is first read.
-- `verify_requests.probe_outcome`: how the master proof this ticket started ended
-- (`verify::probe::ProbeOutcome`); its own column because the proof task writes it while readers
-- rewrite `classification`.
--
-- No CHECK constraints (the rule of 0170_verify_requests.sql).

ALTER TABLE verify_runs ADD COLUMN failed_tests TEXT;
ALTER TABLE verify_requests ADD COLUMN inherited_task_ids TEXT NOT NULL DEFAULT '[]';
ALTER TABLE verify_requests ADD COLUMN classification TEXT;
ALTER TABLE verify_requests ADD COLUMN probe_outcome TEXT;

CREATE INDEX verify_requests_inherited ON verify_requests (project_id)
    WHERE inherited_task_ids != '[]';
