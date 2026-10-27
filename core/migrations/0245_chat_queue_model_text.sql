-- A queued message that was a slash command carries two texts: what the person typed (the
-- transcript keeps it) and what the model reads (the expansion). `model_text` holds the second;
-- NULL means the message was sent as typed, which is every row from before this column.
-- No CHECK constraints, as in 0180, 0182, 0184 and 0186.
ALTER TABLE chat_queue ADD COLUMN model_text TEXT;
