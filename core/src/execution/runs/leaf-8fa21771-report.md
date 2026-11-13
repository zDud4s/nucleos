# Node Handoff

Node ID: 8fa21771-6ad6-4e56-9511-6a703b2c8f2b
Root ID: 9ae37a23-895b-4714-a400-0aaaf6b9964c
Role: leaf
Outcome: done
Status: done
Contract used: none

## Summary
Decisions affecting siblings: a resolution whose TARGET ref is positively absent (git rev-parse --verify exits 1) now settles the request and its open twins as `target-gone` instead of counting an attempt; spawn.rs, stalled.rs, decisions.rs, wire.rs untouched. Cause of 39 lines for `aut/integ` (inferred from code; the DB was not readable): the cap of 3 attempts is per request row, and the same merge asked again is a new row with fresh attempts (13 x 3). Nothing compiled or tested.

## What was done
- core/src/execution/runs/create.rs: in the provisioning-failure arm, `settle_missing_target` runs first. It checks the target with `git rev-parse --verify --quiet <target>^{commit}`, exit 1 only. It then settles the request and every open escalated twin (same project_id and args) via `vcs::settle(.., "target-gone")`. Any other failure keeps the old count-an-attempt path.
- core/src/execution/runs.rs: the old test (missing target stays retryable, attempts == 1) contradicted the new behaviour; replaced by `a_conflict_whose_target_branch_is_missing_fails_once_and_settles_its_twins` (request and twin both settled `target-gone`, one `worktree_provision_failed` feed line, no resolution run).
- `cargo fmt -p nucleos-core` and check_invariants.py ran via heavy.py with no complaints.

## Decisions
- Reused the resolver's existing reason `target-gone` (resolver.rs:658), not a new word.
- Detection lives in create.rs using the pub `worktree::get()`-style helper `worktree::git()`; worktree.rs is not owned.
- A git that cannot be asked (spawn error, other exit code) stays transient and counted.
- This report is at core/src/execution/runs/leaf-8fa21771-report.md, not `.nucleos-report.md`: that path is owned by sibling feed-distinct-subjects and the hook refused it. It is untracked; delete it after reading.

## Files touched
- core/src/execution/runs/create.rs
- core/src/execution/runs.rs
- core/src/execution/runs/leaf-8fa21771-report.md (untracked, this report)

## Commands for the root
cargo test -p nucleos-core --lib a_conflict_whose_target_branch_is_missing_fails_once_and_settles_its_twins
cargo test -p nucleos-core --lib execution::runs::create
cargo test -p nucleos-core --lib resolver

## Open items
- Nothing compiled: check `settle_missing_target` (let-chain `if let ... && ...`, sqlx query_scalar types) and the new test.
- Root cause not confirmed against the live DB (hook blocked it). Worth a look at the vcs_requests rows for `aut/integ`.
- The create.rs comment at ~791 still describes the cap, followed by the new case.
