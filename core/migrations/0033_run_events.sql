-- Records each event in a run's trajectory so later analysis can address it independently.
--
-- A run's trajectory was one TEXT blob, which left no event sequence to replay, no individual
-- tool call to receive cost attribution, and nothing an ablation could score. `shadow_decisions`
-- does not fill that gap: it only ever saw calls that reached the cooperative hook, so it could
-- not describe the complete trajectory the runner actually emitted.
CREATE TABLE IF NOT EXISTS run_events (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    run_id INTEGER NOT NULL,
    seq INTEGER NOT NULL,
    kind TEXT NOT NULL,
    payload TEXT NOT NULL,
    created_at TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_run_events_run_id_seq ON run_events (run_id, seq);
