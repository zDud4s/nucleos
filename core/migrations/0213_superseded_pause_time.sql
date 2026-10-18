-- One-off data repair: give every superseded run back the time it paused.
--
-- Until this branch, approving or declining a paused run overwrote its `completed_at` with the
-- approval time (`runs/resume.rs`), although the process had ended at the pause. A run with no
-- reported cost is priced by elapsed time, so the hours the owner took to answer were billed:
-- run 900428 asked after 4 minutes and was superseded 30.9 hours later. The approval time is
-- still in `proposals.decided_at`.
--
-- The pause time itself was overwritten, so the proposal the pause wrote stands in for it: the
-- latest proposal of the run between its start and its recorded end. It is written just after the
-- pause, so this errs a few milliseconds late, toward the estimate, which is the safe side. A run
-- with no such proposal is left as it is. `julianday` rather than text comparison, because the
-- two columns are not guaranteed one spelling of UTC (`Z` against `+00:00`); an unparsable value
-- compares as NULL and leaves its row untouched.
UPDATE runs
   SET completed_at = (
       SELECT p.created_at FROM proposals p
        WHERE p.run_id = runs.id
          AND julianday(p.created_at) >= julianday(runs.created_at)
          AND julianday(p.created_at) <= julianday(runs.completed_at)
        ORDER BY julianday(p.created_at) DESC, p.id DESC
        LIMIT 1)
 WHERE status = 'superseded'
   AND EXISTS (
       SELECT 1 FROM proposals p
        WHERE p.run_id = runs.id
          AND julianday(p.created_at) >= julianday(runs.created_at)
          AND julianday(p.created_at) <= julianday(runs.completed_at));
