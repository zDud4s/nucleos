-- Automatic compaction (spec 2026-10-08 §4.4 / §8.1 step 3). `auto` separates a compaction the
-- daemon started from one the user typed, for measurement; `chats.auto_compact` switches the
-- trigger on per chat and, when on, suppresses the legacy root rotation. Both default to 0, so
-- every existing row and every manual `/compact` behaves exactly as before.
ALTER TABLE chat_compactions ADD COLUMN auto INTEGER NOT NULL DEFAULT 0 CHECK (auto IN (0, 1));
ALTER TABLE chats ADD COLUMN auto_compact INTEGER NOT NULL DEFAULT 0 CHECK (auto_compact IN (0, 1));
