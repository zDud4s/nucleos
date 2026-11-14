-- Devtime now ingests the transcripts of daemon runs that are bound to a tree node (spec
-- 2026-11-12 section 3.1). The session carries the run and node it belongs to, and the findings of
-- the rules pass inherit them. Both stay NULL for the owner's own sessions.
ALTER TABLE devtime_sessions ADD COLUMN run_id INTEGER;
ALTER TABLE devtime_sessions ADD COLUMN node_id TEXT;
ALTER TABLE devtime_findings ADD COLUMN run_id INTEGER;
ALTER TABLE devtime_findings ADD COLUMN node_id TEXT;
CREATE INDEX idx_devtime_findings_node ON devtime_findings (node_id) WHERE node_id IS NOT NULL;
