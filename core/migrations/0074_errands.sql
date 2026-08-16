-- Errands: standing work that is not a code project.
--
-- The daemon could already do autonomous work, and only inside a git repository -- `autopilot.rs`
-- demands a repo, `.ai/workflow` onboarding and a registered hook before a project may act, and
-- `job.rs` is plan -> implement -> gate -> review over a worktree. "Find me used cars to import" has
-- none of those and is an ordinary request. This table is where such a request gets to live longer
-- than the context window of the turn that received it.
--
-- `chat_key` is the whole design, and it is UNIQUE for that reason: an errand IS a Telegram topic
-- and a topic IS an errand. That single constraint removes a command to "enter" an errand, a notion
-- of which errand is current, and any heuristic guessing which errand a message belongs to --
-- Telegram already shows you where you are, and the topic id already answers the question. It also
-- means two errands can never share a topic, which is the state where nothing says which one
-- answers.
--
-- The absence of a row is the fact that decides, exactly as it already is for `chats` (0061): a
-- topic with no errand is loose conversation and behaves byte for byte the way Telegram behaves
-- today. Nothing here filters Telegram out; Telegram is simply absent until someone opens an errand.
CREATE TABLE errands (
  id          INTEGER PRIMARY KEY,
  -- What a person called it. Kept as typed, and never used as a path -- `folder` below is the
  -- núcleo's own derivation of it.
  name        TEXT NOT NULL,
  -- `<chat_id>:<thread_id>`, composed by the sidecar and opaque here. This module knows a chat key
  -- the way `runs.rs` knows a chat id: as a string it was handed, never as something to parse.
  chat_key    TEXT NOT NULL UNIQUE,
  -- Defaults to 'local' and NOT to 'cloud', which is the opposite of `chats.brain` (0061) and is
  -- deliberate. An errand is born from a Telegram message, and a Telegram message goes to the local
  -- model today -- a default that changes where the question goes is a default that changes the
  -- bill without anyone having asked for it.
  brain       TEXT NOT NULL DEFAULT 'local' CHECK (brain IN ('cloud','local')),
  -- A path RELATIVE to the files root (`files.rs`), assigned by the núcleo and never accepted from
  -- the model or from the user. UNIQUE because the folder is where two errands would otherwise
  -- write over each other; the id is carried inside the slug precisely so two errands named the
  -- same thing cannot collide.
  folder      TEXT NOT NULL UNIQUE,
  status      TEXT NOT NULL DEFAULT 'active' CHECK (status IN ('active','paused','done')),
  created_at  TEXT NOT NULL,
  -- Closing an errand sets this and removes nothing: the folder stays on disk and the row stays
  -- here, because what was found is worth keeping after the question stops being asked. The topic
  -- simply goes back to being loose conversation.
  closed_at   TEXT
);

-- What the model wrote, and what it had read when it wrote it.
--
-- A table rather than a `.meta` file beside each artifact, for the obvious reason: a file next to
-- the file would be written by the same process the mark exists to watch.
--
-- The mark is on the FILE and not on the turn because the turn ends and the file stays. A turn that
-- had read a stranger's words -- a web page, a marked file -- leaves those words behind in whatever
-- it wrote, and a later turn reading that file is reading the stranger again at one remove. That is
-- trust rising through the disk, which is what `trust.rs` already refuses to let happen over the
-- network.
--
-- `tainted` only ever goes up (`errands::record_artifact` upserts with `MAX`). A clean turn
-- rewriting a marked file does not launder it: it may have read the marked part and copied it
-- forward, and "the last writer was clean" is not the same claim as "this file is clean".
--
-- No column is added to `runs` here. Per-run contamination already exists as `runs.read_untrusted`
-- (0027), written by `runs::mark_untrusted_context`; two columns answering one fact is the drift
-- `core/AGENTS.md` exists to prevent.
CREATE TABLE errand_artifacts (
  errand_id   INTEGER NOT NULL REFERENCES errands(id) ON DELETE CASCADE,
  path        TEXT NOT NULL,
  tainted     INTEGER NOT NULL DEFAULT 0,
  -- The run that last wrote it. A logical reference and not a declared one, for the reason
  -- `council_runs.chairman_run_id` gives: `runs` rows are pruned on their own schedule, and an
  -- artifact whose author has been pruned is still an artifact worth knowing the mark of.
  written_by  INTEGER,
  created_at  TEXT NOT NULL,
  -- One row per file per errand: the mark is a property of the path, and a second row for the same
  -- path would be a second answer to a question that admits one.
  PRIMARY KEY (errand_id, path)
);
