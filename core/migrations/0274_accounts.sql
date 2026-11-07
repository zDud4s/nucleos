-- The LLM account pool: one row per provider login the owner can route work to, per-conversation
-- account state, and the account a run actually ran on.

CREATE TABLE accounts (
    id                 TEXT PRIMARY KEY,               -- 'default:<engine>' or '<engine>:<8 hex>'
    engine             TEXT NOT NULL,                  -- EngineId: 'claude', 'codex', ...
    provider           TEXT NOT NULL,                  -- pool key of the inter layer
    label              TEXT NOT NULL,
    kind               TEXT NOT NULL DEFAULT 'subscription'
                       CHECK (kind IN ('subscription', 'api_key')),
    -- subscription: absolute config dir; NULL = the CLI's own default dir (the migrated login).
    -- api_key (future): the `secrets` entry name. Never the secret.
    credential_locator TEXT,
    state              TEXT NOT NULL DEFAULT 'active'
                       CHECK (state IN ('active', 'paused', 'needs_login')),
    created_at         TEXT NOT NULL,
    updated_at         TEXT NOT NULL
);
CREATE UNIQUE INDEX accounts_label_per_engine ON accounts (engine, label);

INSERT INTO accounts (id, engine, provider, label, kind, credential_locator, state, created_at, updated_at)
VALUES ('default:claude', 'claude', 'anthropic', 'default', 'subscription', NULL, 'active',
        strftime('%Y-%m-%dT%H:%M:%SZ','now'), strftime('%Y-%m-%dT%H:%M:%SZ','now')),
       ('default:codex',  'codex',  'openai',    'default', 'subscription', NULL, 'active',
        strftime('%Y-%m-%dT%H:%M:%SZ','now'), strftime('%Y-%m-%dT%H:%M:%SZ','now'));

-- Per-conversation account state. Its own table so chats.rs and its FromRow summaries stay untouched.
CREATE TABLE chat_accounts (
    chat_id             TEXT PRIMARY KEY,
    choice              TEXT NOT NULL DEFAULT 'auto',  -- AccountChoice text
    current_account_id  TEXT,
    previous_account_id TEXT,                          -- hysteresis (P6)
    switched_at         TEXT,
    last_cache_write_at TEXT,
    last_cache_tokens   INTEGER,
    updated_at          TEXT NOT NULL
);

ALTER TABLE runs ADD COLUMN account_id TEXT;      -- where the run actually ran
ALTER TABLE runs ADD COLUMN account_choice TEXT;  -- what was asked (AccountChoice text); NULL = settings default
CREATE INDEX runs_by_account ON runs (account_id, completed_at);
