-- One row per change of a project's post-merge gate colour (spec §5.4, §10).
--
-- `postgate_state` holds only the present. `verify::postgate` writes a row here in the same
-- transaction as the state UPDATE, and only when the colour changed. No foreign key to `projects`:
-- removing a project must not erase its health history. Kept 365 days.
CREATE TABLE postgate_transitions (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    project_id  TEXT    NOT NULL,
    at          TEXT    NOT NULL,                    -- RFC 3339 UTC
    to_state    TEXT    NOT NULL CHECK (to_state IN ('red', 'green')),
    sha         TEXT,                                -- the tip the deciding gate ran on
    red_groups  TEXT    NOT NULL DEFAULT '[]',       -- JSON, as in postgate_state
    reason      TEXT,                                -- 'gate' | 'flake' | 'covered'
    reverted    INTEGER NOT NULL DEFAULT 0           -- 1 on the green that closed a red by revert
);
CREATE INDEX postgate_transitions_project_at ON postgate_transitions (project_id, at);
