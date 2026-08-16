CREATE TABLE agents (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL UNIQUE,
    speciality TEXT NOT NULL,
    prompt TEXT NOT NULL,
    engine TEXT NOT NULL,
    model TEXT,
    tool_policy TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE TABLE teams (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL UNIQUE,
    mission TEXT NOT NULL,
    director_agent_id TEXT NOT NULL REFERENCES agents(id),
    max_rounds INTEGER NOT NULL,
    max_parallel INTEGER NOT NULL,
    budget_usd REAL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE TABLE team_members (
    team_id TEXT NOT NULL REFERENCES teams(id),
    agent_id TEXT NOT NULL REFERENCES agents(id),
    PRIMARY KEY (team_id, agent_id)
);

CREATE TABLE team_runs (
    id TEXT PRIMARY KEY,
    team_id TEXT NOT NULL REFERENCES teams(id),
    request TEXT NOT NULL,
    workspace TEXT NOT NULL,
    token TEXT NOT NULL,
    state TEXT NOT NULL,
    director_node TEXT NOT NULL DEFAULT 'none',
    director_run_id INTEGER REFERENCES runs(id),
    round INTEGER NOT NULL DEFAULT 0,
    next_ordinal INTEGER NOT NULL DEFAULT 1,
    dry_rounds INTEGER NOT NULL DEFAULT 0,
    plan_retries INTEGER NOT NULL DEFAULT 0,
    replanned TEXT NOT NULL DEFAULT 'not_yet',
    outcome TEXT,
    why TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    finished_at TEXT
);

CREATE TABLE team_items (
    team_run_id TEXT NOT NULL REFERENCES team_runs(id),
    ordinal INTEGER NOT NULL,
    round INTEGER NOT NULL,
    agent_id TEXT NOT NULL REFERENCES agents(id),
    description TEXT NOT NULL,
    state TEXT NOT NULL,
    run_id INTEGER REFERENCES runs(id),
    output_path TEXT,
    PRIMARY KEY (team_run_id, ordinal)
);
