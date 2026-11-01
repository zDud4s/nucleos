-- Tree decisions: what a tree job parked on and asked its owner, and the answer it got.
--
-- `status` and `origin` are closed vocabularies validated in Rust, not here, like `tree_jobs.state`.
-- `tree_id` is `tree_jobs.id` and `node_id` is the node of the branch that parked; neither is a
-- foreign key, and a row may have no tree at all. `origin` is who wrote the row last: the agent that
-- asked, then the channel that answered. `at` is RFC 3339 text, the house convention: the time of
-- the question until there is an answer, then the time of the decision.
-- `spec_backlog::store` unions this table with `spec_questions` into the owner's single list.
CREATE TABLE decisions (
    id TEXT PRIMARY KEY,
    tree_id TEXT,
    node_id TEXT,
    question TEXT NOT NULL,
    answer TEXT,
    origin TEXT,
    at TEXT NOT NULL,
    status TEXT NOT NULL
);

CREATE INDEX decisions_status ON decisions (status, at);
CREATE INDEX decisions_tree ON decisions (tree_id);
