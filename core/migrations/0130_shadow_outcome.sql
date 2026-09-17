-- The half of the scoreboard that was always missing. `shadow.rs` has measured decisions
-- against decisions since the pillar existed — "did the human agree with the classifier?" —
-- and never against what a tool call actually DID. This is where a `PostToolUse`/
-- `PostToolUseFailure` report lands once one arrives.
--
-- Three columns, the same shape `decision`/`reason`/`created_at` already has for the decision
-- half:
--   `outcome`       — the raw `tool_response` JSON the CLI reported back. Measured against the
--                     installed CLI (2.1.260): `tool_response` is a `PostToolUse`-only field, and
--                     is the entire reason this migration exists.
--   `outcome_event` — which of the two hooks fired: `PostToolUse` for a success, or
--                     `PostToolUseFailure` for a failure (the CLI never fires the first for a
--                     call that failed). What tells a success and a failure apart in the ledger.
--   `outcome_at`    — when it landed, mirroring `reviewed_at` beside `human_verdict`.
--
-- All three nullable, and stay that way. A row is written the moment `PreToolUse` decides, long
-- before its outcome can exist, and `record_outcome` (`core/src/shadow.rs`) only ever UPDATEs a
-- row that is already there — it never INSERTs one. `record_decision` is called only for
-- `shadow` and `worktree` modes, so a `real`-mode run has no `shadow_decisions` row at all for
-- its outcomes to land on; for that run these columns are unrecorded by construction, not filled
-- with a sentinel.
ALTER TABLE shadow_decisions ADD COLUMN outcome TEXT;
ALTER TABLE shadow_decisions ADD COLUMN outcome_at TEXT;
ALTER TABLE shadow_decisions ADD COLUMN outcome_event TEXT;
