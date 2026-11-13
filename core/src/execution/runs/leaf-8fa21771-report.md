# Node Handoff

Node ID: 8fa21771-6ad6-4e56-9511-6a703b2c8f2b
Root ID: 9ae37a23-895b-4714-a400-0aaaf6b9964c
Status: done
Role: leaf
Outcome: done
Contract used: none

## Summary
Decisions affecting siblings: a resolution whose target ref is positively absent now settles the request and open twins as `target-gone` instead of counting an attempt. spawn.rs, stalled.rs, decisions.rs, wire.rs untouched. Cause (inferred): the 3-attempt cap is per request row, and the same merge asked again is a new row with fresh attempts. Gate fix: create.rs now calls `crate::execution::vcs::settle` (glob re-export) instead of the private `vcs::tickets::settle`. Nothing compiled by me.

## What was done
- create.rs: `settle_missing_target` runs first in the provisioning-failure arm; settles request and twins with `target-gone`; other failures keep the counted path.
- runs.rs: replaced the old retryable-missing-target test with `a_conflict_whose_target_branch_is_missing_fails_once_and_settles_its_twins`.
- Fixed E0603 in create.rs:1325.

## Decisions
- Reused existing reason `target-gone`.
- This report lives in core/src/execution/runs/ because the hook refuses `.nucleos-report.md` (owned by a sibling).

## Files touched
- core/src/execution/runs/create.rs
- core/src/execution/runs.rs

## Commands for the root
- cargo test -p nucleos-core --lib a_conflict_whose_target_branch_is_missing_fails_once_and_settles_its_twins
- cargo test -p nucleos-core --lib execution::runs::create

## Open items / next step
Delete core/src/execution/runs/leaf-8fa21771-report.md after reading; it is not part of the change.
