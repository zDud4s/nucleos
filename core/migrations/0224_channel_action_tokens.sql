-- Button tokens: single use, valid while the proposal is pending (section 6.3).
CREATE TABLE channel_action_tokens (
    token        TEXT PRIMARY KEY,
    channel      TEXT NOT NULL,
    conversation TEXT NOT NULL,
    action       TEXT NOT NULL CHECK (action IN ('approve', 'reject')),
    proposal_id  INTEGER NOT NULL,
    created_at   TEXT NOT NULL,
    used_at      TEXT
);
CREATE INDEX channel_action_tokens_proposal ON channel_action_tokens (proposal_id);
