-- Which conversation asked for a run, when a chat turn started one through `POST /runs`. Decision
-- of 2026-10-11: chat turns leave the main Runs list behind a filter, and the runs a chat LAUNCHES
-- stay in it, tagged with the chat they came from — they are the expensive work and must not
-- disappear with the turns that asked for them.
--
-- Distinct from `runs.chat_id`, which says a run IS a turn of that conversation. TEXT and no foreign
-- key, as `chat_relays.from_chat_id`: the conversation may be archived later and the tag still says
-- where the run came from. A label, not an authority: nothing decides anything on it.
ALTER TABLE runs ADD COLUMN launched_by_chat_id TEXT;
