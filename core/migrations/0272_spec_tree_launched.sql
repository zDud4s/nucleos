-- A spec's recorded tree id is written before the launcher is asked for the tree. This flag says the
-- launcher accepted it, so a recorded id with no tree and the flag still 0 is a crash between the
-- two (the same id is launched again), not a tree that vanished. Rows from before the column default
-- to 0: with a live tree they still wait on it.
ALTER TABLE specs ADD COLUMN tree_launched INTEGER NOT NULL DEFAULT 0;
