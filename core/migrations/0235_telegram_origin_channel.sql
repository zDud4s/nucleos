-- The Telegram origin becomes a channel origin: `telegram` -> `channel:telegram`, the spelling
-- `Origin::from_wire` resolves through the channel registry.
--
-- `chat_said_now.origin` is deliberately left alone: it only ever held what the old code wrote and
-- `Origin::from_wire("telegram")` still reads it. No CHECK constraints on any of the three columns.
--
-- Idempotent: a migrated row no longer equals `telegram`.
UPDATE runs        SET origin = 'channel:telegram' WHERE origin = 'telegram';
UPDATE chat_queue  SET origin = 'channel:telegram' WHERE origin = 'telegram';
UPDATE owner_notes SET origin = 'channel:telegram' WHERE origin = 'telegram';
