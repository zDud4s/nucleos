-- A fix's re-gate got its own queue level between the owner's and the autonomous one (spec
-- autonomy-trees A7), so autonomous became 2 and the post-merge family 3. Rows still waiting were
-- queued under the old numbers; shift them so they keep their place in the order. Finished rows
-- are history and keep the number they ran under.
UPDATE verify_runs SET priority = priority + 1 WHERE status = 'queued' AND priority >= 1;
