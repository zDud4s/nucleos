-- Lookups on `runs` that had no index since 0053 dropped the `project_id` one and 0154 rebuilt the
-- table with only chat_id, (mode, completed_at), team_run_id, from_relay_id and lineage_root_id.
-- Each of these was a full scan of a 229 MB table:
--   * `WHERE job_id = ?` (distill's review and failed-gate readers, budget's job rows);
--   * `WHERE status = 'awaiting_approval'` (the approvals list, polled);
--   * `WHERE project_id = ? AND status = 'running'` (consolidation's "is the project busy" probe),
--     answered by 0198's `runs_by_project_status`, the same (project_id, status) index.
--
-- Numbered 0202: it landed as a second 0198, and sqlx keys migrations by number alone, so a fresh
-- database failed on `_sqlx_migrations.version` and an existing one on the checksum.
--
-- Partial where the column is mostly empty or the value rare, so the index stays small. A partial
-- index is only chosen when the query repeats its WHERE term literally, which is why the status one
-- is `= 'awaiting_approval'` and not an IN list.
CREATE INDEX IF NOT EXISTS idx_runs_job_id ON runs(job_id) WHERE job_id IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_runs_awaiting_approval ON runs(id) WHERE status = 'awaiting_approval';
