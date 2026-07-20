CREATE TABLE repo_trigger_state (
    project_id   TEXT NOT NULL,
    trigger_name TEXT NOT NULL,
    last_sha     TEXT NOT NULL,
    PRIMARY KEY (project_id, trigger_name)
);
