Review SQLite schema migrations (core/migrations/NNNN_name.sql) and the SQL/Rust code that uses them.

## Numbering and immutability
- Gap, duplicate or out-of-order NNNN: Grep the migrations dir for the highest number; the new file must be highest+1 and unique. Two branches adding the same number is a merge hazard.
- Diff modifies an existing migration file instead of adding a new one: shipped migrations are immutable (already-applied dbs will not rerun it). Report as [high].

## ALTER and table rebuild
- DROP COLUMN / ALTER COLUMN / adding a constraint to an existing column on SQLite: unsupported or version-dependent; needs a rebuild.
- Rebuild must: PRAGMA foreign_keys=OFF, create the new table, INSERT...SELECT every column (compare against the old column list; a missed column is silent data loss), drop old, rename new, recreate ALL indexes, triggers and views that referenced the old table, run PRAGMA foreign_key_check before re-enabling.
- PRAGMA foreign_keys inside a transaction is a no-op: if the migration runner wraps the file in a transaction, the OFF does nothing and DROP TABLE may cascade-delete children. Check how the runner applies files.
- Rename order: rename-old-then-create-new can rewrite FK references in other tables to the old name; check dependents.
- Rebuild that drops an index/trigger: Grep the old definitions in earlier migrations and confirm each is recreated.

## Constraints and backfill
- New CHECK / NOT NULL / UNIQUE / FK on a table with existing rows: do existing rows satisfy it? NOT NULL without DEFAULT fails on ALTER ADD.
- Backfill: NULL inputs, values with no mapping (CASE without ELSE yields NULL), empty strings, duplicates under a new UNIQUE.
- json_valid(col) CHECKs: NULL passes json_valid as NULL; confirm intent. Existing malformed JSON breaks a rebuild.
- FK ON DELETE: missing clause means NO ACTION; CASCADE on a table holding audit/history data is a loss risk.
- Irreversible step (drop column/table, lossy type change) with no mention of backup or rollback.

## Rust coupling
- Column added/removed/renamed: Grep the table name in .rs. Check each struct, every SELECT * and explicit column list, INSERT column lists and positional row.get(n) indexes.
- New code reading old rows: nullable or defaulted for pre-migration rows? Old binary reading a migrated db (rollback): does it break on the new column/constraint?

## Query smells
- SQL built by string concatenation/format! with values (injection); use bound params.
- UPDATE/DELETE with no WHERE, or WHERE on the wrong key.
- LIMIT without ORDER BY (nondeterministic); `= NULL` instead of IS NULL; `!=` silently excluding NULLs.
- Transaction held across an .await, or no rollback on error path; multi-statement change outside a transaction.
- Timestamp format mixed in one column (unix int vs RFC3339 vs datetime('now')): compare with existing writers.
