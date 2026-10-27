-- One digest of capture questions per local day (spec 2026-10-22 B7, section 7). The text lives in
-- one `capture_digest` feed line, not here; this table says which jobs the day asked about, so a
-- reply to that line can find its open requests (`capture_requests.digest_id`).
--
-- No CHECK constraints (the rule of 0143_knowledge.sql). No `project_id`: a digest spans projects,
-- so it is deliberately outside `project_exit::PROJECT_SCOPED`; project_exit deletes it through
-- `json_each(job_ids)` and removes its feed line (`capture_digest:<id>`) with it.

CREATE TABLE capture_digests (
  id         INTEGER PRIMARY KEY,
  day        TEXT NOT NULL UNIQUE,   -- local date YYYY-MM-DD; the key, so a 23h or 25h day fires once
  job_ids    TEXT NOT NULL,          -- JSON array of job ids, every request the digest opened
  created_at TEXT NOT NULL           -- capture::stamp
);

-- NULL for a request opened before this migration (the old per-job path) or without a digest.
ALTER TABLE capture_requests ADD COLUMN digest_id INTEGER;

CREATE INDEX capture_requests_digest ON capture_requests (digest_id);
