-- Slot leases for the controller's execution waves (perfil-de-velocidade spec §4.6).
--
-- A wave's workers are local processes, so they hold the same slots as the daemon's own work: in
-- `project_slots` as owner_kind 'wave' with owner_id = `wave_workers.id`. One row per worker,
-- because a slot is one per owner and a wave of three needs three.
--
-- The lease lives on `waves`. A wave is live while `released_at` is NULL and `renewed_at` is recent,
-- which `concurrency.rs` judges against a cutoff computed and bound from Rust — never SQLite's
-- `datetime('now')`, whose spelling does not sort against the RFC 3339 written here.
--
-- AUTOINCREMENT on both tables, deliberately: a grant deletes the worker row it could not seat, and
-- without it the next insert would reuse that id — a slot key naming two different workers over time.
--
-- No `project_id` here, and that is not an omission: the project a wave's slots count against is on
-- each of its `project_slots` rows already, and a second copy would make this a project-scoped table
-- that `project_exit::PROJECT_SCOPED` has to list and forget — for a column nothing reads.
CREATE TABLE waves (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    created_at  TEXT    NOT NULL,
    renewed_at  TEXT    NOT NULL,
    released_at TEXT
);

CREATE TABLE wave_workers (
    id      INTEGER PRIMARY KEY AUTOINCREMENT,
    wave_id INTEGER NOT NULL REFERENCES waves (id)
);

CREATE INDEX wave_workers_by_wave ON wave_workers (wave_id);
