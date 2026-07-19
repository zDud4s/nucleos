CREATE UNIQUE INDEX one_open_worktree_run_per_project
    ON runs (project_id)
    WHERE mode = 'worktree' AND status IN ('running', 'awaiting_approval');
