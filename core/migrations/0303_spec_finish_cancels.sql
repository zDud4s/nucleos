-- A spec the owner told spec finish not to launch (`POST /backlog/finish/cancel`). Every finish
-- pass, the sweeper's included, leaves a cancelled spec out and checks again right before each
-- launch; a tree already open is never touched. A later finish request that names the spec (by id,
-- or by its whole project) deletes the row. No foreign keys, ids as TEXT.
CREATE TABLE spec_finish_cancels (
    spec_id TEXT PRIMARY KEY,
    created_at TEXT NOT NULL
);
