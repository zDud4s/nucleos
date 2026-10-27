---
name: orchestrate
description: Use when a development task (code or docs) should run end-to-end from one prompt — the default entry point. Route TDD-able medium/large code to the orchestrate-tdd skill instead.
---

<!-- Cockpit: this skill runs in the controller's own session, under the user's permissions
     and with the user watching, so it carries no tool wall — and its frontmatter must not
     pretend otherwise. A skill's `tools:` field is inert (`tools:` belongs to agent files),
     and the walls in this pipeline are on the dispatched phases: `.claude/agents/wf-*.md`. -->

You are the orchestrator. You run the full workflow pipeline end-to-end from one task description.

**Read `.nucleos/workflow/workflow.md` and `.nucleos/workflow/dispatch.md` once before starting.** `workflow.md` holds the rules (Rule 3 verification, Rule 6 review) and introduces `<state_dir>` and `<bundle_dir>`, both read from `.nucleos/workflow/project.json`; this skill names those rules and never restates them. `dispatch.md` defines the dispatch contract, routing logic (`inline | agent | dispatcher`), prompt-passing convention (temp file -> stdin), resume rule, and dispatch-time error table. Everything below assumes those rules. Do not duplicate them.

**Packets are read-only templates.** `.nucleos/workflow/packets/*.md` is the schema layer: phases READ them and EMIT filled copies in their output. Filled execution packets flow through temp files to the executor and are deleted after dispatch. For medium/large tasks the planner MAY persist a filled plan at `<state_dir>/plans/<YYYY-MM-DD>-<slug>.md` (new file only). You must never call Edit/Write on `.nucleos/workflow/packets/`.

## Discovery path convention

"Discovery path" — the per-runtime path to a `<name>` skill body — is defined in `.nucleos/workflow/dispatch.md`, which you read at start.

## Entry point

The user invokes you with `Use the orchestrate skill. Task: [description]`.

**When NOT to use:** test-first TDD-able medium/large code goes to `orchestrate-tdd` (handed off at the deterministic-gate step).

## Output format

Returns a Markdown report containing:
- Task summary and outcome
- Files changed (paths and counts)
- Validation results (exit codes and test evidence)
- Unresolved risks (if any)
- Memory updates applied
- Phase execution log (plan, execute, review, wrap-up with tool/model/source columns)

## Pre-flight checks

First, a project with no workflow state yet: when `<state_dir>/project.yaml` or `<state_dir>/models.yaml` is missing, or `project_name` is `unknown`, run bootstrap before anything else. Bootstrap is what writes both files, so there is no `bootstrap.tool` to read yet: dispatch it in `agent` mode through the `wf-bootstrap` cockpit on the session's own model, the one exception to dispatch.md's never-substitute rule, and record it in the Phase execution log as `source=session`. When `models.yaml` exists and names `bootstrap`, dispatch through it as usual. After bootstrap returns, re-read both files and continue.

Then stop immediately if any fail: `<state_dir>/models.yaml` exists; `<state_dir>/project.yaml` `project_name` is not `unknown`; `.nucleos/workflow/dispatch.md` exists; the `executor` skill exists in your discovery path; the bridge skill named after `execute.tool` (`claude` or `codex`) exists in your discovery path; and, for any phase routing to `agent` mode, its cockpit file `.claude/agents/wf-<phase>.md` exists. If missing, use the dispatch error table wording.

Then resolve the **session identity** per dispatch.md's "Session identity" rule: use the model actually running this conversation (normalized — strip any `[...]` variant suffix), not the static `session.model`. If they differ, emit the drift warning and proceed with the running model for all routing and inline attribution.

## Speed

Resolve the speed once, after triage and before choosing a path:

1. the request's own: `speed: <value>`, or the words "fast"/"thorough" used as an instruction about
   how to run this task; else
2. `speed:` in `<state_dir>/project.yaml`; else
3. `normal`.

Then run `python <bundle_dir>/scripts/resolve_speed.py --speed <value> --size <triaged size> [--elevated]
[--tdd-able] --max-workers <parallel_execute.max_workers>` and read its JSON. Never interpret the
word `fast` yourself — the decision is the only thing that says what it buys. If the JSON carries
`config_error`, say it to the user in one line and carry on with the decision it returned (it is
`normal`'s).

- `fast_path_allowed: false` forbids the fast path below whatever triage says.
- `reasoning_effort` replaces `execute.reasoning_effort` for this task's execute dispatch.
- `review_required: true` runs review whatever Rule 6 alone would say.
- Test runs do not depend on the speed: the batched mode is the only mode (Rule 3), and its flow
  lives in orchestrate-tdd's `## Batched test runs`.
- `workers > 1` asks for a wave, which only orchestrate-tdd's GREEN runs (its `## Parallel GREEN
  wave`). Without one the task runs sequentially and the metric row carries the engine's
  `metrics_args`: `--degraded wave_disabled` while `parallel_execute.enabled` is `false`.
- Every execute metric row carries `workers_requested`, `workers_used` and one `degraded` entry
  per reason in the decision. A phase in `mode: agent` has no effort flag: add
  `degraded: effort_lever_unavailable` when the decision's effort differs from the configured one.

## Fast path - single-call execution (arm a)

**Unproven — treat as a heuristic.** Nothing measured supports a speedup from this path; the guardrails below are what makes it safe. After pre-flight, triage inline. The fast path applies only when ALL hold: the resolved speed allows it (`fast_path_allowed`); the task is unambiguous and clearly `trivial`/`small` (≤3 files, no new subsystem); change type is code or docs; no elevated-risk markers (auth, payments, security, data handling, migrations, deletions, prod config); the user didn't ask for a plan. It uses `execute.tool`/`execute.model` from `<state_dir>/models.yaml` directly — auto-select needs the planner, so `auto_overrides = {}` on this path.

When it applies: skip Phases 1 and 3. Build ONE execute dispatch — `execute.md` packet schema + user task + `project.yaml` + a memory slice. There is no planner here to emit `Memory tags`, so derive them yourself during the inline triage, from the closed `## Topic vocabulary` block in `<state_dir>/memory.md`, and slice as in "Memory slice" below. Not the whole file: a task small enough for this path is small enough to name its topics. Handoff check (`evidence_gate.py`) and the deterministic gate run unchanged: the single final verification (Rule 3) is the only test run, and its result is the ship/no-ship decision. Metric row: `phase:"execute"`, `size` from inline triage, `budget:null`.

Fallback (any of: triage uncertain between small/medium; executor escalates; triage_check.py run against Handoff `Files changed` derives medium+; final verification still red after the one recovery resume): restart at Phase 1 with the full pipeline; no second fast attempt. Dispatch-layer smoke/health checks ("pong"-style) are not tasks — run them against the cheapest configured model, never through the pipeline.

Do not talk yourself out of the guardrails on this path:

| Rationalization | Reality |
|---|---|
| "obviously trivial, skip triage rigor" | Still triage; a wrong trivial call skips review on an elevated-risk change. |
| "user is in a hurry, skip the plan" | Speed IS the fast path; the plan is skipped only when every applicability condition holds. |
| "I should re-run the executor's gate myself" | No. Rule 3: the Handoff is checked by `evidence_gate.py`; the one final verification decides ship/no-ship. |
| "metrics write failed, skip silently" | The write never aborts the pipeline, but note the failure in the report — do not hide it. |

## Phase 1 - Triage + Plan

Before dispatching the planner, triage enough to determine whether Size is `medium`/`large` or Risk is `elevated`. When either applies, complete `.nucleos/workflow/packets/intent.md` from the user's request, get it confirmed, and record it with `mcp__nucleos__record` (`kind: intent`, `task_id`, `payload`: the filled intent). The controller writes this task copy; the planner does not. Trivial and small low-risk tasks, including the single-call fast path, do not need an intent.

Read `plan.tool` and `plan.model` from `<state_dir>/models.yaml`. Build a planner prompt combining the `planner` skill (discovery path), the user task, relevant facts from `project.yaml` and `decisions.md`, and `memory.md` as its INDEX (`python <bundle_dir>/scripts/assemble_prompt.py ... --memory-index`: the header plus a per-tag entry count, never the entries — the planner greps the tags it needs), both packet schemas `.nucleos/workflow/packets/plan.md` and `.nucleos/workflow/packets/execute.md`, and the confirmed intent when required. Dispatch through the configured tool/model.

The planner output must state both `Size` and `Risk level` at the top. Size definitions and the elevated-trivial-promotes-to-small rule live in the `planner` skill — do not redefine them here.

Risk level controls review. Trust the planner's `Risk level` + `Risk matches`; run Phase 3 if `Risk level: elevated` OR Size is `medium` OR Size is `large`.

If planner output is missing `Size`, missing `Risk level`, or emits `TRIVIAL:` with `Risk level: elevated`, STOP and report invalid planner output.

Plan accepted and persisted under `<state_dir>/plans/` -> pin it now, per dispatch.md's "Plan pin": record its path and `sha256sum` for Phase 3. Not persisted -> both are `none`.

Run `python <bundle_dir>/scripts/triage_check.py --files <planner Relevant files> --claim-size <Size> --claim-risk <Risk level>`. On exit 1, adopt the stricter axis (promote Size and/or elevate Risk, never downgrade) and log `triage: promoted` in the report.

### Deterministic gate (code changes)

Medium/large code changes that are TDD-able default to the deterministic gate. When the planner emits Size `medium`/`large`, `Change type: code`, and `TDD-able: yes`, tell the user that `orchestrate-tdd` is the default route, then read the `orchestrate-tdd` SKILL (discovery path) and run its RED -> GREEN -> REFACTOR phases from Phase 2 onward in this session; do NOT stop and wait for re-invocation.

The ship/no-ship decision is the single final verification of Rule 3 (workflow.md): run once by you per task, at every size, after the last code-changing phase. Do NOT re-run a packet's `Validation.Commands` or any per-packet gate; the executor's evidence is accepted by `evidence_gate.py`.

For `TDD-able: no` code tasks, stay in normal orchestrate flow with no formal TDD cycle; the same single final verification (immediately after execute) is the deterministic gate.

### Auto-select handoff (when `auto_select.enabled: true`)

If `auto_select.enabled: true`, after receiving the planner output: locate the `## Selected models` block. Missing or malformed → STOP per the auto-select rows in dispatch.md's error table. Parse each line into `(phase, tool, model, reasoning_effort?, reason)`; header followed by zero lines = "no match", fall back to `models.yaml` for every downstream phase. Verify each parsed tool is locally available; if not, STOP `auto-selected tool unavailable: <tool> for phase <phase>` — do NOT silently fall back. Record `auto_overrides = { phase: (tool, model, effort, reason) }` for Phases 2-4. If `auto_select.enabled` is `false` or absent, set `auto_overrides = {}`.

### Large tasks: run a node tree

When Size is `large`, or the plan would not fit one controller within `nodes.orchestrator_budget` (speed.yaml), you are the **controller** and the root node of a tree. Small and medium tasks skip this section and run Phase 2 unchanged.

**Who does what.** Only you run cargo, gates, commits and merges, and only you write the contract. Every other node edits (a leaf, inside its own files) or splits (an orchestrator, without editing). The engine is `python <bundle_dir>/scripts/tree.py <verb>`: `init`, `branch add|bind|drop`, `budget`, `open`, `prompt`, `record`, `gate`, `review`, `merge`, `close`, `park`, `status`, `finish`. Each verb has `--help`. `--tree <id>` or the env var `NUCLEOS_TREE_ID` selects the tree for every verb except `init`. It prints JSON; exit 2 is a refusal, so read its `error` and fix the input instead of retrying. It never calls MCP: when a verb returns `{"mcp": [...]}`, make those calls in that order. The tree's state is `tree.json` and `node_tree`, never your context. Node-side rules live in `orchestrate-node`; the decomposition rules are in workflow.md ("Decomposition (node trees)").

**Precondition.** Before step 1, check that the `mcp__nucleos__node_open`, `node_update`, `node_close`, `node_tree` and `node_active` tools are in this session. If any is absent (the nucleos MCP is not connected, or the daemon predates the fix that gave the worktree box the `node_*` tools), open nothing and do not improvise a tree: tell the user why, and ask whether to run the task undecomposed as a medium flow.

**The cycle**

1. **Open.** `node_open` the root (no `parent_id`), then `tree.py init <root_id> --feature-branch <b> --goal <g>`. Its `dirty_owner_files` lists the owner's uncommitted files in the main checkout: tell the user now that a feature touching them will be Blocked at the land. Never stash them.
2. **Plan the top branches** by layer or surface and `branch add <name> --paths ... --layer ...` each one (`--base <branch>` when it needs another branch merged first). Write the shared contract sections **before** any branch opens. A spike, if needed, is branch 0, alone.
3. **Wave.** `tree.py budget` says how many branches open now and why. For each: `open <branch>`, `node_open` the branch node under the root with the branch paths, `branch bind <branch> --node-id <id>`, then `prompt <node_id> --branch <b> --kind orchestrator --goal <g> [--paths ... --pointers ...]` for the branch orchestrator and launch it (modes below). A branch you give up on is `branch drop <branch> --reason <r>`. A branch with `--base` opens as soon as its base is `merged`, without waiting for the wave.
4. **Record.** Save the root's `mcp__nucleos__node_tree` output to a file (it carries each node's `owned_paths` and `updated_at`), then run `tree.py record <node_id> --branch <b> --nodes <node_tree.json> [--report-file <f>]` (the report is read from stdin without `--report-file`) **one node at a time**: when a branch orchestrator returns (direct mode) or as each leaf returns (partition mode). A leaf owns its files until its own `record`, even if it already closed itself. `refused` or `reverted` files fail the node; an invalid report gets one retry with the error attached, a second failure marks it `failed`.
5. **WIP commit.** Right after each accepted `record`, before the next one, commit exactly the files it returns in `wip_commit.paths` in the branch worktree, with its `wip_commit.message`. A recorded leaf owns nothing any more, so its uncommitted files would read as a foreign write in the next `record` and be reverted. Never use `git add -A`.
6. **Gate the branch.** Run the branch's commands through the project's heavy-command broker (and `mcp__nucleos__verify` with `scope` on the branch worktree when it matches the project's test groups), then `tree.py gate <branch> --exit N --log F --sha S`. Red: a small fix tree (below), then back here.
7. **Review the branch** with a fresh read-only `reviewer` over the branch diff and the contract. Triage the findings and submit them as a JSON list with `tree.py review <branch> [--findings F]` (stdin otherwise; integration findings: `review --integration`). `keep` findings become a fix tree under the branch node; back to step 6.
8. **Merge.** `git merge --no-ff <branch>` through the VCS queue, then `tree.py merge <branch> [--sha <merge commit>]` (make the `node_close` it returns: that closes the branch node and frees its paths for a branch based on it) and `tree.py close <branch>`. An `unreviewed` warning means step 7 was skipped. A conflict goes to the existing resolver.
9. **Integrate.** When every branch is `merged`: **one** `mcp__nucleos__verify` with `scope: "scope"` on the feature branch (Rule 3), then an integration review (Rule 6) over the whole diff. Fixes hang under the root.
10. **Land and finish.** Land on master through the queue, then `tree.py finish` and make its `node_close` call. If the land is Blocked by the owner's files: `tree.py park tree --needs "clean <files> in the main checkout"`.

**Launch modes.** Same protocol in both; pick by the harness resolved for `orchestrate_node`, and use the report mode when in doubt.
- *Direct* (Claude Code): launch the branch orchestrators in one message, in the background, through the `wf-orchestrator` cockpit (`Agent`), prompt from `tree.py prompt`. They launch their own children and return one aggregated report. This is the one exception to the synchronous rule in Phase 2: dispatched phases stay synchronous; tree orchestrators are not phases.
- *Partition by report* (any harness): the orchestrator launches nothing and returns `outcome: split` with the children's pieces; you open and launch them, and `record` each as it returns. Same shape in phase 2 of the roadmap, where the daemon launches.

**Fix trees.** Group the `keep` findings by file; each group is a piece owned by those files. One group is a leaf, several are an orchestrator with leaves. Hang them under the branch node (branch review) or under the root (integration review). The reviewer who found them never fixes them.

**Contract.** A node needing something the contract lacks returns `contract_request`; it never decides. You decide, write `E<n>` (date, branch, reason), relaunch the asker with the new section, and open an adjustment piece in any running branch the decision affects. A product decision only the owner can take is a `park`, not a question.

**Shared resources.** `tree.py budget` and `gate` record disk samples. Disk below the floor mid-run: open no branch and start no gate, `close` only your own `merged` branches' target dirs, and if that is not enough `park tree --needs "disk"`; never delete caches that are not yours. A foreign lock held for more than `tree.lock_wait_minutes`: move to another branch with work, come back later. The project's heavy-command broker answering "busy": retry with the longer timeout.

**Never ask the owner mid-run.** Nothing in a tree is a question to the user. A blocker is `park node <id> --role <leaf|orchestrator|review> --needs ...`, `park branch <b> --needs ...` or `park tree --needs ...`, which reaches the channel through the feed; `park branch` and `park tree` run in two steps (the first returns a `node_open` payload, make it, then rerun the same command with `--marker-id <id>`). Then carry on with the other branches. After a compaction or a dead session: `tree.py status --nodes <node_tree.json>` plus `node_tree`, close the dead `running` node as `handoff` and relaunch the same piece. A parked branch whose decision arrived resumes with `tree.py park branch <b> --resume`.

You still own what no node may own: the intent, RED (orchestrate-tdd), the single final verification (Rule 3), review (Rule 6) and maintenance. Do not restate node mechanics here; the `orchestrate-node` skill is their single home.

| Rationalization | Reality |
|---|---|
| "The leaf can run its own tests to save a round trip" | Only the controller runs cargo and gates; a node that builds is unmeasured and the guard denies it and logs it. |
| "The owner is watching, so ask them about the disk" | A tree never asks mid-run; park with `--needs` and continue the other branches. |
| "Commit once at the end, when both branches are done" | WIP commit after every accepted `record`; a closed leaf owns nothing, so its uncommitted files are reverted as foreign writes by the next `record`. |

## Phase 2 - Execute

If `auto_overrides["execute"]` is set, use its `(tool, model, reasoning_effort)`. Otherwise read `execute.tool`, `execute.model`, and `execute.reasoning_effort` from `<state_dir>/models.yaml`. Dispatch the execution packet (or trivial instruction) through the resolved tool. The executor's instructions are the **`executor`** skill (discovery path) — that is the phase skill body you inline into the prompt. Tool-specific invocation details live in the bridge skill named after `<execute.tool>`; read that one **yourself** and never inline it, per dispatch.md's "The phase skill, and the skill that is not one". When dispatch routing resolves to `agent`, the controller delegates via the Claude Code Task tool (in-process subagent) instead of a subprocess; see dispatch.md.

### Hard rule: no in-context execution

The orchestrator does not make code changes itself unless `execute` resolves to `inline` per dispatch routing. Never Edit/Write a patch produced by a dispatched executor, never extract a diff from its output and apply it, never offer "let me apply it myself" — if you catch yourself doing any of these, STOP.

### Hard rule: synchronous dispatch only

Every dispatched phase MUST be launched synchronously — the orchestrator blocks until the subprocess exits or its timeout fires. Never use background-launch flags (`run_in_background: true`, shell `&`, `nohup`, `Start-Process` without `-Wait`, etc.). Background mode returns immediately with metadata instead of the phase output, which breaks the Handoff check, dispatch tracker, and metric row. If a phase needs more than the default timeout, raise `<phase>.timeout_seconds` in `<state_dir>/models.yaml`. See dispatch.md "Synchronous-call invariant". The one exception is the branch orchestrators of a node tree in direct mode (see "Large tasks: run a node tree"): they are launched through `Agent` in one message and recorded afterwards by `tree.py record`.

If the executor fails (timeout, non-zero exit, environment/permission block), only allowed responses: report the exact error; suggest `<state_dir>/models.yaml` or executor-environment fixes; dispatch rescue (`auto_overrides["rescue"]` if set, else `rescue.tool`/`rescue.model` from `<state_dir>/models.yaml`); STOP and wait for user direction.

### Handoff check

Wait for the executor to complete within the configured timeout (see dispatch.md), then handle the exit: timeout -> freeze, rescue, report, stop; non-zero with `## Escalation` -> surface `reason`, `needed`, `suggested-next`, `partial-output`, ask rescue/re-plan/abandon, do not advance; non-zero without escalation -> tooling failure; exit 0 -> inspect `## Handoff`.

Proceed only if `Files changed`, `Tests added`, and `Validation evidence` are complete. `Validation evidence` needs one block per validation command with `$ <cmd>`, `exit:` (or `deferred: batched`), and `tail:` lines; concrete `could not run:` reasons are acceptable. Check it with `python <bundle_dir>/scripts/evidence_gate.py` (Rule 3).

If incomplete, attempt ONE recovery resume using the `<execute.tool>` skill (discovery path). Prompt: "The Handoff section is incomplete. Re-fill it. `Validation evidence` must contain one block per command in Validation.Commands, as the executor skill defines. If a command could not run, state a concrete reason. Output the completed Handoff. Before changing anything, state the one-line root cause you verified; a fix without a stated root cause is rejected."

After the resume: incomplete Handoff -> dispatch rescue, report, stop. Do not advance to review.

Handoff complete (evidence gate passed) -> run the single final verification now (Rule 3, workflow.md), over the whole branch diff, passing the task's `task_id`. Its result is the recorded ship/no-ship decision. A red follows Rule 3; still red -> rescue, stop.

## Phase 3 - Review (conditional)

Run if the review gate from Phase 1 says so; otherwise skip. Review may start in parallel with the final verification when Rule 6 allows it; the verdict is accepted only after that verification is green and the review gate passes.

If `auto_overrides["review"]` is set, use its `(tool, model)`. Otherwise read `review.tool` and `review.model` from `<state_dir>/models.yaml`. Build a reviewer prompt combining the `reviewer` skill (discovery path), the execution packet objective, the executor's filled Handoff, `.nucleos/workflow/packets/review.md`, and the confirmed intent when one was required, and the `Plan path:`/`Plan sha256:` pinned in Phase 1 for the reviewer to copy verbatim. The reviewer compares the Handoff against the intent's `Outcome`; a deviation is recorded as a `scope_creep` rework row (`mcp__nucleos__record`, `kind: rework`) before its verdict. Dispatch through the resolved tool/model.

Before reading the verdict, run the review gate per dispatch.md's "Review handoff": pipe the filled review packet into `python <bundle_dir>/scripts/review_gate.py`. Only an exit-0 packet's verdict enters the handling below; a moved plan STOPs, any other refusal re-dispatches review once.

Review authority: where a deterministic gate exists and passed, the reviewer verdict is advisory. Record reviewer `request-changes` / `escalate` findings as advisory annotations under `Risks` and continue; the reviewer's hard-gate checks remain authoritative for validation evidence present, gate ran, green, and tests accounted for because they confirm the gate itself. Tasks without a deterministic gate keep the blocking reviewer verdict. This overrides the Verdict handling below for `request-changes` / `escalate` when the gate passed.

`approve` always proceeds to Phase 4 (gate passed or not). Verdict handling for `request-changes` / `escalate` when no deterministic gate passed (gate-less task, or the gate failed): `request-changes` -> show findings and ask send back / accept / stop; `escalate` -> STOP and report full findings + Handoff.

For send-back, re-dispatch the executor with `Reviewer findings:\n<findings>\n\nOriginal objective: <objective>\n\nVerify each reviewer finding against the code before implementing, one at a time; a factually wrong finding is rebutted with evidence in the Handoff, not applied; no agreement phrases.` via temp file -> stdin — a fresh write-capable `execute` dispatch, NOT `resume --last` (resume can't carry the bypass flag, so it stalls codex's sandbox and fails the permission allow rule; see dispatch.md's resume rule). The executor's prior edits are already in the tree, so nothing is lost. Re-run review and increment N. There is no automatic cap; warn from iteration 5 onward. Gate-failure fix loops instead follow the 3-strikes rule in orchestrate-tdd (rescue at the 3rd failed same-gate iteration); this review send-back is the user-mediated loop and only warns.

If accepted without changes, surface unresolved findings under `Risks` with "Reviewer findings accepted without changes (iteration N)".

## Phase 4 - Wrap up

1. **Pending deletions.** Ask for confirmation before any deletion; report declined deletions as unresolved.
2. **Memory updates.** Collect executor/reviewer updates and append them to `<state_dir>/memory.md`. Maintenance auto-detects whether a consolidation pass is needed (size, contradictions, density triggers in its SKILL — no flag from the orchestrator). Dispatch maintenance only when there are pending updates to apply.
3. **Report to user:** Summary, Files changed, Validation, Risks, Memory updates applied, and Phase execution log. Per-phase log line columns: `tool`, `model`, `source=auto|config` (`auto` when the value came from the planner's `## Selected models` block, `config` when from `<state_dir>/models.yaml`), and when `source=auto`, the `reason` from the planner. `configured`, `resolved`, and `command` columns are unchanged.

Examples:
`plan tool=claude model=claude-opus-4-7 source=config configured=auto resolved=inline command=inline`
`execute tool=codex model=gpt-5.6-sol source=auto reason="small/low/medium-budget" configured=auto resolved=dispatcher command=codex exec ...`

## Dispatched-phase prompt contents

When you build a delegated prompt for any phase, include ONLY:
- the phase skill body (from discovery path)
- the relevant packet schema from `.nucleos/workflow/packets/`
- `project.yaml`
- the user task / current objective
- the relevant slice of `memory.md` (see "Memory slice" below)

Do NOT include: `dispatch.md`, this skill, any other phase skill, or the `claude`/`codex` bridge skills. The dispatch contract is yours alone; dispatched phases only need their own skill plus the schema they will fill.

The phase-to-skill map (`plan`→`planner`, `execute`→`executor`, `review`→`reviewer`, `rescue`→`rescue`, `maintenance`→`maintenance`, `bootstrap`→`bootstrap`) is in dispatch.md. A phase gets one skill body, and it is the one named after it.

**The list above is also data, in `.nucleos/workflow/prompt-contract.yaml`** — `order`, `never`, and per-phase `allowed`/`required` section keys. Read it when assembling; it is the version a check can hold you to, and this prose is the version that can quietly stop being true.

**Assemble with the script, dispatch what it wrote, record it with the writer.** For every dispatched phase:

1. Run `python <bundle_dir>/scripts/assemble_prompt.py --phase <phase> --slug <task_slug> --n <n> --objective <file>` plus the sections this phase takes (`--packet`, `--memory-tags`, `--memory-full`, `--decisions`, `--prior-artifacts`). It archives the prompt with section markers and a manifest beside it, and refuses to write at all when the result would breach the contract — a forbidden section, a missing required one, a tag outside the closed vocabulary, a tag list that selects nothing, or forbidden material anywhere in the body.
2. Dispatch the text it prints. In `agent` mode that is a pointer to the archive; in `dispatcher` mode (`--emit dispatcher`) the archive is the stdin file. Do not retype, re-abridge or "tighten" the prompt on the way out.
3. When the phase returns, record it (see Metrics logging). The manifest values come from the file the assembler left beside the archive, never from your memory.

**Do not hand-assemble.** A hand-made archive diverges from what was dispatched, and an unmeasurable dispatch cannot be trusted afterwards.

| Rationalization | Reality |
|---|---|
| "the phase will figure out what it needs, extra context is harmless" | Extra context is the cockpit failing. A phase given `dispatch.md` reads instructions addressed to you and acts on them; that is why the one-controller-per-node invariant exists. |
| "the archive is close enough; I trimmed a little on the way out" | Then the archive is not evidence of anything. Dispatch the file the script wrote. |
| "the prompt is gone, I will write the manifest from memory" | Then it is a claim, not a measurement. Archive the prompt as you assemble it, not afterwards. |

### Memory slice

After Phase 1, parse the planner's `Memory tags: [tag1, tag2, ...]` line. For Phases 2-4 dispatched prompts, inject only `memory.md` entries whose topic tag matches the list — e.g. `grep -E '^\- [0-9-]+ \[(tag1|tag2|tag3)\]' <state_dir>/memory.md`. Always include the file header (through `## Topic vocabulary`) so the dispatched phase understands the entry format and the closed tag list.

**There is no full-memory fallback.** Three checks, in this order, each a STOP:

1. **Line missing** (and the planner did not emit `TRIVIAL:`) — STOP: `invalid planner output: Memory tags line missing`.
2. **A tag outside the vocabulary** in `<state_dir>/memory.md`'s `## Topic vocabulary` block, canonical or legacy — STOP: `invalid planner output: unknown memory tag <tag>`.
3. **Non-empty tags, empty slice** — STOP: `memory slice empty for tags [<tags>]`.

`Memory tags: []` is itself invalid output and fails check 1's class. Injecting the whole file when the tag line is unusable hands a phase every entry about subsystems it will never touch, and because it *works*, a planner that names a topic no entry carries is never corrected. The three checks turn both into a stop the user can see.

## Metrics logging

After every dispatched phase completes (regardless of `auto_select.enabled`), record it with `mcp__nucleos__record` (`kind: metric`, `task_id` = the task slug, `phase`, `payload`). Never abort the pipeline if the write fails. Source data for the adaptive scorer.

**Never compose a row from memory.** Take the manifest fields (`prompt_sections`, `memory_tags`, `memory_lines`, `prompt_file`, `prompt_sha256`) from the manifest file `assemble_prompt.py` left beside the archive, and the observed fields at the moment the phase returns. If a dispatch was not recorded when it returned (context compacted, step skipped), leave it unrecorded and name it in the report — an honest gap beats a rebuilt row. An `inline` phase has no assembled prompt: name it in the report.
<!-- F5-OPEN: confirm whether mcp__nucleos__record reads the assemble_prompt manifest itself or expects the fields in the payload. -->

Payload fields (one record per dispatch):

```
{"phase":"<plan|execute|review|rescue|maintenance|bootstrap|red|green|refactor>","tool":"<tool>","model":"<model>","reasoning_effort":"<low|medium|high|xhigh|null>","size":"<trivial|small|medium|large|null>","risk":"<low|elevated|null>","budget":"<low|medium|high|null>","exit_code":<int>,"duration_ms":<int>,"handoff_complete":<bool|null>,"review_verdict":"<approve|request-changes|escalate|null>","retries":<int>,"gate_status":"<passed|failed|errored|null>","gate_exit_code":<int|null>,"tokens_in":<int|null>,"tokens_out":<int|null>,"cache_read":<int|null>,"cache_creation":<int|null>,"workers_requested":<int|null>,"workers_used":<int|null>,"degraded":[<reasons>],"note":"<text|null>","prompt_sections":[<section keys, in order>],"memory_tags":[<tags, or []>],"memory_lines":<int>,"prompt_file":"<path of the archive>","prompt_sha256":"<64 hex, of the archived file's bytes>"}
```

Field rules: `task_id` lowercased+hyphenated, same across phases; `tool`/`model`/`reasoning_effort` post-`auto_overrides`; `size`/`risk`/`budget` `null` for `plan`; for non-`plan` rows, `budget` records the `effective_budget` the selector decided on: configured `auto_select.token_budget` after the one-rung upgrade (`low`→`medium`→`high`, `high` stays) when Risk is elevated or Size is large; `exit_code` `0` on inline success; `duration_ms` wall-clock from dispatch start; `handoff_complete` only for `execute` (else `null`); `review_verdict` only for `review` (else `null`); `retries` = recovery resumes + review send-backs; `gate_status`/`gate_exit_code` record the final verification's result — `passed` on green, `failed` on red, `errored` when it could not run, and `null` only when no gate ran for this phase. Never derive them from `exit_code`: that field is a model's own word in `agent`/`inline` routing and answers a different question. `memory_lines` is the entry count the slice actually produced. `prompt_sections` holds the keys from `prompt-contract.yaml` for the sections you actually inlined, in order.

Token capture: dispatcher + codex populates `tokens_out` from the printed "tokens used" total; the other token fields stay null. Dispatcher + claude populates all four from `--output-format json` `.usage` (`input_tokens`, `output_tokens`, `cache_read_input_tokens`, `cache_creation_input_tokens`). Agent (Task tool) + inline leave all token fields null.

One record per dispatched phase; a re-dispatch is a new assembly (`--n`).

## Pipeline error table

Dispatch-layer errors (missing config, tool unavailable, unrecognized values, session-block issues) are in `.nucleos/workflow/dispatch.md`. The rows below are pipeline-specific.

| Situation | Action |
|---|---|
| Planner output missing `Size`/`Risk level`, or elevated `TRIVIAL:` | STOP: invalid planner output. |
| `Memory tags` line missing, `[]`, or carrying a tag outside `<state_dir>/memory.md`'s vocabulary | STOP: invalid planner output. Never fall back to the full `memory.md`. |
| Tags parse but the slice comes back empty | STOP: `memory slice empty for tags [<tags>]`. An empty slice is a contradiction, not a small one. |
| Executor timeout | See dispatch.md timeout row; freeze, rescue, no auto-retry. |
| Executor non-zero with escalation | Surface the four fields. |
| Executor non-zero without escalation | Phase 2 allowed options only. |
| Handoff incomplete | Rescue and stop. |
| Reviewer `request-changes` | No gate passed: ask send back / accept / stop. Gate passed: annotate per the Phase 3 advisory rule; warn from iteration 5. |
| Reviewer `escalate` | No gate passed: stop and report full context. Gate passed: annotate per the Phase 3 advisory rule. |
