-- The council: N seats answer one question, rank each other blind, and a chairman synthesises.
--
-- Two tables and no third. The ANSWERS are not here — they live in the transcript of the `runs` row
-- that produced them, and a seat keeps only that row's id. Copying the text into a column beside it
-- would put the same answer in two places, and two places is where they eventually disagree.
--
-- Numbered 0064, having been cut as 0062 and having been wrong twice before that.
--
-- The design named 0060; by the time this branch was cut, 0060 was an untracked file in the main
-- checkout and 0061 was on `feat/canvas-da-frota`, so it went out as 0062. Master then landed that
-- branch with its two renumbered to 0062 and 0063, and the collision was invisible to git — two
-- files with different names and the same version, which only `sqlx::migrate!` would have caught.
--
-- The rule this repository keeps: the BRANCH gives way, master's lineage stands. Twice before under
-- the same heading ("give 0060 up to the reset that is landing on master"). A number is not
-- reserved by being written down, only by being on master.

-- One deliberation.
CREATE TABLE IF NOT EXISTS council_runs (
    -- A uuid rather than a rowid, because it is also the ANONYMISATION SEED (see `anon_seed`), and
    -- an id a caller can predict is a seed a caller can predict.
    id             TEXT PRIMARY KEY,
    created_at     TEXT NOT NULL,
    -- The owner's question, exactly as written. Never rewritten, never summarised: a seat that is
    -- asked a paraphrase answers the paraphrase.
    question       TEXT NOT NULL,
    -- running | done | error | cancelled. `cancelled` and `error` stay apart on purpose — one says
    -- somebody stopped it, the other says it broke, and the owner is entitled to know which.
    status         TEXT NOT NULL,
    -- Which phase is current (1, 2, 3), for whoever reads the record while it is still being
    -- written. Without it a half-filled council and a council whose seats all failed look alike.
    stage          INTEGER NOT NULL DEFAULT 1,
    -- The seed the label shuffle is derived from — the council's own id, stored again under its own
    -- name.
    --
    -- Redundant on purpose. The shuffle has to be REPRODUCIBLE from the row: `anon_map` below says
    -- which seat each label meant, and this says how that map was arrived at. Deriving the seed
    -- implicitly from `id` would work exactly until somebody changed what it is derived from, and
    -- then every historical row would silently describe a shuffle nobody can recompute.
    anon_seed      TEXT NOT NULL,
    -- `{"A": 2, "B": 0}` — label to `seat_idx`. NULL until phase 1 ends, because the map is over the
    -- seats that ANSWERED, which is not known before then.
    anon_map       TEXT,
    -- `[{"seat_idx": n, "avg_rank": f, "n": k}]`, best first. NULL until phase 2 ends, and still
    -- NULL afterwards when phase 2 was skipped — which is a real state and not an error.
    leaderboard    TEXT,
    -- cloud | local, and the model or agent that presides. Copied onto the row rather than read back
    -- from configuration, so a council stays readable after `.ai/council.yaml` is edited.
    chairman_kind  TEXT NOT NULL,
    chairman_ref   TEXT NOT NULL,
    -- The `runs` row that wrote the synthesis. NULL until phase 3 starts. A logical foreign key and
    -- not a declared one: `runs` rows are pruned on their own schedule, and a council whose seat
    -- rows vanished is worth keeping as the record that it happened.
    chairman_run_id INTEGER,
    -- Why, when `status = 'error'`. Populated for the startup reconciliation too, which is the case
    -- where the reason is the only thing distinguishing this from a council nobody ever ran.
    error          TEXT
);

-- The list is read newest-first and by nothing else.
CREATE INDEX IF NOT EXISTS idx_council_runs_created ON council_runs (created_at DESC);

-- One seat of one deliberation.
CREATE TABLE IF NOT EXISTS council_seats (
    council_id    TEXT NOT NULL,
    -- Position in the roster, and the seat's identity everywhere in the system: the anonymisation
    -- map points at it, the leaderboard is keyed on it, and ties break on it. Never the model name,
    -- which is not unique — the same model twice is a legitimate roster.
    seat_idx      INTEGER NOT NULL,
    kind          TEXT NOT NULL,
    -- Spelled `ref` in `.ai/council.yaml` and `model_ref` here, because `ref` is a Rust keyword and
    -- a column whose name has to be escaped at every use is a column that eventually gets it wrong.
    model_ref     TEXT NOT NULL,
    stage1_run_id INTEGER,
    -- pending | ok | error | timeout | cancelled. `timeout` is distinct from `error` because they
    -- say different things about the model: one refused, the other never finished.
    stage1_status TEXT NOT NULL DEFAULT 'pending',
    stage1_error  TEXT,
    stage2_run_id INTEGER,
    -- The same set plus `skipped`, which is what every seat gets when there were fewer than two
    -- valid answers to rank.
    stage2_status TEXT NOT NULL DEFAULT 'pending',
    stage2_error  TEXT,
    -- `[{"anon": "A", "rank": 1}]` — what the parser could read out of this seat's answer, already
    -- filtered to labels this seat was actually shown. An empty array is a blank vote, not a failure.
    rankings      TEXT,
    PRIMARY KEY (council_id, seat_idx)
);

-- Seats go when their council goes. A council is the only thing that gives a seat meaning, so an
-- orphaned seat row is not a record of anything.
CREATE TRIGGER IF NOT EXISTS council_seats_follow_councils
AFTER DELETE ON council_runs BEGIN
    DELETE FROM council_seats WHERE council_id = old.id;
END;
