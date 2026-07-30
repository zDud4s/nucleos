-- Durable, named API keys with an explicit authorization level.
--
-- Like service_tokens, the secret is stored separately from the prefix carried by the bearer
-- credential so resolution can select one row and compare the secret in constant time.
CREATE TABLE api_tokens (
    name         TEXT PRIMARY KEY,
    token        TEXT NOT NULL,
    access_level TEXT NOT NULL CHECK (access_level IN ('read-only', 'run-creating', 'admin')),
    created_at   TEXT NOT NULL
);
