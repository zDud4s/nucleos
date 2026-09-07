-- A second deliberation round: each seat revises its own answer in the light of the ranking, and
-- the chairman synthesises the revised answers rather than the first ones.
--
-- Called **revision** and not `stage3`, which is the tempting name and the wrong one. The chairman
-- has been phase 3 since `0065_council.sql` and is read as phase 3 by the shell, by the feed lines
-- and by every council already in this database. Renumbering it to make room for a round that most
-- councils will never run would break readers that have nothing to do with this feature, to save
-- one word. The chairman is THE LAST PHASE, not phase 3 — `council::stages_total` is where that is
-- now said out loud.
--
-- **Opt-in, and off by default.** `rounds` defaults to 1 here and in `CouncilConfig`, so every
-- council already recorded reads back as the three-phase council it was, and a daemon whose
-- `~/.nucleos/council.yaml` says nothing about rounds keeps running exactly what it ran yesterday.
-- That default is not tidiness: a second round asks every seat the question again, so it roughly
-- doubles what phase 1 cost. Nobody is going to pay that by accident.
--
-- Numbered 0136, having been planned as 0131 and written as 0134. Master landed 0130 through 0133
-- between the plan and the file, and then 0134 and 0135 between the file and the landing, and the
-- rule this repository keeps is the one `0065_council.sql` states at length: the BRANCH gives way,
-- master's lineage stands. A number is not reserved by being written down.
--
-- Twice is worth recording rather than smoothing over, because the second time was not bad luck.
-- The queue publishes a merge without gating it, so two branches can each land a 0134 that was
-- green on its own — which is what happened on 2026-09-07 and left master unopenable for an hour
-- (`UNIQUE constraint failed: _sqlx_migrations.version`). A long-lived branch will meet this every
-- time master takes a migration, and the answer is always the same: renumber the half no database
-- has run. This file's was read out of the live database before the rename, exactly as
-- `fix(migrations): 0134 was claimed twice` prescribes — 134 there is `notify policy`, master's,
-- and renaming an APPLIED file changes its checksum and the database then refuses to open at all.

-- The `runs` row that produced this seat's revised answer. NULL until the round starts, and NULL
-- forever on a council of one round. A logical foreign key and not a declared one, for the reason
-- `0065_council.sql` gives for `chairman_run_id`: `runs` rows are pruned on their own schedule.
ALTER TABLE council_seats ADD COLUMN revision_run_id INTEGER;

-- pending | ok | error | timeout | cancelled | skipped — the same vocabulary as `stage2_status`,
-- deliberately, because the shell already reads those six words and a seventh would be a new word
-- for a state it can already draw.
--
-- `skipped` means the ranking did not happen: below two valid answers phase 2 is skipped whole, and
-- revising in the light of a ranking that does not exist is not revising.
--
-- `pending` is what a one-round council leaves here, untouched. That is not an oversight to be
-- tidied into `skipped` later: a council of one round never had a revision phase to skip, and
-- `rounds` on the row is what says so. Writing `skipped` across every seat of every one-round
-- council would be six writes per council to record a phase that was never part of it.
ALTER TABLE council_seats ADD COLUMN revision_status TEXT NOT NULL DEFAULT 'pending';

ALTER TABLE council_seats ADD COLUMN revision_error TEXT;

-- 1 or 2. Copied onto the row rather than read back from configuration, for the third time under
-- this heading and the same reason as `chairman_ref` and `model_ref`: the file is editable, and a
-- council that ran four phases in March has to keep reading as a council that ran four phases.
--
-- Also what `stages_total` is computed from, so a reader watching `stage` climb knows whether 3 is
-- the last one or the second to last.
ALTER TABLE council_runs ADD COLUMN rounds INTEGER NOT NULL DEFAULT 1;
