-- How many times in a row the daemon restarted under this conversation's turn and a resume was
-- queued for it. A completed turn puts it back to 0; at the limit the turn stays interrupted.
ALTER TABLE chats ADD COLUMN auto_resumes INTEGER NOT NULL DEFAULT 0;
