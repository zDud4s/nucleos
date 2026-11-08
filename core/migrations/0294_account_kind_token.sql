-- A pool account can be added by pasting a subscription token (`claude setup-token`), so the
-- `kind` CHECK of 0274 admits a third value, 'token'. The token itself is never a column: it lives
-- in the OS credential store under `claude-account-token:<id>`.
--
-- Rebuild-and-copy because SQLite cannot change a CHECK in place (same shape as 0096). Nothing
-- REFERENCES accounts, so the DROP fires no cascade. The unique label index goes with the old table
-- and is recreated below under the same name.
CREATE TABLE accounts_new (
    id                 TEXT PRIMARY KEY,
    engine             TEXT NOT NULL,
    provider           TEXT NOT NULL,
    label              TEXT NOT NULL,
    kind               TEXT NOT NULL DEFAULT 'subscription'
                       CHECK (kind IN ('subscription', 'api_key', 'token')),
    credential_locator TEXT,
    state              TEXT NOT NULL DEFAULT 'active'
                       CHECK (state IN ('active', 'paused', 'needs_login')),
    created_at         TEXT NOT NULL,
    updated_at         TEXT NOT NULL
);

INSERT INTO accounts_new
    (id, engine, provider, label, kind, credential_locator, state, created_at, updated_at)
SELECT id, engine, provider, label, kind, credential_locator, state, created_at, updated_at
FROM accounts;

DROP TABLE accounts;
ALTER TABLE accounts_new RENAME TO accounts;
CREATE UNIQUE INDEX accounts_label_per_engine ON accounts (engine, label);
