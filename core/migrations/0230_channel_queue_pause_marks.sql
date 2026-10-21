-- 0230 (A2): "queue paused" already announced for this conversation; cleared when the owner writes
-- again (replies::clear_queue_pause), so the next pause is announced again (Anexo B, B.9).
CREATE TABLE channel_queue_pause_marks (
    channel      TEXT NOT NULL,
    conversation TEXT NOT NULL,
    announced_at TEXT NOT NULL,
    PRIMARY KEY (channel, conversation)
) WITHOUT ROWID;
