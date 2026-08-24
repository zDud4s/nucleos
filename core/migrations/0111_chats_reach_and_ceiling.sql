-- What else a conversation may touch, what one of its turns may spend, and who answers when the
-- model it wants is unavailable.
--
-- Three columns because three flags existed on the CLI and no conversation could reach any of them.
-- `RunRequest` could not express them either — this is the same gap 0110 closed for `--model`, in
-- the three places it was left open.
--
-- ── extra_dirs ──────────────────────────────────────────────────────────────────────────────────
-- `--add-dir <directories...>`. A conversation has exactly one directory today (`cwd`, 0070), and
-- everything outside it is invisible to its tools. That is right as a default and wrong as a
-- ceiling: a person working across a repository and its sibling worktree, or a project and the
-- notes folder beside it, has to choose which half the model can read.
--
-- JSON and not one path, because the flag is variadic and the plural is the useful answer. Stored
-- as a TEXT array rather than a side table for the reason `handover` (0091) is a column: it is
-- read whole, written whole, and never queried across rows.
--
-- Absolute paths only, checked at the door in `http.rs` the way `cwd` already is. A relative one
-- would resolve against the DAEMON's working directory, which is not a place the caller knows or
-- meant — and this grants tool access, so a path nobody verified is a directory nobody chose.
ALTER TABLE chats ADD COLUMN extra_dirs TEXT;

-- ── turn_budget_usd ─────────────────────────────────────────────────────────────────────────────
-- `--max-budget-usd <amount>`, and the name says what it actually bounds.
--
-- The flag limits ONE invocation of the CLI, and this daemon spawns one per turn. So a value here
-- is a ceiling on a single answer, not on the conversation — ten turns at the ceiling cost ten
-- times it. Naming this `budget_usd` would have read as a total and been wrong every time somebody
-- relied on it, which is the worst kind of wrong: quiet, and about money.
--
-- A conversation-wide total is a different thing and the daemon could do it — `runs.cost_usd` is
-- already recorded per turn — but it would be the daemon refusing rather than the CLI stopping, and
-- the two fail at different moments. Not built here; this column does not pretend to be it.
--
-- NULL is no ceiling, which is what every conversation has had since the table was made.
ALTER TABLE chats ADD COLUMN turn_budget_usd REAL;

-- ── fallback_model ──────────────────────────────────────────────────────────────────────────────
-- `--fallback-model`, a comma-separated list tried in order when the chosen model is overloaded or
-- unavailable. The CLI re-tries the primary at the start of each user turn, so this degrades for as
-- long as it has to and no longer.
--
-- Its own column and not a second value inside `model` (0110): they answer different questions —
-- one is who should answer, the other is who may answer INSTEAD — and packing them into one string
-- would make "no fallback" and "no model" the same absence.
--
-- Stored as the CLI takes it, comma-separated. Every name in it is checked against the same
-- catalogue `model` is, so a fallback cannot name something the daemon would not run.
ALTER TABLE chats ADD COLUMN fallback_model TEXT;
