-- The errands feature was removed on 2026-09-30. This migration deletes what it left behind.
-- Indexes are dropped before their columns, because SQLite refuses DROP COLUMN on an indexed
-- column. Proposal rows stay: they are refused-action records, only their errand link goes.

DELETE FROM knowledge_events WHERE knowledge_id IN (SELECT id FROM knowledge WHERE scope_kind = 'errand');
DELETE FROM run_knowledge WHERE knowledge_id IN (SELECT id FROM knowledge WHERE scope_kind = 'errand');
UPDATE knowledge SET supersedes = NULL WHERE supersedes IN (SELECT id FROM knowledge WHERE scope_kind = 'errand');
DELETE FROM knowledge WHERE scope_kind = 'errand';
DELETE FROM notify_policy WHERE selector LIKE 'errand\_%' ESCAPE '\';
DELETE FROM feed WHERE errand_id IS NOT NULL OR subject LIKE 'errand:%';

DROP INDEX feed_by_errand;
ALTER TABLE feed DROP COLUMN errand_id;
DROP INDEX proposals_by_errand;
ALTER TABLE proposals DROP COLUMN errand_id;

DROP TABLE errand_rules;
DROP TABLE errand_artifacts;
DROP TABLE errands;
