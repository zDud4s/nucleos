-- One row per rule the owner has set. Family rules and kind exceptions share a table, told apart
-- by `scope`, because they are the same decision at two grains: somebody asking "why did this
-- line not arrive?" should find both halves of the answer in one place.
--
-- An absent row is not a rule that says "yes" — it is no rule at all, and the resolution treats
-- it as "passes". That is why a fresh install behaves exactly as the daemon did before this
-- table existed: it is empty.
CREATE TABLE notify_policy (
    id         INTEGER PRIMARY KEY,
    -- 'family' matches by prefix; 'kind' matches the literal and wins over any family.
    scope      TEXT    NOT NULL CHECK (scope IN ('family', 'kind')),
    selector   TEXT    NOT NULL,
    enabled    INTEGER NOT NULL CHECK (enabled IN (0, 1)),
    updated_at TEXT    NOT NULL
);

CREATE UNIQUE INDEX notify_policy_selector ON notify_policy (scope, selector);
