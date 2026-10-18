-- Indexes for the queries the shell polls, each chosen from an EXPLAIN QUERY PLAN against a copy of
-- a real database (2026-10-17): every one turned a full-table SCAN into a SEARCH.
--
-- GET /projects (every 3s): the roster counts `runs` awaiting approval, pending `proposals` and
-- open `shadow_decisions` per project, and `last_gate_verdicts` groups `runs` by project.
CREATE INDEX IF NOT EXISTS runs_by_project_status ON runs (project_id, status);
CREATE INDEX IF NOT EXISTS proposals_by_project_status ON proposals (project_id, status);
CREATE INDEX IF NOT EXISTS shadow_decisions_open ON shadow_decisions (run_id) WHERE human_verdict IS NULL;
CREATE INDEX IF NOT EXISTS runs_gate_by_project ON runs (project_id, completed_at, gate_status) WHERE gate_status IS NOT NULL;
-- GET /waiting/count: `proposals WHERE status = 'pending' AND kind = ...`.
CREATE INDEX IF NOT EXISTS proposals_by_status_kind ON proposals (status, kind);
-- GET /runs: `ORDER BY created_at DESC, id DESC LIMIT n`, with or without a status filter.
CREATE INDEX IF NOT EXISTS runs_by_created ON runs (created_at, id);
CREATE INDEX IF NOT EXISTS runs_by_status_created ON runs (status, created_at);
-- GET /assistant/chats: the `working` EXISTS looks for a live turn of the chat.
CREATE INDEX IF NOT EXISTS runs_live_by_chat ON runs (chat_id) WHERE status IN ('running', 'pending');
