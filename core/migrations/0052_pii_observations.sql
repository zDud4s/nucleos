-- What a local model would have redacted, recorded without redacting anything.
--
-- The enforcing half of the egress filter (`redact.rs`) only touches what a check digit or an
-- issuer prefix proves. Everything else — a person's name, an address, a line about somebody's
-- health — needs judgement, and a judgement that is sometimes wrong cannot be allowed to mutate a
-- stranger's words on the way to a model. So it observes instead, and this table is what it
-- observes into. Whether any class here graduates to being redacted is a decision to take later,
-- from these rows.
--
-- `excerpt` is stored, and that is the point of the table rather than a detail of it. The question
-- to answer in a few months is "what would redacting class X have cost?", and rows that only COUNT
-- occurrences cannot answer it — they measure how often, when the decision turns on what.
--
-- Storing it is safe here and would not be everywhere, which is why `source_column` exists and is
-- constrained. Every column named below is one the row keeps for ever anyway: a subject, a sender's
-- name, and the summary a LOCAL model wrote. An excerpt of those is a copy of something already
-- permanent. `emails.body_text` is deliberately absent and must stay absent — triage sets it to
-- NULL when it is done, and an excerpt of a body would outlive the body, which is the retention
-- decision undone by a measurement.
CREATE TABLE IF NOT EXISTS pii_observations (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    -- Which row was read, so a finding can be traced back and so a deleted source can take its
    -- observations with it.
    source_table  TEXT NOT NULL,
    source_id     INTEGER NOT NULL,
    -- Constrained rather than free text: the constraint IS the retention guarantee. Adding a column
    -- here is the moment to check that it is one the source keeps.
    source_column TEXT NOT NULL CHECK (source_column IN ('subject', 'from_name', 'triage_summary')),
    -- name | address | health | financial | other
    class         TEXT NOT NULL,
    excerpt       TEXT NOT NULL,
    -- The model's own confidence, kept so a later decision can be made at a threshold rather than
    -- on every hit. A 4B is wrong often enough that "it found something" is not a measurement.
    confidence    REAL,
    -- Which attempt this row records, and it exists only for `unreadable`.
    --
    -- A summary the model garbles must be retried, or one truncated answer excludes it from the
    -- denominator for ever; and it must not be retried indefinitely, or a summary it garbles
    -- deterministically blocks the sweep from reaching anything older. Counting the attempts is
    -- what allows both. A real observation is always attempt 0.
    attempt       INTEGER NOT NULL DEFAULT 0,
    observed_at   TEXT NOT NULL
);

-- The query this table exists to answer is "what did we see, by class", so that is the index.
CREATE INDEX IF NOT EXISTS idx_pii_observations_class ON pii_observations (class, observed_at);

-- One observation pass per source row: re-running triage on a message must not double-count it.
CREATE UNIQUE INDEX IF NOT EXISTS idx_pii_observations_unique
    ON pii_observations (source_table, source_id, source_column, class, excerpt, attempt);

-- Observations go when their subject goes. Without this the table would outlive the mail it
-- describes, which is the exact failure the column constraint above exists to prevent.
CREATE TRIGGER IF NOT EXISTS pii_observations_follow_emails AFTER DELETE ON emails BEGIN
    DELETE FROM pii_observations WHERE source_table = 'emails' AND source_id = old.id;
END;
