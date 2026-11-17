-- The raw gate-failure fingerprint a learning explains, so the consolidator can match a repeated
-- gate failure to it; NULL for every row that has none. Distinct from cause_key (the lesson's own
-- identity key).
ALTER TABLE knowledge ADD COLUMN gate_signature TEXT;
CREATE INDEX knowledge_gate_signature
  ON knowledge (scope_kind, scope_id, gate_signature)
  WHERE gate_signature IS NOT NULL;
