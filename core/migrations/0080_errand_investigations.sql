-- An errand that works across windows instead of answering one message at a time.
--
-- Piece 3 of the design, and the question it had to answer first was "what is the gate of an
-- investigation?". A code job has one: the test suite runs, and its exit code decides. "Find me used
-- cars worth importing" has nothing of the kind — there is no command whose exit code means the
-- answer is good enough.
--
-- The answer taken here is that the gate is a SENTENCE the owner wrote, checked by something that
-- is not the worker. `done_when` is that sentence, in their words: "when I have five candidates
-- with price, year and mileage". It is not a filter and not a schema; it is what a person would say
-- if asked when to stop, and the only thing that reads it is the verifier.
--
-- WHY A COUNTER SITS BESIDE IT. `done_when` is a judgement, and judgement can be wrong in the
-- direction that costs money: a verifier that keeps saying "not yet" is an unattended loop. So the
-- criterion decides when to stop EARLY, and `windows_left` decides when to stop ANYWAY. Neither is
-- sufficient alone — a counter without a criterion always spends everything it is given, and a
-- criterion without a counter has no floor under a bad judgement.
--
-- DEFAULT ZERO, which is what keeps this dark. An errand that nobody has given windows to behaves
-- exactly as it did before this migration: it answers when spoken to and when a rule fires, and it
-- starts nothing on its own. Turning an errand into an investigation is one explicit act.
ALTER TABLE errands ADD COLUMN done_when TEXT;

-- How many more turns this errand may take on its own initiative. Spent one per window, and never
-- refilled by anything but a person: an investigation that could top up its own budget is a budget
-- in name only.
ALTER TABLE errands ADD COLUMN windows_left INTEGER NOT NULL DEFAULT 0;
