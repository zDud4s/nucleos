-- Who a lesson is about: NULL = the code, the work, the project; 'owner' = the person.
-- Written only by the generalisation pass (spec 2026-10-22 5.3), and only with proof of origin.
-- No CHECK constraint (the rule of 0143_knowledge.sql): the vocabulary is a Rust constant.
ALTER TABLE knowledge ADD COLUMN about TEXT;
