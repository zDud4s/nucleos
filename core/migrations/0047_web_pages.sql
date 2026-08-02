-- The web pillar's cache and index (spec §8).
--
-- The cache is what makes the pillar cheap: a page read once is not fetched again, and the archive
-- it accumulates is what the shell's Web tab shows on day one. It also changes what "search" means
-- — first what has already been read, then the internet.
--
-- FTS5 is compiled into the SQLite this crate links (verified: `pragma_compile_options` reports
-- ENABLE_FTS5), so this costs no crate and no embedding model, exactly like 0035 did for run_events.

CREATE TABLE web_pages (
    id             INTEGER PRIMARY KEY AUTOINCREMENT,
    -- What the caller asked for, and where the bytes actually came from. BOTH, because the trust
    -- decision (core/src/trust.rs) is made over the pair: an allowlisted host that redirects out of
    -- the allowlist must lose trust, and an unknown host that redirects into it must not gain any.
    -- Storing only one of them would make a stored row unable to justify its own verdict.
    requested_url  TEXT NOT NULL,
    final_url      TEXT NOT NULL,
    host           TEXT NOT NULL,
    title          TEXT,
    byline         TEXT,
    -- Markdown, never HTML. The núcleo has never held the markup: the sidecar extracts, and what
    -- crosses the boundary is text that carries no capability to execute anything.
    content_md     TEXT NOT NULL,
    -- 'article' or 'fallback' — whether this is a page's prose or its accessibility tree. A reader,
    -- person or model, deserves to know which of the two it is looking at.
    extract_status TEXT NOT NULL,
    -- 'raw' or 'quarantined': the trust this page was fetched UNDER, plus the rule that decided it.
    --
    -- This column is the fix for spec §10.4. A page read once by the owner from an allowlisted host
    -- and served later from cache to a cron run would be a trust escalation through time. The
    -- decision is therefore remade on every read and this column is what the remake is compared
    -- against — the cache stores bytes, never permissions.
    trust_at_fetch TEXT NOT NULL,
    trust_rule     TEXT NOT NULL,
    bytes          INTEGER NOT NULL,
    fetched_at     TEXT NOT NULL
);

-- One row per destination. A re-read replaces the row rather than accumulating history: this is a
-- cache, and a cache that grows a version per visit is a log nobody asked for.
CREATE UNIQUE INDEX web_pages_by_final_url ON web_pages (final_url);
CREATE INDEX web_pages_by_host ON web_pages (host);
-- Retention (spec §8) prunes by age, so the sweep must not be a table scan.
CREATE INDEX web_pages_by_fetched_at ON web_pages (fetched_at);

-- External-content index: the text stays in web_pages and the index holds only terms, so a long
-- page is never stored twice. web_pages.id is an INTEGER PRIMARY KEY, hence a rowid alias, so it
-- can serve as content_rowid.
CREATE VIRTUAL TABLE web_pages_fts USING fts5(
    title,
    content_md,
    content='web_pages',
    content_rowid='id',
    tokenize='unicode61'
);

-- Unlike run_events (0035), this table is genuinely mutated: a re-read updates a row and retention
-- deletes one. All three triggers are load-bearing here, not insurance.
CREATE TRIGGER web_pages_fts_insert AFTER INSERT ON web_pages BEGIN
    INSERT INTO web_pages_fts (rowid, title, content_md)
    VALUES (new.id, new.title, new.content_md);
END;

CREATE TRIGGER web_pages_fts_delete AFTER DELETE ON web_pages BEGIN
    INSERT INTO web_pages_fts (web_pages_fts, rowid, title, content_md)
    VALUES ('delete', old.id, old.title, old.content_md);
END;

CREATE TRIGGER web_pages_fts_update AFTER UPDATE ON web_pages BEGIN
    INSERT INTO web_pages_fts (web_pages_fts, rowid, title, content_md)
    VALUES ('delete', old.id, old.title, old.content_md);
    INSERT INTO web_pages_fts (rowid, title, content_md)
    VALUES (new.id, new.title, new.content_md);
END;
