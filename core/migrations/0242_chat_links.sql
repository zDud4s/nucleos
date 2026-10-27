-- Browser agents linked to the conversation that asked for them (spec 2026-10-22, section 4.1).
--
-- `chat_links` says which conversation a child (the browser agent) answers to. A child has at most
-- one ACTIVE link (`unlinked_at IS NULL`); relinking closes the active row and opens another, and
-- nothing is deleted, so the history of who a child answered to stays readable.
-- `parent_chat_id` NULL is a link with no mother: only the panel talks to the child.
--
-- No foreign keys, and `chats.chat_id` (TEXT) rather than the spec's `REFERENCES chats(id)`: the
-- primary key of `chats` is `chat_id`, and every reference to a conversation in this schema is
-- unconstrained TEXT, for the reason 0121 gives - archiving a conversation must never be blocked by
-- its history, nor delete it. `created_by_run` is the mother's turn that created the child (NULL
-- for a link made from the panel).
CREATE TABLE chat_links (
    id             INTEGER PRIMARY KEY,
    child_chat_id  TEXT    NOT NULL,
    parent_chat_id TEXT,
    kind           TEXT    NOT NULL,
    created_by_run INTEGER,
    created_at     TEXT    NOT NULL,
    unlinked_at    TEXT
);

-- The one-active-link rule is the database's, not only `chats::links::open`'s care.
CREATE UNIQUE INDEX chat_links_one_active ON chat_links(child_chat_id) WHERE unlinked_at IS NULL;
CREATE INDEX chat_links_by_parent ON chat_links(parent_chat_id) WHERE unlinked_at IS NULL;

-- What happened to each final reply of a linked child (delivered to the mother as a relay, queued
-- behind her busy turn, or refused by the relay gate), so the panel can say "not delivered:
-- <reason>". `reason` is the `relay_refusal_response` slug; NULL unless refused. `relay_id` is the
-- `chat_relays` row when the gate admitted the reply. No foreign keys, as above.
CREATE TABLE chat_link_deliveries (
    id         INTEGER PRIMARY KEY,
    link_id    INTEGER NOT NULL,
    run_id     INTEGER NOT NULL,
    relay_id   INTEGER,
    outcome    TEXT    NOT NULL,
    reason     TEXT,
    created_at TEXT    NOT NULL
);
CREATE INDEX chat_link_deliveries_by_link ON chat_link_deliveries(link_id, id);

-- A refused reply becomes a `browser_reply` notice on the mother, and notices never reach the
-- model on their own. The mother's next turn prefixes the unconsumed ones to its prompt once and
-- stamps them with its run id here, so each is seen exactly once.
ALTER TABLE chat_notices ADD COLUMN consumed_by_run INTEGER;
