-- F3-12 (spec 2026-10-09, decisions 2, 5 and 12): the post-merge gate stops repeating what the
-- daemon already measured, and only starts a batch gate when the machine is quiet. No CHECK
-- constraints, as in 0180, 0182 and 0184.
-- `covered_sha` (decision 5) is the newest target tip proven by a daemon-run `verify` request over
-- a green-or-covered base. A cover never moves `last_green_sha`: a later red is bisected from the
-- last COMPLETE green, so a wrong cover costs a longer bisection and never a lost red. A confirmed
-- red clears it (decision 6).
-- `last_started_at` is when the latest gate started, for the minimum interval between gates;
-- `held_note` is the reason a start is being held ('interval' / 'busy'), kept so a hold writes one
-- feed line per reason and episode, and cleared when a gate starts (decision 12).
-- `verify_requests.measured_tree` (decision 2) is the `HEAD^{tree}` of the worktree a `test` /
-- `scope` request measured, written by the daemon only when the worktree was clean before and
-- after the run and its map was trusted; NULL never covers anything.
ALTER TABLE postgate_state ADD COLUMN covered_sha TEXT;
ALTER TABLE postgate_state ADD COLUMN last_started_at TEXT;
ALTER TABLE postgate_state ADD COLUMN held_note TEXT;
ALTER TABLE verify_requests ADD COLUMN measured_tree TEXT;
CREATE INDEX verify_requests_measured_tree
    ON verify_requests (project_id, measured_tree)
    WHERE measured_tree IS NOT NULL;
