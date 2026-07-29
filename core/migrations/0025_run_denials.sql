-- How many denied actions a run has attempted.
--
-- A `deny` was the end of the exchange: the run got its answer and carried on, free to try the
-- next spelling. The classifier is lexical, so its strength is exactly the number of attempts it
-- grants, and that number was unbounded and unwatched — which made the harsher of the two
-- verdicts the cheaper one to hit, since `pending_approval` at least stops the run and asks a
-- human.
ALTER TABLE runs ADD COLUMN denials INTEGER NOT NULL DEFAULT 0;
