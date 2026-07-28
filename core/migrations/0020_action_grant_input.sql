-- The grant must name the action it authorizes, not just the tool that performs it.
-- Keyed on (run_id, tool_name) alone, an approval of `git push origin main` authorized the
-- resume's FIRST Bash call whatever it turned out to be: for every shell action tool_name is the
-- constant "Bash", so the human approved one command and licensed any other.
--
-- Nullable because the column is new and pre-existing rows have no recorded input; a NULL grant
-- matches only a NULL input, so an old row authorizes nothing it cannot prove.
ALTER TABLE action_grants ADD COLUMN tool_input TEXT;
