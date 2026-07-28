-- A key per sidecar, so a bug in one is not a key to the daemon.
--
-- Both sidecars were launched with the control token. The email sidecar parses MIME written by
-- strangers and needs exactly two routes; the telegram sidecar takes text from a public chat and
-- needs thirteen. Holding the key to all forty means a parsing or path bug in either — and
-- `56f9b4a` was already one of those — is not a sidecar bug but a daemon compromise.
--
-- Rewritten at every daemon start rather than persisted as a durable secret: a sidecar is a child
-- process that dies with its parent, so a key that outlives the run of the daemon that minted it
-- has no one left to use it legitimately. The table exists so `auth::resolve` has somewhere to
-- look, not so the value survives.
CREATE TABLE service_tokens (
    name  TEXT PRIMARY KEY,
    token TEXT NOT NULL
);
