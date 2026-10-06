-- What a person said INTO a chat turn that was already running ("Send now", spec 2026-10-04
-- slice 8): the text goes down the live process's stdin and the CLI folds it into the turn, so
-- nothing in the stream says it was ever typed. This row is the only record, and the transcript
-- draws it under the turn it landed in.
--
-- No CHECK constraints, for the house's reason (`0143_knowledge.sql`): `origin` is a Rust
-- constant (`Origin::as_wire`). No REFERENCES and no `project_id`: the row belongs to a chat and
-- to a run, and `run_id` is held by value so the record outlives either.

CREATE TABLE chat_said_now (
    id INTEGER PRIMARY KEY,
    chat_id TEXT NOT NULL,
    run_id INTEGER NOT NULL,
    text TEXT NOT NULL,
    origin TEXT NOT NULL,
    created_at TEXT NOT NULL
);

CREATE INDEX chat_said_now_chat ON chat_said_now (chat_id, id);
