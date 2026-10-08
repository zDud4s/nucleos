-- What the post-merge gate does with a red (spec 2026-10-05 §6.2; F3-3): recheck on the same sha,
-- bisect the first-parent merges of (red_base_sha, red_sha], report to the owner. All of it is
-- kept on the project's row so a restart resumes where it stopped. No CHECK constraints, as in
-- 0180. `red_phase` is NULL when no red is being handled, else 'flake_check' or 'bisect'.
-- `probes`, `candidates` and `also_suspect` are JSON arrays.
ALTER TABLE postgate_state ADD COLUMN red_phase TEXT;
ALTER TABLE postgate_state ADD COLUMN red_sha TEXT;
ALTER TABLE postgate_state ADD COLUMN red_base_sha TEXT;
ALTER TABLE postgate_state ADD COLUMN probe_sha TEXT;
ALTER TABLE postgate_state ADD COLUMN probe_request_id INTEGER;
ALTER TABLE postgate_state ADD COLUMN probes TEXT NOT NULL DEFAULT '[]';
ALTER TABLE postgate_state ADD COLUMN reported_sha TEXT;
ALTER TABLE postgate_state ADD COLUMN culprit_sha TEXT;
ALTER TABLE postgate_state ADD COLUMN candidates TEXT NOT NULL DEFAULT '[]';
ALTER TABLE postgate_state ADD COLUMN also_suspect TEXT NOT NULL DEFAULT '[]';
