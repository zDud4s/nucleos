-- Writing: which origins a profile may submit a form to, and what it has submitted.
--
-- The second half of the browser pillar's permission model. `browser_sites` says where a profile may
-- READ; the column added here says where it may also act as the person. They are two permissions and
-- not one, because they fail in different directions and are wanted in different combinations: read
-- the Jira and open no tickets, read the inbox and answer nothing.
--
-- Nothing here decides anything either. The decision is the sidecar's `fence.Decide`, which is pure
-- and takes both lists as arguments; these rows are where the second argument comes from.

-- A column and NOT a second table, and the reason is the revocation screen.
--
-- Spec §10 asks that what is given can be taken back in the same place. A separate `browser_writable`
-- table would be a second list of permissions in a second place, which is exactly how a person loses
-- track of what they granted — and this design's own worst case is a grant somebody forgot about. On
-- this column, withdrawing the write is one UPDATE next to the DELETE that withdraws the read, on the
-- row a person is already looking at.
--
-- 0 by default, so every origin ever granted before this migration — and every one granted by a code
-- path that has not learned about writing — is read-only. A default that had to be got right
-- somewhere else is a default that eventually is not.
ALTER TABLE browser_sites ADD COLUMN writable INTEGER NOT NULL DEFAULT 0;

-- Every form submission that actually left, one row each.
--
-- # Why this table exists at all
--
-- Because supervision has to be possible AFTERWARDS, given that it deliberately is not beforehand.
-- The whole point of a per-origin grant is that the agent then works alone inside it — asking a
-- person at each write would be mandatory supervision, and that is the thing this design rejected.
-- What is left is that a person can see what was done. Autonomy with no record is autonomy with no
-- supervision available at all, which is a different arrangement and not the one this project wants.
--
-- It is read next to the revocation, and that is what makes it supervision rather than decoration: on
-- the screen where the grant comes off, "this origin writes, and here is what it has written".
--
-- # What is not here, and will not be
--
-- The VALUES. A form carries a password, a session token, a private message. Recording what was
-- submitted would be the most useful audit trail there is, and it would turn this database into the
-- place where every credential an agent ever types comes to rest, permanently, for anyone who later
-- reads the file. The cost of not having it is real and is accepted with open eyes: knowing that
-- something was submitted to a reply form does not say what the reply said.
CREATE TABLE IF NOT EXISTS browser_writes (
  id           INTEGER PRIMARY KEY AUTOINCREMENT,
  -- The session it happened in, as the núcleo numbers them — `browser_sessions.id`, not the
  -- sidecar's, for the reason that table already spells out.
  session_id   INTEGER NOT NULL,
  -- The project whose profile was used, which is what the revocation screen is keyed on. Stored
  -- rather than joined through the session, because a session can outlive nothing and a project's
  -- write history has to survive the session rows being pruned.
  project_id   TEXT,
  -- Where it went, normalised the same way `browser_sites.origin` is: `https://host[:port]`, exact,
  -- lowercase, punycode. The same string, so the join a person makes by eye is the join the database
  -- would make.
  origin       TEXT NOT NULL,
  -- The form's action with the query REMOVED, and the method it was submitted with. An action can
  -- carry a token in its query string — the pattern is ordinary — so keeping it would put a
  -- credential in the one column nobody would think to look at.
  action       TEXT NOT NULL,
  method       TEXT NOT NULL,
  -- The NAMES of the fields, as a JSON array of strings, and how many there were.
  --
  -- Two columns rather than one, because they disagree on purpose: a long form is truncated to a
  -- readable number of names while the count stays true. A count that had quietly become "the first
  -- thirty-two" would be the same shape of confident wrongness the reading half of this pillar spent
  -- itself removing.
  fields       TEXT NOT NULL,
  field_count  INTEGER NOT NULL,
  -- The act that caused it: the ref from the snapshot and the verb. This is the fifth condition of
  -- the write rule written down rather than asserted — a row can be read back against the reading
  -- that produced it, which is the difference between "the agent submitted something" and "the agent
  -- submitted the thing it was looking at".
  ref          TEXT,
  verb         TEXT,
  written_at   TEXT NOT NULL
);

-- The only query this table has: what has this project written, most recent first. It runs on the
-- screen where a person decides whether to keep the grant, which is the one place these rows are for.
CREATE INDEX browser_writes_by_project ON browser_writes (project_id, written_at DESC);
