-- The browser pillar: which projects have a profile, what each profile is allowed to load, and
-- which sessions have been opened in them.
--
-- Three tables, and the middle one is the security control. `browser_sites` is the allowlist spec
-- §5.2 says may only grow by a person logging in — everything else here exists to make that list
-- answerable ("who granted this, and when?") and enforceable ("which profile is this session in?").
--
-- Nothing here decides anything. The decision is `browser_policy::decide`, which is pure and takes
-- the site list as an argument; these tables are where that argument comes from.
--
-- 0079 and not 0073, with a deliberate gap. 0073 through 0078 are claimed by branches that are not
-- merged yet — `feat/equipas-de-agentes` holds 0073-0076 and `feat/assuntos` holds 0074-0078 — and
-- this file has already been renumbered twice by landing behind one of them. sqlx orders by version
-- and does not require them to be contiguous, so the gap costs nothing and closing it would put this
-- file back into contention with work that has not landed. Whoever lands next: take the number above
-- everything any branch claims, not the number above master.

-- One row per project that has ever had a browser profile.
--
-- The directory on disk is the real thing (`profiles/project-<id>`), and this row is what lets the
-- núcleo answer questions about it without listing another process's disk: which projects have a
-- profile, which was used least recently, which one the person is about to revoke. Spec §8's
-- max_profiles is enforced in the sidecar, where the directories are; this is what the UI reads.
CREATE TABLE IF NOT EXISTS browser_profiles (
  -- The project id, exactly as the rest of the núcleo spells it. It becomes a directory name in the
  -- sidecar, which lowercases and validates it there — see `browser_client::slug` and the sidecar's
  -- `profile.Ref`.
  project_id   TEXT NOT NULL PRIMARY KEY,
  created_at   TEXT NOT NULL,
  -- Touched on every open. What "least recently used" means when the ceiling is reached and someone
  -- has to be told which profile to forget.
  last_used_at TEXT NOT NULL
);

-- The allowlist. A host is here because a person logged into it (spec §5.2), and for no other
-- reason: there is no path from a YAML file, an agent, or a config default into this table.
--
-- The origin is the whole identity — scheme, host and port, exact, lowercase, punycode — because
-- `browser_policy` matches on the whole origin and a subdomain is a different principal inside a
-- profile holding live login cookies.
CREATE TABLE IF NOT EXISTS browser_sites (
  project_id  TEXT NOT NULL,
  -- `https://host[:port]`. Never a bare host, never a path: what a document is checked against is an
  -- origin, and storing anything else would need normalising at read time, in the hot path, by
  -- whoever remembered to.
  origin      TEXT NOT NULL,
  -- `destination` or `idp`.
  --
  -- The distinction is spec §5.3a's: a login crosses hosts, so the whole chain is granted as one set
  -- when the person hands the wheel back. The identity providers in that chain are marked, because
  -- they are shareable — the second project site that uses the same Google does not ask again — and
  -- because a person revoking access wants to see that `accounts.google.com` is here as a stepping
  -- stone rather than as somewhere they chose to browse.
  kind        TEXT NOT NULL CHECK (kind IN ('destination', 'idp')),
  granted_at  TEXT NOT NULL,
  -- The destination whose login brought this origin in, or NULL when this IS the destination.
  --
  -- Without it a revocation is guesswork: removing `jira.example.org` should offer to remove the IdP
  -- that came with it, and only if nothing else still leans on it.
  granted_for TEXT,
  PRIMARY KEY (project_id, origin)
);

-- Every session the núcleo has opened, live or finished.
--
-- # Why the id here is ours and not the sidecar's
--
-- The sidecar numbers its sessions from one, per process. It restarts — that is its whole restart
-- policy (spec §9.1: nothing durable is lost) — and the next session it opens is called `s1` again,
-- which is the name of a row that may still be sitting here unclosed. So the primary key is minted
-- on this side and the sidecar's id is a column, unique only for as long as that process lives.
-- The same lesson the sidecar's own pool learned about routing between two browsers.
CREATE TABLE IF NOT EXISTS browser_sessions (
  id             INTEGER PRIMARY KEY AUTOINCREMENT,
  -- What the sidecar calls it. NULL is impossible while the session is open and meaningless after it
  -- closed, so it is kept either way: a log that says which sidecar session a row was is the only
  -- way to read that process's log next to this table.
  sidecar_id     TEXT NOT NULL,
  -- The run this session belongs to, when there is one. NULL for an assistant turn that is not
  -- inside a run.
  run_id         INTEGER,
  -- The project this session was opened FOR — not necessarily the one whose profile it runs in.
  --
  -- The two are different questions and an earlier version of this column answered only the second,
  -- leaving NULL for every throwaway. That lost the fact that the assistant was working on a project
  -- when it browsed, and spec §4.5 needs exactly that fact: a wheel request from a throwaway is a
  -- request to establish a session IN THE PROJECT, so the handover has to know which one. Which
  -- profile a session actually ran in is `profile_kind` + `profile_id`, one line below.
  project_id     TEXT,
  profile_kind   TEXT NOT NULL CHECK (profile_kind IN ('project', 'ephemeral')),
  -- What actually went on the wire as the profile id: the project id, or the run id for a throwaway.
  -- Stored rather than derived, because deriving it later means re-running a decision with today's
  -- site list instead of the one that was in force.
  profile_id     TEXT NOT NULL,
  requested_url  TEXT NOT NULL,
  final_url      TEXT NOT NULL,
  -- Which `browser_policy` rule chose the profile: `project-site`, `off-list-ephemeral`,
  -- `redirected-out-ephemeral`. The rule travels with the verdict everywhere else in this repo for
  -- the same reason — "ephemeral" alone cannot be audited, and the interesting question is always
  -- which condition failed.
  rule           TEXT NOT NULL,
  -- Who has the wheel, as spec §4.4's state machine spells it.
  --
  -- `wheel-requested` is a state and not a flag on `agent`, because rule 1 turns on it: from the
  -- REQUEST onward — not from the window opening — the agent's actions are refused. The two moments
  -- are separated by a process swap (§4.2), which is not atomic, and an act landing in between would
  -- touch a page the person is about to inherit.
  --
  -- `delivery-failed` is §4.4a: the person accepted and the headful browser would not start. It does
  -- NOT go back to `agent` — the agent does not recover the wheel because of a failure of ours — and
  -- it is distinguishable from `human` so the UI can offer a retry rather than a window.
  mode           TEXT NOT NULL
                 CHECK (mode IN ('agent', 'wheel-requested', 'human', 'delivery-failed')),
  -- The proposal that asked for the wheel (spec §4.4 rule 3). NULL until the agent asks.
  proposal_id    INTEGER,
  -- The navigation the person's window recorded, as a JSON array, written when the wheel comes back.
  --
  -- Stored rather than passed straight to `grant` because the decision needs two steps: the window
  -- closes, the person is shown where they went, and only then do they keep the set or none of it
  -- (spec §5.3a). Keeping it here is also what makes the second step unable to name a host — it
  -- answers yes or no to what is written in this column, and the column was written by a browser
  -- under a person's own hands.
  chain          TEXT,
  -- When the person answered that question. Set on either answer, so a chain cannot be granted twice
  -- nor days later: the permission belongs to the moment of the login, which is the whole of §5.2.
  chain_decided_at TEXT,
  -- The consequence the fence named when it stopped the navigation this session was opened for, or
  -- NULL when nothing was stopped. A refused session still exists and is still addressable; it is
  -- simply empty (spec §6.2).
  refusal        TEXT,
  opened_at      TEXT NOT NULL,
  closed_at      TEXT,
  -- Why it ended: `closed` when someone closed it, `sidecar-restarted` when the process that held it
  -- went away. Two different things to a person reading a run back, and indistinguishable without
  -- this column.
  closed_reason  TEXT
);

-- Finding the open sessions is the only hot query here: it runs when the sidecar restarts and every
-- live row has to be retired, and when the UI lists what is open. Partial, because a finished
-- session is never the answer to either question and would otherwise make the index grow forever.
CREATE INDEX browser_sessions_open ON browser_sessions (opened_at) WHERE closed_at IS NULL;

-- Reading a project's list happens on every open, before anything else can be decided.
CREATE INDEX browser_sites_by_project ON browser_sites (project_id, origin);
