-- Lets "what was this daemon doing in June" be answered by words that are not the exact wording.
-- feed::search matched `summary LIKE '%q%'` only — a substring on one column, so finding an entry
-- meant already knowing how it was phrased. This is the same shape 0035 gave run trajectories, for
-- the same reason, and it is deliberately the SECOND index of this kind rather than a new mechanism.
--
-- external-content table: the text stays in `feed` and the index holds only terms. `feed.id` is an
-- INTEGER PRIMARY KEY, hence a rowid alias, so it serves as content_rowid.
--
-- The delete trigger is not a precaution here, unlike 0035's. `feed::prune` removes entries past a
-- 90-day window on every retention pass, and the branch this migration lands on carries a commit
-- written because an FTS index outlived what it indexed. An index that keeps the terms of a pruned
-- entry is that bug again, one table over.
CREATE VIRTUAL TABLE IF NOT EXISTS feed_fts USING fts5(
    summary,
    content='feed',
    content_rowid='id',
    tokenize='unicode61'
);

-- Backfill every entry written before this migration.
INSERT INTO feed_fts (rowid, summary) SELECT id, summary FROM feed;

CREATE TRIGGER IF NOT EXISTS feed_fts_insert AFTER INSERT ON feed BEGIN
    INSERT INTO feed_fts (rowid, summary) VALUES (new.id, new.summary);
END;

CREATE TRIGGER IF NOT EXISTS feed_fts_delete AFTER DELETE ON feed BEGIN
    INSERT INTO feed_fts (feed_fts, rowid, summary) VALUES ('delete', old.id, old.summary);
END;

CREATE TRIGGER IF NOT EXISTS feed_fts_update AFTER UPDATE ON feed BEGIN
    INSERT INTO feed_fts (feed_fts, rowid, summary) VALUES ('delete', old.id, old.summary);
    INSERT INTO feed_fts (rowid, summary) VALUES (new.id, new.summary);
END;
