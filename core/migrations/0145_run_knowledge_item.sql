-- A non-team item's runs deliberately carry runs.item_id = NULL (0095: NULL means the run works
-- in its job's tree), while spawn_node rewrites job_items.run_id on every start. Record the item
-- on each briefing trace row so a final item verdict can still credit every earlier attempt,
-- independently of handoffs, approvals, successors, or resumes.
ALTER TABLE run_knowledge ADD COLUMN item_id INTEGER REFERENCES job_items(id);

-- The credit sweep runs on every job tick and reads only trace rows it has not credited yet.
CREATE INDEX idx_run_knowledge_uncredited
    ON run_knowledge (item_id, run_id)
    WHERE credited_at IS NULL;
