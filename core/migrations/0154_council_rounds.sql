-- The council deliberates in ROUNDS: an answer, then any number of critique-and-revise rounds, then
-- a synthesis. One row per (seat, round, phase) in a new table, instead of one column triple per
-- phase on the seat row.
--
-- Why a table and not a fourth and fifth triple of columns. `0136_council_revision.sql` already
-- paid for one round by adding three columns, and said why it would not renumber the chairman to
-- make room. A council of N rounds would need 3N columns decided at schema time; a row per step
-- needs none, and the number of rounds becomes data — which is what it is.
--
-- ADDITIVE ONLY. Nothing is dropped here: the old `stage1_*`, `stage2_*`, `rankings`, `revision_*`
-- columns and `council_runs.stage` stay, and the setters that write them also write the new table
-- until every reader has moved over. The drops come later, appended to this file before it lands,
-- once nothing reads the old shape. Until then both shapes are written and the new one is authoritative
-- for nobody yet.
--
-- `council_runs.stage` is kept DELIBERATELY, beyond that transition: `job.rs` and `hooks.rs` tests
-- insert council rows that name it, and a column their fixtures write is not this migration's to
-- take away.
--
-- Numbered 0154: master's highest when this was written was 0153. The rule `0065_council.sql`
-- states at length stands — the BRANCH gives way, master's lineage stands — so re-check the number
-- at every merge.

-- One step of one seat in one council.
CREATE TABLE IF NOT EXISTS council_rounds (
    council_id TEXT NOT NULL,
    -- 0 is the answer round; critique and revise share round 1, 2, ... — a revision answers the
    -- critique of the same round.
    round      INTEGER NOT NULL,
    seat_idx   INTEGER NOT NULL,
    -- answer | critique | revise.
    phase      TEXT NOT NULL,
    -- The `runs` row that produced this step. A logical foreign key, not a declared one, for the
    -- reason `0065_council.sql` gives for `chairman_run_id`: `runs` rows are pruned on their own
    -- schedule.
    run_id     INTEGER,
    -- The seat vocabulary (pending | ok | error | timeout | cancelled | skipped), plus `invalid`:
    -- the run finished and what it wrote could not be read.
    status     TEXT NOT NULL,
    error      TEXT,
    -- What the step produced that is NOT prose. A critique's `{"reviews": [...], "ranking": [...]}`;
    -- NULL for an answer and a revision, whose prose lives in the run's transcript, never here.
    payload    TEXT,
    PRIMARY KEY (council_id, round, seat_idx, phase)
);

-- Steps go when their council goes, as the seats do (`council_seats_follow_councils`, 0065).
CREATE TRIGGER IF NOT EXISTS council_rounds_follow_councils
AFTER DELETE ON council_runs BEGIN
    DELETE FROM council_rounds WHERE council_id = old.id;
END;

-- What a seat is FOR in this council (e.g. a devil's advocate). NULL is an ordinary seat, which is
-- every seat recorded before this column existed.
ALTER TABLE council_seats ADD COLUMN role TEXT;

-- How many critique rounds actually ran, which is not `rounds` (what was asked for): a council can
-- stop early when the seats converge, and one whose critique was skipped ran none.
ALTER TABLE council_runs ADD COLUMN rounds_run INTEGER NOT NULL DEFAULT 0;
-- 1 when the council stopped before `rounds` because nothing was left to change.
ALTER TABLE council_runs ADD COLUMN stopped_early INTEGER NOT NULL DEFAULT 0;
-- The chairman's structured synthesis, and how producing it ended. NULL until the chairman runs,
-- and NULL on every council recorded before this — its synthesis is the chairman run's transcript.
ALTER TABLE council_runs ADD COLUMN synthesis_json TEXT;
ALTER TABLE council_runs ADD COLUMN synthesis_status TEXT;
-- Where the council is: a round and a phase, replacing the single `stage` number, which could not
-- count past the one revision round it was designed around. Phase is answer | critique | revise |
-- chairman | done.
ALTER TABLE council_runs ADD COLUMN current_round INTEGER NOT NULL DEFAULT 0;
ALTER TABLE council_runs ADD COLUMN current_phase TEXT NOT NULL DEFAULT 'answer';

-- The copy. A phase that is still at the column default — `pending`, no run attached — is NOT
-- copied: on a one-round council the revision columns sit at `pending` for a phase that was never
-- part of it (0136 says so), and on a live council a `pending` with no run is a step not yet begun,
-- which the driver will write when it begins. A `pending` WITH a run is in flight and is copied.

-- Phase 1: the answers, round 0. No payload — the prose is in the transcript.
INSERT INTO council_rounds (council_id, round, seat_idx, phase, run_id, status, error, payload)
SELECT council_id, 0, seat_idx, 'answer', stage1_run_id, stage1_status, stage1_error, NULL
  FROM council_seats
 WHERE stage1_run_id IS NOT NULL OR stage1_status <> 'pending';

-- Phase 2: the ranking becomes round 1's critique. The old `[{"anon", "rank"}]` vote becomes the
-- new ordered ballot — labels best first, ties in the order they were stored — with no reviews,
-- because the old council never asked for any. A blank vote (`[]`) stays a blank ballot; a missing
-- one (NULL, the seat failed this phase) stays no payload. The ballot is wrapped in `json()` so it
-- nests as an array and not as a string, whatever SQLite does with the subtype across a subquery.
INSERT INTO council_rounds (council_id, round, seat_idx, phase, run_id, status, error, payload)
SELECT council_id, 1, seat_idx, 'critique', stage2_run_id, stage2_status, stage2_error,
       CASE WHEN rankings IS NULL THEN NULL ELSE
           json_object(
               'reviews', json('[]'),
               'ranking', json((
                   SELECT json_group_array(a) FROM (
                       SELECT json_extract(r.value, '$.anon') AS a
                         FROM json_each(council_seats.rankings) r
                        ORDER BY json_extract(r.value, '$.rank'), CAST(r.key AS INTEGER)
                   )
               ))
           )
       END
  FROM council_seats
 WHERE stage2_run_id IS NOT NULL OR stage2_status <> 'pending';

-- The revision: round 1's revise.
INSERT INTO council_rounds (council_id, round, seat_idx, phase, run_id, status, error, payload)
SELECT council_id, 1, seat_idx, 'revise', revision_run_id, revision_status, revision_error, NULL
  FROM council_seats
 WHERE revision_run_id IS NOT NULL OR revision_status <> 'pending';

-- A round ran when at least one critique of it was not skipped. Below two valid answers phase 2
-- was skipped whole, and that is no round of deliberation.
UPDATE council_runs
   SET rounds_run = CASE WHEN EXISTS (
           SELECT 1 FROM council_rounds s
            WHERE s.council_id = council_runs.id AND s.phase = 'critique' AND s.status <> 'skipped'
       ) THEN 1 ELSE 0 END;

-- The position. A settled council is `done` at the round it reached, whatever stopped it — its
-- status already says how it ended. A running one maps its stage: 1 answers, 2 critiques, 3 is the
-- revision when there was a second round and otherwise the chairman, and anything past that is the
-- chairman (the last stage, `council::stages_total`).
UPDATE council_runs
   SET current_round = CASE
           WHEN status <> 'running' THEN rounds_run
           WHEN stage <= 1 THEN 0
           ELSE 1
       END,
       current_phase = CASE
           WHEN status <> 'running' THEN 'done'
           WHEN stage <= 1 THEN 'answer'
           WHEN stage = 2 THEN 'critique'
           WHEN stage = 3 AND rounds >= 2 THEN 'revise'
           ELSE 'chairman'
       END;
