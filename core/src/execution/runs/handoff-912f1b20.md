# Node Handoff

Node ID: 912f1b20-0055-4559-8847-18613dbba80a
Root ID: f77b2fb4-7d5d-4df7-8582-967b07b38165
Status: done
Role: leaf
Outcome: done
Contract used: none

## Summary
Decisions affecting siblings: none. `append_run_events` in core/src/execution/runs/spawn.rs writes a run's events in transactions of at most 500 rows via `write_pool::for_writes`; NOT EXISTS dedupe, seq and order unchanged. Three tests added (`run_event_batch_tests`): rows and order, re-append dedupe, 25 rows at batch 10 gives 3 transactions. fmt and check_invariants ran via heavy.py with no output; no cargo test/check run by me. This file stands in for `.nucleos-report.md`, whose path the hook refuses (owned by sibling sqlite-synchronous-normal).

## What was done
- Split into `append_run_events` (wrapper, batch 500), `append_run_events_in_batches` (returns committed transaction count) and `insert_run_event`.
- Added the three tests.

## Decisions
- Used for_writes; left storage.rs, pool size, FTS, hooks.rs untouched.

## Files touched
- core/src/execution/runs/spawn.rs
- core/src/execution/runs/handoff-912f1b20.md (stray stand-in; delete)

## Commands for the root
- cargo test -p nucleos-core run_event_batch_tests
- Pending deletions: core/src/execution/runs/handoff-912f1b20.md

## Open items / next step
Gate: run the test command above, then land spawn.rs. Path ownership overlap on `.nucleos-report.md` with sibling f9833faf needs fixing.
