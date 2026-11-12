# Review Packet
<!-- READ-ONLY TEMPLATE. See packets/README.md for the contract. -->
Task ID: <!-- must match planning and execution packets -->
Plan summary: <!-- from the planning packet -->
Risk level: <!-- from the planning packet: low | elevated -->
Plan path: <!-- the persisted `<state_dir>/plans/<date>-<slug>.md` this review judges, or `none` when the plan was never written to a file -->
Plan sha256: <!-- sha256 of that file's RAW BYTES as pinned when the plan was approved, or `none`. Re-derived by `<bundle_dir>/scripts/review_gate.py`: if it no longer matches, the execution under review implemented a plan that has since been edited. -->
Files changed: <!-- from executor Handoff -->
Deviations from plan: <!-- from executor Handoff -->
## Hard gates
- [ ] Handoff section present and non-empty
- [ ] `Validation evidence` block exists for EVERY command in `Validation.Commands`
- [ ] Every evidence block shows `exit: 0` or accepted skip
- [ ] `Tests added` accounts for every planned test or concrete skip reason
- [ ] Plan/execute `Tests to add` values match
## Review Checklist
- [ ] Scope respected; no unrelated changes
- [ ] Acceptance criteria met
- [ ] Criteria have tests or exceptions
- [ ] No broken contracts or schemas
- [ ] Edge cases addressed
- [ ] Validation evidence matches acceptance criteria
- [ ] No regressions in adjacent code
- [ ] No security issues introduced
- [ ] Elevated-risk matches got scrutiny
Potential regressions:
Missing tests: <!-- unjustified coverage gaps -->
Simpler alternative: <!-- simpler approach, or `none` -->
Memory updates to apply: <!-- entries to apply, or `none` -->
Limitations: <!-- what this review could NOT inspect, one per line, or `none`. REQUIRED, and `none` is an answer while silence is not: a review that ran out of context, could not build, or never opened half the diff ends in the same word as a thorough one, and this is the only place that difference is visible. -->
Verdict: <!-- approve | request-changes | escalate -->
Recommendation: <!-- one sentence next action -->
