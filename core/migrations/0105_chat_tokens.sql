-- A key that belongs to a conversation's live CLI rather than to one of its turns.
--
-- `runs.token` is minted per run and, per `auth::resolve`, stops authenticating the moment that run
-- stops running. That rule is what makes it safe to hand a rooted turn -- one with Bash, Read and
-- Write -- a key at all, and it is also what stops a CLI process from outliving its turn: the
-- process is given its environment once, at spawn, so a process kept alive for a second turn would
-- present turn one's key forever. Wrong attribution, and a run key that never dies.
--
-- Measured, which is why this exists: a second turn fed down a live process's stdin reaches `init`
-- in 1.5s against 5.8s for a fresh spawn with `--resume`, runs its first shell command at 6.7s
-- against 26.4s, and finishes in 12.0s against 26-32s. None of the four `SessionStart` hooks fire
-- at all. The saving is entirely wall-clock -- turn 2's cost was within noise of a resumed spawn's,
-- and the prompt cache is at the API, not in the process.
--
-- So the key is scoped to the conversation and resolved, at the moment it is presented, to whatever
-- turn of that conversation is running. Between turns it names nothing and authenticates nothing,
-- which is the same rule `runs.token` follows, kept rather than weakened.
--
-- What it does cost: a turn could keep the secret and be attributed to a LATER turn of the same
-- conversation. That is a real loss and a deliberate one -- the blast radius is one conversation's
-- own gate calls, against a control token that approves proposals and disengages the kill switch.
--
-- One row per conversation, replaced on every spawn, so a process that died leaves a secret that no
-- longer opens anything the moment its successor starts.
CREATE TABLE chat_tokens (
    chat_id TEXT PRIMARY KEY,
    token   TEXT NOT NULL
);
