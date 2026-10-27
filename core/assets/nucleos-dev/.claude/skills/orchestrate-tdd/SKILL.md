---
name: orchestrate-tdd
description: Use when a development task has testable behavior and should be built test-first with independently verified failing-then-passing gates. Default route for medium/large TDD-able code tasks; non-testable tasks belong to the orchestrate skill.
---

<!-- Cockpit: this skill runs in the controller's own session, under the user's permissions
     and with the user watching, so it carries no tool wall — and its frontmatter must not
     pretend otherwise. A skill's `tools:` field is inert (`tools:` belongs to agent files),
     and the walls in this pipeline are on the dispatched phases: `.claude/agents/wf-*.md`. -->

You are the TDD orchestrator. You run the full workflow pipeline end-to-end under a strict RED -> GREEN -> REFACTOR discipline, from a single task description.

**Read `.nucleos/workflow/workflow.md` and `.nucleos/workflow/dispatch.md` once before starting.** `workflow.md` holds Rule 3 (verification) and Rule 6 (review) and introduces `<state_dir>` and `<bundle_dir>`; this skill names those rules and never restates them. `dispatch.md` defines the dispatch contract, routing logic (`inline | agent | dispatcher`), prompt-passing convention (temp file -> stdin), resume rule, and dispatch-time error table. Everything below assumes those rules. Do not duplicate them.

**`<test_one>` is the per-test command from `commands.test_one` in `<state_dir>/project.yaml`**, resolved per stack — `cargo test -p <crate> <name>`, `go test -race -run <name> ./...`, `npm test -- -t <name>`. In this workflow it is a command the plan names and the controller's RED proof runs; executors never run it (see `## Batched test runs`).

**Packets are read-only templates** (schema layer — identical to `orchestrate`; phases READ and EMIT filled copies, never Edit/Write `.nucleos/workflow/packets/`). This skill reuses `plan.md` (planning) and dispatches `execute.md` three times — RED, GREEN, REFACTOR — with different `Objective` + `Boundaries`.

## What makes this different from `orchestrate`

`orchestrate` plans tests and lets the executor write tests and implementation together. This skill enforces test-first: failing tests must exist and be *independently proven to fail by you* before any implementation is dispatched; the single final verification then proves them passing. The executor writes all code; you never edit code, but you DO run the RED proof and read-only file/`git` checks. If the task has no testable behavior, this skill does not apply (see Phase 1).

## Discovery path convention

"Discovery path" — the per-runtime path to a `<name>` skill body — is defined in `.nucleos/workflow/dispatch.md`, which you read at start.

## Entry point

The user invokes you with `Use the orchestrate-tdd skill. Task: [description]`.

## Output format

Returns a Markdown report containing:
- Task summary and outcome
- Files changed (paths and counts), tests vs implementation separated
- RED evidence (the RED proof output showing the tests failed first)
- GREEN evidence (the final verification showing every new test passing)
- Final verification evidence (the single final run, every size)
- Refactor summary (or "skipped")
- Review verdict (if review ran)
- Unresolved risks (if any)
- Memory updates applied
- Phase execution log (plan, red, green, refactor, review, wrap-up with tool/model/source columns)

## Pre-flight checks

As in `orchestrate` §Pre-flight checks (state files, dispatch.md, the `executor` skill, the bridge skill named after `execute.tool`, the `wf-executor` cockpit file), PLUS: you must be able to run the RED proof yourself — you are the authority on RED. If you cannot, you stop at the RED gate rather than trust self-reported evidence (see Error table).

## Phase 1 - Triage + Plan (TDD-aware)

Build the planner prompt as in `orchestrate` §Phase 1 (planner skill, task, `project.yaml`/`decisions.md` facts, `memory.md` as an index, `.nucleos/workflow/packets/plan.md`). Add three TDD requirements to it:

1. State a `TDD-able: yes|no` verdict at the top. Non-TDD-able = no observable behavior expressible as an automated test (docs-only, config-only, pure refactor with no behavior change, trivial <10-line edits).
2. If `TDD-able: no`: emit a one-line reason and stop the plan.
3. If `TDD-able: yes`: produce the normal plan with (a) `Tests to add` as an ordered list of test cases, one test each; and (b) `Validation.Commands` set to the `<test_one> <test_refs>` form scoped to the new tests only — never a full suite, never the final verification (that is the controller's one run).

Dispatch through the configured tool/model. The planner output must state `Size`, `Risk level`, and `TDD-able`. Size values and the review gate are unchanged from `orchestrate`: run Phase 5 (review) if `Risk level: elevated` OR Size is `medium`/`large`.

**Applicability gate.** If `TDD-able: no`, STOP the pipeline and tell the user: "This task is not TDD-able (<reason>). Use the `orchestrate` skill instead." Do not fall back to a non-TDD flow.

If planner output is missing `Size`, missing `Risk level`, missing `TDD-able`, or (when `TDD-able: yes`) missing a test command or test-case list, STOP and report invalid planner output.

Plan accepted and persisted under `<state_dir>/plans/` -> pin it now, per dispatch.md's "Plan pin": record its path and `sha256sum` for Phase 5. Not persisted -> both are `none`.

### Auto-select handoff (when `auto_select.enabled: true`)

As in `orchestrate` §Auto-select handoff. The RED, GREEN, and REFACTOR dispatches all use the `execute` role's resolved `(tool, model, reasoning_effort)`. Review and rescue use their own resolved values.

### Speed

Use the speed decision `orchestrate` resolved (see its `## Speed`); when this skill was invoked directly, resolve it the same way, with `--tdd-able` from the planner's `TDD-able:` line. Its `reasoning_effort` replaces the `execute` role's for the RED, GREEN and REFACTOR dispatches, and `review_required: true` runs Phase 5 whatever Rule 6 alone would say. `workers > 1` asks for a wave (`## Parallel GREEN wave` below). Whenever there is none, GREEN runs sequentially and the payload of its `metric` record merges in the `metrics_args` dict the wave engine answered — `degraded: wave_disabled` while `parallel_execute.enabled` is `false`. The speed never changes how tests run: `## Batched test runs` is the only mode.

## Batched test runs

This is the single home of the batched flow (Rule 3 in workflow.md points here). A test pass costs minutes, a dispatch costs seconds of reading, so a task runs the fewest test passes that still prove RED and decide ship: when nothing is red, exactly **two** — one RED proof, one final verification.

1. **Every execute packet carries `Test runs: batched`** on its own line under `## Validation`, after the `Commands:` list (which still names the `<test_one> <test_refs>` it covers — that list is what you batch). The executor runs no test, build or check command at all; each command's evidence block is `$ <command>` followed by `deferred: batched`, which `evidence_gate.py` accepts only in a packet carrying that line. Instead of running, the executor re-reads its own diff against the packet — every changed hunk, every name it calls, every import — and states in `Deviations from plan` anything it could not confirm by reading.
2. **RED: dispatch every RED packet first, then prove them all in ONE pass, run by you only.** Collect every packet's test refs and run them together, one invocation per `<test_one>` form (`<state_dir>/project.yaml`), its `<name>` replaced by all the names: `cargo test -p <crate> --lib -- <name1> <name2> ...` (keep the form's `--lib`/`--bin`; libtest takes several filters after `--`), `go test -race -run '<name1>|<name2>' ./...`, `npm test -- -t '<name1>|<name2>'`. The right-reason check is per test: each new test fails on an assertion, OR its test binary fails to compile on a symbol the plan is about to add (in Rust one missing symbol stops the whole crate's test binary — that is a valid RED for every new test in it, provided every compile error names an absent-implementation symbol and none is a typo in a test). Count the names: a filter that matches nothing runs `0 tests` and exits 0 without a word, so the output must show every name failing, or a compile error, never fewer tests run than names given. A wrong-reason or passing test sends back only its own packet; then re-run only that packet's refs. Snapshot the test files as usual (Phase 2).
   <!-- F5-OPEN: how the controller runs the RED proof now that only the daemon's verify runs tests — a named-test scope on mcp__nucleos__verify is not defined in the F5 brief. -->
3. **GREEN: dispatch every GREEN packet (sequentially, or as the wave), with no test run in between.** Only the integrity check against the RED snapshot, once all have returned.
4. **REFACTOR**, when it runs, is batched too: no test run of its own.
5. **The final verification is the GREEN proof.** Run it once (`## Final verification`) and confirm every new test actually ran in it: each name appears as passed in the verification result. A reporter that prints only totals (a count of passed tests) proves a name only when its group ran with no name filter, there is no failure line, and the passed count includes the file's new tests. A test the verification did not reach (it stopped at an earlier red, or its group was not selected) is not proven. Green and all reached -> ship. Red -> resume, in one round, only the packets that own a failing test or file (each still batched, each told the failing tail and asked for its verified root cause), then the re-run Rule 3 prescribes. Third red on the same gate -> rescue.

| Rationalization | Reality |
|---|---|
| "a quick build or check in the executor is cheap" | Batched means none: the executor reads its diff instead. |
| "I'll run the scoped GREEN tests before the final verification, to be safe" | That is the pass batching removes; the verification runs them, and step 5 checks it did. |
| "RED is batched, so a compile error proves every test failed for the right reason" | Only when each error names a symbol the plan adds. A typo in one test is that packet's wrong-reason RED. |

## Large TDD tasks: root node

When Size is `large`, or the plan would not fit one controller within `nodes.orchestrator_budget`, follow `orchestrate` §"Large tasks: run a node tree": open a root node (`mcp__nucleos__node_open`, no `parent_id`, with `task_id`) and decompose into child nodes per the `orchestrate-node` skill. Medium tasks run the phases below unchanged. For a node tree:

- Each node's leaves still author the tests and the code through `wf-executor` packets (RED and GREEN objectives and boundaries as below); a node dispatches only its direct children and returns a summary.
- Nodes never run tests. Every packet stays `Test runs: batched`, and a node hands back the exact test commands and refs it would have run.
- You, the root, collect every packet's refs and prove RED once for all packets together (`## Batched test runs` step 2), run the integrity check against the RED snapshot, and run the single final verification (Rule 3). No node, and no subtree, decides ship.
- `nodes.subtree_verify` (default `false`) may add an intermediate `mcp__nucleos__verify` on a finished subtree. It is a gate for catching a red early, never the ship decision, and the final verification still runs once at the end.

## Phase 2 - RED (author failing tests)

Resolve the executor `(tool, model, reasoning_effort)` from `auto_overrides["execute"]` or `execute.*` in `<state_dir>/models.yaml`. Dispatch a filled `execute.md` packet scoped to tests only:

- Objective: "Author the failing tests listed. Do NOT write or modify implementation/source code."
- Boundaries: Allowed = the test files; Do-not-touch = source/implementation files.
- Tests / To add: the planner's ordered test-case list.
- Validation: the `<test_one> <test_refs>` it covers, `Test runs: batched`.

Dispatch synchronously through the resolved tool. The phase skill body is `executor` — RED, GREEN and REFACTOR are three `execute` dispatches and all three go through the `wf-executor` cockpit (dispatch.md, "The phase cockpit"). Tool-specific invocation lives in the bridge skill named after `<execute.tool>`, which you read yourself and never inline. Apply the standard exit/escalation handling.

### Gate: prove RED (you run it, once, for every packet together)

After every RED dispatch returns exit 0 with a complete Handoff, prove RED as `## Batched test runs` step 2 describes — the new tests only; proving RED never needs more. Then:

- The new tests MUST fail. If they PASS, the tests are bogus (assert nothing, or the behavior already exists). Reject and resume the executor: "your tests pass without implementation — they do not capture the new behavior." Re-run the proof for that packet.
- **Right-reason check.** Inspect the output tail. Accept either (a) a behavioral assertion failure, or (b) an import/collection/compile error caused by the absent implementation-under-test (e.g. `ModuleNotFoundError`/`ImportError`/`AttributeError`, or an unresolved symbol, for the symbol you are about to implement) — both are legitimate "implementation missing" REDs. Reject only a failure unrelated to the missing implementation: a syntax error in the test, a broken conftest/fixture, or a wrong import path to an already-existing module — resume the executor to fix the test, then re-run.
- Record the RED evidence: `$ <command>`, `exit: <non-zero>`, `tail: <last lines>`. Mandatory; the reviewer checks it.
- **Snapshot** the authored test files (read their content / hash) so you can detect tampering during GREEN.

| Rationalization | Reality |
|---|---|
| "tests error rather than fail — close enough" | Only an import/collection/compile error for the absent symbol is a valid RED; a test syntax error, broken fixture, or wrong import to existing code is not — resume and fix the test. |
| "snapshot hashing is overkill for a small diff" | Snapshot every authored test file regardless of diff size; it is the only GREEN tamper check. |

Do NOT dispatch GREEN until RED is proven.

## Phase 3 - GREEN (implement until all pass)

Dispatch a filled `execute.md` packet scoped to implementation only:

- Objective: "Implement until ALL tests pass. Do NOT modify the tests."
- Boundaries: Allowed = source/implementation files; Do-not-touch = the test files authored in RED.
- Validation: the frozen `<test_one> <test_refs>` set only, `Test runs: batched` (no selector, no full suite).

### GREEN: no controller test run

After the GREEN dispatch returns exit 0 with a complete Handoff (the evidence gate accepts its deferred evidence), do NOT run any test yourself; the single final verification is the GREEN proof. Run only the cheap non-test check: the test-file snapshot is unchanged.

- **Integrity check.** Compare the test files against the RED snapshot. If any changed, the executor edited a frozen test — reject and resume ("tests are frozen during GREEN; revert your test changes and make the implementation pass instead"). Check again from the resumed Handoff.
- A red final verification resumes the packets that own the failure with its tail; each resume prompt must require the executor to state the one-line root cause it verified before changing anything (a fix without a stated root cause is rejected). On the 3rd failed iteration on the same gate, stop resuming and dispatch rescue with an explicit mandate to question the approach, then report and stop.
- Record the GREEN evidence from the final verification (command, result, the names proven).

## Parallel GREEN wave (optional, off by default)

This is the single home of the wave mechanics. When the speed decision's `workers` is above 1 and the plan carries a `## Parallel execute manifest` (planner skill), GREEN may fan out to one executor per unit, each in its own worktree. Every decision of the wave — whether there is one, how many units run at once, when to stop opening units, what lands — is taken by `<bundle_dir>/scripts/wave_engine.py`. You only sequence its verbs and dispatch the workers it opens. The wave's slots are asked for, kept and given back by `<bundle_dir>/scripts/wave_lease.py`, which the engine calls for you: never call the lease, or the daemon, yourself during a wave.

The workers run in `dispatcher` mode only: a subprocess is the only executor that can be given a worktree. When the `execute` role resolves to `agent` mode, do not prepare a wave. Run GREEN sequentially and add `note: "wave needs a dispatcher-mode executor"` to its record, with no `degraded` entry.

1. **Prepare.** Start clean for every task: delete `<state_dir>/wave/packets/` and `<state_dir>/wave/frozen.txt` if they exist, so no packet id of an earlier task survives into this one. Then write the frozen RED test paths, one per line, to a fresh `<state_dir>/wave/frozen.txt`, and each unit's filled GREEN packet to `<state_dir>/wave/packets/<packet_id>.md`. Then run `python <bundle_dir>/scripts/wave_engine.py prepare --cwd <absolute project path> --plan <the persisted plan> --packets-dir <state_dir>/wave/packets --workers <the decision's workers> --frozen <state_dir>/wave/frozen.txt`, plus one `--risk-glob <glob>` per `Risk matches` boundary.
   - `"mode": "sequential"`: there is no wave and nothing was created. Run GREEN as in Phase 3, and merge the answer's `metrics_args` dict into the payload of its `metric` record. They carry `workers_requested`, `workers_used: 1`, every `degraded` reason (`wave_disabled` while `parallel_execute` is off, `task_not_decomposable` when the manifest does not validate, the lease's reasons) and a `note` holding any `config_error`.
   - A non-zero exit: the same, with `note: "wave engine: <its error>"`.
   - `"mode": "wave"`: go on. From here, step 4 is your `finally`.
   - Whatever the mode, a non-empty `swept` means `prepare` first closed waves an interrupted earlier task left in this checkout. Report each swept wave's `wave_id` and `pending` units to the user: that earlier task's GREEN did not finish, and its pending units were never recovered. A swept entry with `"status": "closing"` still has `leftovers`; report them too, and the next `prepare` retries them.
2. **Loop on `next`:** `python <bundle_dir>/scripts/wave_engine.py next --cwd <abs> --wave-id <id>`.
   - `"action": "open"` comes with `unit`, `worktree` and `packet`. Assemble the unit's prompt as for a sequential GREEN dispatch, with `--objective <packet>` and the next free `--n` (`python <bundle_dir>/scripts/assemble_prompt.py --phase green --slug <slug> --n <n> --objective <packet> ... --emit dispatcher`). Then start, in the background (`run_in_background`) and with the Bash tool's `timeout` set to `(worker_timeout_seconds + 600) × 1000` ms, capped at 7200000 (the default background limit would stop the engine before it can time the worker out and write its record), `python <bundle_dir>/scripts/wave_engine.py exec --cwd <abs> --wave-id <id> --unit <unit> --archive <the archive> --tool <tool> --model <model> [--reasoning-effort <e>] [--size <s>] [--risk <r>] -- <the execute command>`. The execute command is the one the bridge skill named after the tool prescribes for a dispatcher-mode execute, with its project-path flag (`-C` for codex) pointing at the unit's `worktree`, and without `timeout`, `cat … |`, `sh -c` or redirections. The engine supplies stdin, cwd, the timeout, the evidence gate and the unit's commit. It records nothing itself: when `exec` returns, take the `record` field of its JSON answer (`{kind, phase, payload}`) and write it with `mcp__nucleos__record` (with the task's `task_id`) — the worker's metric row exists only if you do. Then call `next` again.
   - `"action": "wait"`: wait for one background `exec` to finish, then call `next`. An `exec` that exits non-zero, or whose background task was stopped, still counts as returned: call `next` all the same. The engine settles such a unit as `errored`, and a unit left `running` past `worker_timeout_seconds` + 600 s is settled the same way by `next`.
   - `"action": "drained"`: go to 3.
   Once the lease is gone, `next` stops opening units, the running ones finish, and the rest come back as `pending` for step 5. A worker that returns after the lapse has its record marked by the engine. There is nothing for you to decide here.
3. **Consolidate:** `python <bundle_dir>/scripts/wave_engine.py consolidate --cwd <abs> --wave-id <id>`. It merges the finished units in an `integration-wave-<id>` worktree and writes the result into your tree, uncommitted, as a sequential GREEN would leave it. Or it lands nothing (`landed: []`): when the wave aborted, or when your tree changed during the wave. **Never edit your tree while a wave runs.**
4. **Close — always:** `python <bundle_dir>/scripts/wave_engine.py close --cwd <abs> --wave-id <id>`, after consolidate, and also on any error, abort or interruption from step 1's `"mode": "wave"` on. It never fails. Keep its `pending` and its `metrics_args` (a dict to merge into the payload of the GREEN `metric` record). A `"status": "closing"` answer lists `leftovers` that cleanup could not remove (a tree a still-running worker holds): report them to the user; the next `prepare` in this checkout retries them. A wave an interrupted session left open is closed by the next `prepare` in this checkout.
5. **Recover sequentially.** Each unit in `pending` runs as an ordinary sequential GREEN dispatch of its packet, in your tree, and the payload of each of those `metric` records merges in close's `metrics_args` (a dict: `workers_requested`, `workers_used`, `degraded`, and `note` when there is one). When a recovery record has a `note` of its own, join it with the `note` inside close's `metrics_args` into ONE `note`, separated by `"; "` (close's note first).
6. **Then the integrity check, over everything** — wave and recovery together: the integrity check against the RED snapshot. Only after that, the single final verification. Never before the sequential recovery.

## Phase 4 - REFACTOR (optional, gated)

After GREEN, decide whether a refactor is warranted (duplication, unclear names, dead code surfaced by the implementation). If nothing clear, skip and note "Refactor: skipped".

| Rationalization | Reality |
|---|---|
| "obviously nothing to refactor" | Skipping is allowed only with a one-line stated justification in the report; an unexplained skip is not acceptable. |
| "tests are green, so the code is clean" | Green proves behavior, not clarity; scan for duplication, dead code, and unclear names before deciding. |

If warranted, dispatch a filled `execute.md` packet:

- Objective: "Refactor for clarity; behavior unchanged; all tests must stay green; do not modify the tests."
- Boundaries: Allowed = source; Do-not-touch = the test files.

Run no test after REFACTOR; re-verify only that the test-file snapshot is unchanged. The final verification catches any breakage.

### Final verification (you run it)

After the last code-changing phase (post-REFACTOR when it ran, else the final GREEN), run the final verification of Rule 3 (workflow.md) exactly ONCE, at every size, with the task's `task_id`. This single run is the recorded ship/no-ship decision and the GREEN proof (`## Batched test runs` step 5): green -> ship; red -> Rule 3; still red -> rescue, never ship. Record the evidence (command, result, tail). The only earlier controller test run is the RED proof.

## Phase 5 - Review (conditional)

Run if the review gate from Phase 1 says so (Risk elevated OR Size medium/large); otherwise skip. Rule 6 (workflow.md) says when review may run alongside the final verification and when its verdict is accepted.

Resolve the reviewer from `auto_overrides["review"]` or `review.*`. Build a reviewer prompt combining the `reviewer` skill (discovery path), the objective, the executor's filled GREEN Handoff, the RED, GREEN, and final verification evidence you recorded, `.nucleos/workflow/packets/review.md`, and the `Plan path:`/`Plan sha256:` pinned in Phase 1 for the reviewer to copy verbatim. Before reading the verdict, run the review gate per dispatch.md's "Review handoff" (pipe the filled packet into `python <bundle_dir>/scripts/review_gate.py`); only an exit-0 packet's verdict enters the handling below. Tell the reviewer to additionally confirm (a) RED evidence exists (tests were proven to fail before implementation) and (b) the test files were not weakened during GREEN.

Review authority (mirrors `orchestrate`, per Rule 6): the final verification has passed, so the reviewer's content verdict is advisory — record `request-changes`/`escalate` content findings as advisory annotations under `Risks` and proceed to Phase 6. **Gate-integrity carve-out:** the reviewer's two TDD confirmations — RED evidence present (tests were proven to fail first) and test files not weakened during GREEN — remain authoritative; a failure in either is blocking, treated as `request-changes` (send back). `approve` proceeds to Phase 6 either way. A send-back resumes the executor under the SAME boundaries as the phase being corrected (tests stay frozen during a GREEN/REFACTOR send-back); if it changed code after the final verification, that run is stale — verify once more (the once-per-task rule counts the run that ships, not superseded ones). The executor must verify each reviewer finding against the code before implementing, one at a time; a factually wrong finding is rebutted with evidence in the Handoff, not applied; no agreement phrases.

## Phase 6 - Wrap up

1. **Pending deletions.** Ask for confirmation before any deletion; report declined deletions as unresolved.
2. **Memory updates.** Collect executor/reviewer updates and append them to `<state_dir>/memory.md`. Maintenance auto-detects whether a consolidation pass is needed. Dispatch maintenance only when there are pending updates.
3. **Report to user:** Summary, Files changed (tests vs implementation), RED evidence, GREEN evidence, Refactor summary, Review verdict, Risks, Memory updates applied, and the Phase execution log.

## Hard rules

- **No in-context code changes.** You never write tests or implementation, never apply a diff produced by a dispatched executor, never "fix it yourself". The executor writes all code (exception: inline mode — see Notes).
- **You DO run the RED proof, the final verification and read-only checks.** Reading/hashing the test files is validation, not mutation. Read-only `git diff` is allowed. Editing any source or test file is not.
- **RED before GREEN, always.** No implementation dispatch until you have independently proven the tests fail for the right reason.
- **Tests are frozen during GREEN and REFACTOR**, enforced by the Do-not-touch boundary AND your snapshot comparison.
- **Synchronous dispatch only** (see dispatch.md). Never background-launch a phase.

## Dispatched-phase prompt contents, memory slice, metrics logging

As in `orchestrate`: §Dispatched-phase prompt contents (include-ONLY list, exclusions, assemble with the script, `prompt-contract.yaml`), §Memory slice (no full-memory fallback; the three STOP checks, each before RED), and §Metrics logging (one `mcp__nucleos__record` per dispatched phase, never composed from memory). Differences here: the `phase` field takes `plan`, `red`, `green`, `refactor`, `review`, `rescue`, or `maintenance`; `red`, `green` and `refactor` are all `execute` under a different objective and boundary set, so all three get the executor's sections and the `wf-executor` cockpit; `handoff_complete` applies to `red`/`green`/`refactor`; on those records add `workers_requested`, `workers_used` and one `degraded` entry per reason in the decision. The RED proof and the final verification are not dispatches and get no record; their evidence goes in the report.

## Error table (TDD-specific; the rest inherit from `orchestrate` and dispatch.md)

| Situation | Action |
|---|---|
| Planner `TDD-able: no` | STOP — route the user to `orchestrate`. |
| Planner output missing `TDD-able`, test command, or test-case list | STOP — invalid planner output. |
| Tests PASS at the RED proof | Bogus tests; resume the executor, re-prove RED for that packet. |
| Wrong-reason RED (test syntax error, unrelated fixture/import crash) | Resume the executor to fix the test, re-prove RED for that packet. |
| You cannot run the RED proof (no runner, env missing) | STOP and escalate. Never fall back to trusting self-reported evidence — it destroys the RED proof. |
| GREEN modified the test files (snapshot mismatch) | Reject; resume under the frozen-tests boundary. |
| Final verification still failing after recovery resumes | Rescue, report, stop. |
| REFACTOR broke green | Resume once or revert; still broken -> surface and stop. |
| Reviewer reports missing RED evidence | Treat as `request-changes`. |

## Notes

- When `dispatch_mode: auto` resolves the executor to `inline` (session tool+model match `execute.*`), the executor work runs in this session and you may write the code yourself — but the TDD sequence is unchanged: author the tests first, prove RED, only then implement, and let the final verification prove GREEN. The independent proofs are mandatory in every mode; inline does not relax them.
- Manual phase runs are only guaranteed to match `<state_dir>/models.yaml` when launched through the configured tool/model.
- `request-changes` has no automatic retry cap; each iteration prompts the user.
