-- Indexes for the analytics pipeline's window reads (spec analytics-pipeline §12.6, §13 step 0).
--
-- Each is the one a read needed, found with EXPLAIN QUERY PLAN over the schema after migration
-- 0208 (the `the_pipeline_window_reads_search_an_index_instead_of_scanning` test keeps the check
-- alive). Every read has the shape `col >= ? AND col < ? AND (? IS NULL OR project_id = ?)`: the
-- OR makes a `(project_id, col)` index unusable, so the bare time column is what serves both the
-- global and the per-project call. Before, each of them printed a full SCAN:
--   landing, post-merge reverts   SCAN vcs_requests
--   gate                          SCAN verify_runs + USE TEMP B-TREE FOR ORDER BY
--   jobs                          SCAN jobs
--   classifier                    SCAN shadow_decisions
--   refused actions               SCAN proposals USING INDEX proposals_by_status_kind (a full
--                                 walk of the index: `(status, kind)` cannot serve `kind = ?`)
-- `judge_verdicts_by_time` already exists. `postgate_state` (one row per project) and
-- `postgate_transitions` (a few rows per incident, read whole up to the window's end) are exempt.
--
-- Two more for the reads metrics 2 and 3 added (still SCAN vcs_requests without them): the
-- conflicts read windows by `COALESCE(settled_at, finished_at)`, which only an index on that exact
-- expression serves, and the owner-blocked read is an OR of `status` terms with no window, which
-- SQLite answers as a multi-index OR once `status` leads an index.

CREATE INDEX IF NOT EXISTS vcs_requests_by_created ON vcs_requests (created_at);
CREATE INDEX IF NOT EXISTS verify_runs_by_started ON verify_runs (started_at);
CREATE INDEX IF NOT EXISTS jobs_by_created ON jobs (created_at);
CREATE INDEX IF NOT EXISTS shadow_decisions_by_created ON shadow_decisions (created_at);
CREATE INDEX IF NOT EXISTS proposals_by_kind_created ON proposals (kind, created_at);
CREATE INDEX IF NOT EXISTS vcs_requests_by_ended
    ON vcs_requests (COALESCE(settled_at, finished_at));
CREATE INDEX IF NOT EXISTS vcs_requests_by_status ON vcs_requests (status);
