-- One table for both kinds of capture, discriminated by `kind`.
--
-- A dictation and a memo differ in what happens to them, not in what they are: both are a recording
-- that became text. Two tables would duplicate the retention sweep and the cleanup-state column for
-- no gain, and the query that matters -- newest-first within one kind -- is served by the index below
-- either way.
--
-- No audio column, and there never is one: the recording is deleted the moment a transcript exists
-- (transcribe.rs's TempAudio guard), so by the time a row can be written there is nothing to store.
CREATE TABLE voice_captures (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    kind TEXT NOT NULL CHECK (kind IN ('dictation', 'memo')),
    created_at TEXT NOT NULL,
    duration_ms INTEGER NOT NULL,
    -- Kept alongside the cleaned text rather than replaced by it: the raw transcript is what the
    -- cleanup prompt gets tuned against, and it is the only evidence when cleanup makes things worse.
    raw_text TEXT NOT NULL,
    -- NULL when cleanup did not run or was rejected; `cleanup_state` says which.
    clean_text TEXT,
    -- 'cleaned' | 'raw' | 'shrunk'. ONE flag for a state the UI, the API and the shrink guard all
    -- read, because three independent booleans would drift and the interface would end up calling a
    -- document clean that the guard had rejected.
    cleanup_state TEXT NOT NULL CHECK (cleanup_state IN ('cleaned', 'raw', 'shrunk')),
    -- Which model did the cleanup, so a bad batch can be attributed after the config has moved on.
    model TEXT
);

-- Serves both reads that exist: the newest-first listing per kind, and the retention sweep that
-- deletes dictations past their cutoff.
CREATE INDEX voice_captures_kind_created_at ON voice_captures (kind, created_at);
