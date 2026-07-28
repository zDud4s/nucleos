-- What a message carries besides its text, described but not stored.
--
-- The bytes are deliberately absent. A person deciding whether something is worth opening needs a
-- name, a type and a size; keeping the file itself would put a stranger's executable on disk before
-- anyone asked for it. The content is fetched from the mailbox on demand, addressed by `position`.
CREATE TABLE email_attachments (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    email_id   INTEGER NOT NULL REFERENCES emails(id) ON DELETE CASCADE,
    -- Which attachment, counting from zero in the order the message carries them. NOT a MIME part
    -- number: it is re-derived by walking the message the same way, which is what makes it stable
    -- enough to ask the mailbox for later.
    position   INTEGER NOT NULL,
    -- Sender-controlled and therefore never a path. Whatever writes this to a filesystem sanitises
    -- it there; storing it raw keeps the record faithful to what actually arrived.
    filename   TEXT,
    mime_type  TEXT,
    -- Decoded size, so it matches what the file would weigh on disk rather than its base64 form.
    size_bytes INTEGER NOT NULL,
    UNIQUE (email_id, position)
);

-- Every read is "the attachments of this message", in the order they arrived.
CREATE INDEX email_attachments_by_email ON email_attachments (email_id, position);
