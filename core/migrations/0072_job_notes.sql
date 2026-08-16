-- Somewhere to leave a message for a job that is already in flight.
--
-- A job is a sequence of runs over one worktree, and every node in it is born with a clean context
-- window. Nothing could speak to the next one. The steering channel is not it: `POST
-- /runs/{id}/message` writes into an in-memory map holding a LIVE process's stdin, so it needs the
-- target to be running and to have been spawned `steerable`, and `create_job_node_run` passes
-- `false` on purpose -- a node is one step of a plan, and text typed into it mid-flight would change
-- what that step does while the queue still claims the item it was given. So an owner watching a
-- night job had nowhere to put "when you get to item 3, update the docs too": the words had to be
-- said to a process that did not exist yet.
--
-- The mechanism is the one migration 0069 already built for `job_items.gate_output`, with a
-- different author. Text is stored on a row, `advance` reads it, and the next node's prompt carries
-- it. The gate wrote that one; a person writes this one.
--
-- A table rather than a column on `jobs`, and both halves of that matter. There can be several notes
-- for one job -- a night is long -- so a column would have the second overwrite the first. And each
-- is delivered separately: a column has nowhere to record that one of them has already been read out
-- while another still waits.
--
-- Nothing here changes the behaviour of a job that has no notes, which is every job that exists. An
-- empty table is an empty queue, and an empty queue leaves each prompt exactly as it is today.
CREATE TABLE job_notes (
    id                   INTEGER PRIMARY KEY,

    -- Which job the words are for. The whole job and not one item, because the owner is speaking to
    -- the night rather than to a queue position: "when you get to item 3" is a sentence about the
    -- work, and the node that reads it may not be the one working item 3. Tying a note to an item
    -- would make the owner guess an ordinal that the plan may yet renumber, and would silently drop
    -- the note if that item finished first.
    job_id               INTEGER NOT NULL REFERENCES jobs(id),

    -- What was said, verbatim. Stored as written and never as a rewritten instruction: the prompt is
    -- built from this by a pure function that quotes it, so what a node reads is what the owner
    -- typed. NOT NULL because a note with no words is not a note -- the queue this table is would
    -- have an entry in it that delivers nothing and can never be delivered again.
    body                 TEXT    NOT NULL,

    -- Who said it. NOT NULL, and it is not decoration: the whole difference between this and
    -- `job_items.gate_output` is that a person is speaking, and a node given anonymous text has been
    -- handed a second brief with no way to weigh it against the one it already has. It is stored
    -- rather than derived from the token that left the note because tokens are revoked and renamed,
    -- and the words outlive them.
    --
    -- This phase writes only the owner here. A run that could leave a note for another run is a
    -- separate change with a governance question attached, and this column is the place that question
    -- would be answered -- not a reason to consider it answered now.
    author               TEXT    NOT NULL,

    created_at           TEXT    NOT NULL,

    -- When the note was carried into a node's prompt. NULLABLE, and the nullability is the entire
    -- design rather than an omission: `delivered_at IS NULL` IS the queue. A note is pending because
    -- nothing has read it out yet, not because a status column says so, so there is no state machine
    -- to keep in step and no way for the two to disagree.
    --
    -- Written only AFTER the run that carries it exists. A node that failed to start -- the item was
    -- claimed by another pass, the project's slot was taken, the tree could not be provisioned --
    -- never read the words, and a note consumed by it would be lost silently, which is the one
    -- failure the owner cannot see and cannot repeat.
    delivered_at         TEXT,

    -- Which run was told. NULLABLE for the same reason and at the same moment as `delivered_at`:
    -- both are NULL while the note waits, and both are written together when it is delivered.
    --
    -- Kept rather than dropped once the note leaves the queue, because "it was delivered" and "it
    -- reached THAT node" are different answers to the question an owner asks afterwards -- whether
    -- the words arrived in time to change anything. The run holds the prompt they were appended to,
    -- so this is the join back to the evidence.
    delivered_to_run_id  INTEGER
);
