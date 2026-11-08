-- Offers on a task (spec 2026-11-06-tickets-globais). An offered ticket is still `open`: the offer
-- names the competent holder the daemon routed it to and is advisory, so anyone may still claim and
-- the first claim wins (the claim clears both columns). `offered_to` is the JSON of a `Holder`
-- (`{"kind","id","worktree"}`); `offered_at` is a `wave::stamp` text bound from Rust. No CHECK
-- constraints (the rule of 0143).
ALTER TABLE task_claims ADD COLUMN offered_to TEXT;
ALTER TABLE task_claims ADD COLUMN offered_at TEXT;
