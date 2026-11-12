-- The landing of a spec's tree (owner decision F6, option (a), 2026-11-12): `settle_land` records
-- the merge sha the landing queue reported (NULL when it reported none) and when (RFC 3339) on
-- every spec whose `tree_id` is the landed job, and the spec file's `Estado:` line is then written
-- from these two columns, never edited by hand. Neither is a status: whether the spec is done is
-- still the audit's word in `status`.
ALTER TABLE specs ADD COLUMN landed_sha TEXT;
ALTER TABLE specs ADD COLUMN landed_at TEXT;
