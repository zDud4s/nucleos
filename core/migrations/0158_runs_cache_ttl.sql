-- How long a chat turn's prompt cache lives: '5m', '1h', or NULL when the stream never said.
--
-- Read off the API's `cache_creation` split at turn end (`runner::cache_ttl_from_stream`), so the
-- Chats window can count down to the moment the next turn stops being cheap. NULL on every turn
-- from before this column and on any turn that wrote no cache.
--
-- Appended after the large columns `0154` moved last, which only costs a read of THIS column: the
-- one reader is the chat transcript, which reads `stdout` on the same row anyway.
ALTER TABLE runs ADD COLUMN cache_ttl TEXT;
