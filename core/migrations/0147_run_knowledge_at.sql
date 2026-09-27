-- brief::prune deletes by at alone every hour; no existing index leads with that column.
CREATE INDEX idx_run_knowledge_at ON run_knowledge (at);
