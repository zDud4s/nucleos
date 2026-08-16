-- One open refused action per run, the same guarantee `one_open_action_proposal_per_run`
-- already gives the kind beside it and for the same reason: a model told no tries again, and a
-- person who opens their phone to eleven copies of one question stops reading the list.
--
-- An index rather than a check-then-insert in the handler, because two attempts arriving together
-- would both find nothing there and both write. The database is the only place that can answer
-- this without a race.
--
-- 0074 and not 0070: `master` has already spent 0068 through 0072, and 0073 is claimed twice over
-- on two unmerged branches (`feat/equipas-de-agentes` and `feat/pilar-de-browser`). 0074 is free on
-- every branch this repository currently has.
CREATE UNIQUE INDEX one_open_refused_action_per_run
    ON proposals (run_id)
    WHERE status = 'pending' AND kind = 'refused-action';
