-- The runs that extracted a spec's criteria. An audit must not be done by a run that shaped the
-- criteria it checks, so they count as part of the spec's own lineage. Insertion order is kept in
-- `seq`; a repeated pair changes nothing. No foreign keys, ids as TEXT.
CREATE TABLE spec_extraction_runs (
    seq        INTEGER PRIMARY KEY AUTOINCREMENT,
    spec_id    TEXT NOT NULL,
    run_id     TEXT NOT NULL,
    created_at TEXT NOT NULL,
    UNIQUE (spec_id, run_id)
);
