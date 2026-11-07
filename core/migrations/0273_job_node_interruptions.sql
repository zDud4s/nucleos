-- How many times this item's node run ended `interrupted` (a daemon restart) and the item was put
-- back to `pending` for a fresh run. `runs` carries no per-item link for a non-team item
-- (`runs.item_id` is NULL there, 0145), and `job_nodes.run_id` is overwritten by every new run, so
-- the count lives on the item itself. It survives a restart, which is what bounds a crash loop.
ALTER TABLE job_nodes ADD COLUMN interruptions INTEGER NOT NULL DEFAULT 0;
