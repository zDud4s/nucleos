# AI workflow shared instructions

## Pipeline

0. **Intent**: after triage, the controller records and gets confirmation of a filled intent before planning when Size is `medium`/`large` or Risk is `elevated`; trivial and small tasks, including the single-call fast path, skip this stage

1. **Triage**: planner classifies task size (trivial / small / medium / large); for unambiguous trivial/small low-risk tasks the orchestrator may take the single-call fast path instead (see the orchestrate skill; unproven heuristic)
2. **Plan**: planner produces execution packet(s) using the schemas in `.nucleos/workflow/packets/`
3. **Execute**: executor follows packet steps literally, fills Handoff section when done
4. **Review**: reviewer checks Handoff output after the controller's single final verification (runs when Risk is `elevated` or Size is `medium`/`large` — see Rules 3 and 6)
5. **Maintain**: update `<state_dir>/memory.md` and `<state_dir>/decisions.md` with discoveries

When an intent was required, review compares the execution Handoff against its `Outcome`. Any deviation is recorded as a `scope_creep` rework row (`mcp__nucleos__record`, kind `rework`) before the reviewer returns its verdict.

The optional `parallel_execute` wave is disabled by default and does not change the lifecycle above; its mechanics are in the orchestrate-tdd skill's `## Parallel GREEN wave`.

## Roles

Role assignments live in `<state_dir>/models.yaml`. Every node of a task tree has exactly one controller, which dispatches only its direct children through the configured tool/model and never substitutes its own model for a configured phase (see `dispatch.md`, "One controller per node").

## Decomposition (node trees)

- Split breadth first, along logical boundaries with clear file ownership (`owned_paths`).
- Go deeper only until each leaf fits `leaf_budget`, counting its reads with exact line ranges. A big file read whole does not justify more splitting: the packet gives line ranges.
- Limits come from `speed.yaml` `nodes:` (`max_depth`, `max_width`, `leaf_budget`, `orchestrator_budget`, `ceiling`).
- A node closed `handoff` because of the ceiling generates a rework row (`mcp__nucleos__record`, kind `rework`, `caught_by` `budget`, payload kind `decomposition`): a session past the threshold is a decomposition defect.
- Rule 3 stands: exactly ONE final verify, by the ROOT. Nodes never run build or test tools; they return the commands in their handoff. The RED proof and the final verify stay with the root.
- Optional `subtree_verify` (`speed.yaml`, default `false`): when `true`, a sub-orchestrator MAY call `verify` with a scope restricted to its branch as an intermediate gate. The root's final verify remains the only ship/no-ship decision.

## Dispatch

The shared dispatch mechanism (routing modes, prompt-passing, resume rule, review handoff, config error table) lives in `.nucleos/workflow/dispatch.md`. Any controller reads it once and follows it.

## Layer model

Two paths are not fixed in this text and are read from `.nucleos/workflow/project.json`, written by the daemon when it materializes the bundle: `state_dir` (the project's mutable state, `~/.nucleos/projects/<id>/`) and `bundle_dir` (the library directory of this bundle version, whose `scripts/` hold the helper scripts). Everywhere else these are written `<state_dir>` and `<bundle_dir>`; both sit under the home directory, which can hold a space, so quote each path you substitute into a command (`python "<bundle_dir>/scripts/x.py"`). State lives outside the working tree, so it can never enter git.

| Layer | Files | Mutability |
|---|---|---|
| **Workflow core** | `.nucleos/workflow/*.md` and `*.yaml`, `.claude/skills/*/SKILL.md`, `.claude/agents/wf-*.md` (the phase cockpits — the `tools:` line in each is the wall the harness enforces), `.agents/skills/*/SKILL.md` (the mirror a Codex controller reads FIRST, generated from `.claude/skills/` when the bundle is packaged; bridge skills `codex`/`claude` intentionally differ) | Read-only — materialized by the daemon from the bundle; changes only when the bundle version changes. |
| **Packet schemas** | `.nucleos/workflow/packets/*.md` | **Read-only templates.** Phases READ + EMIT filled copies; never edit. |
| **Project state** | `<state_dir>/project.yaml`, `<state_dir>/models.yaml`, `<state_dir>/pricing.yaml`, `<state_dir>/memory.md`, `<state_dir>/decisions.md` (+ `<state_dir>/memory-archive.md`, append-only by consolidation, never read by phases) | Mutable per task by `maintenance` + human edits. |
| **Task instances** | `<state_dir>/plans/<date>-<slug>.md`, `<state_dir>/specs/<date>-<slug>.md` | New-file persistent copies for medium/large tasks; never overwrite. |

Filled packets flow via stdin/temp files (see `dispatch.md`); never Edit/Write `.nucleos/workflow/packets/*.md` during a task — workflow violation.

### State directory reference

| Path under `<state_dir>` | Holds | Distinct from sibling |
|---|---|---|
| `plans/` | Persisted execution plan (packet-level breakdown) for medium/large code tasks. | `specs/` is the design; `plans/` is the executable phase breakdown. |
| `specs/` | Persisted spec (broader design/intent) for large tasks. | Written before/above the plan; large tasks may have both. |
| `loops/` | Spec-session checklists (see below), one file per session. | Working state of a session, not a deliverable. |
| `nodes/` | Handoff files of node trees, one directory per root (`nodes/<root_id>/<node_id>.md`). | Written by nodes at close; the daemon holds the tree itself. |
| `prompts/` | The archived prompt of each dispatched phase (see `prompt-contract.yaml`). | Evidence of what a phase was sent. |

Metrics, rework, intent and event rows are not files you write: the controller records them with the MCP tool `mcp__nucleos__record` (`{kind: metric|rework|intent|event, task_id, phase?, caught_by?, payload}`), and the daemon owns the ledgers.

## Rules

1. If `project_name` in `<state_dir>/project.yaml` is `unknown`, run bootstrap first.
2. Bootstrap may NOT rewrite the workflow core nor implement product changes; preserve existing repository instructions.
3. **Verification.** Executor must fill the Handoff section before declaring done. `Validation evidence` is mandatory — one block per validation command (exit code + output tail), enforced by `python <bundle_dir>/scripts/evidence_gate.py <filled packet>` before a controller accepts an execute Handoff. Self-reported success without evidence is not acceptable. Agents do not verify every step; they guarantee the FINAL delivery. Batched mode is the only mode: executors run no tests (they mark each test command `deferred: batched`), the controller proves RED once for every packet together, and the final verify is the GREEN proof. Exactly ONE final verification runs per task, at every size, by the controller, after the last code-changing phase: `mcp__nucleos__verify` with `scope: "scope"` and the task's `task_id` (diff-aware selection over the repository's `nucleos.tests.yaml`; its `full_sweep` paths widen the selection by themselves). It returns a ticket; poll `mcp__nucleos__verify_status` until it settles. That single result is the recorded ship/no-ship decision, and there is no separate full-suite run. If it is red, fix and re-verify through `verify` (green units are cached). A failure that passes on the re-run with the tree untouched — a flaky or timing failure — closes the gate, recorded with the test that flaked. Only the daemon's `verify` runs tests. Subagents never run build or test tools: they hand back the exact command and the controller runs it. The mechanics of RED and of the wave live in orchestrate-tdd.
4. Prefer the smallest correct change; do not broaden scope silently.
5. A phase SHOULD be launched through the tool/model configured in `<state_dir>/models.yaml` (or the planner's `## Selected models` block when auto-select is enabled). Manual runs that bypass dispatch generate no metric row (`mcp__nucleos__record`, kind `metric`), so they cannot be scored by the adaptive selector — the orchestrator marks them `source=manual` if a user later replays them.
6. Review runs when Risk level is `elevated` OR Size is `medium`/`large`. Size alone never bypasses risk; for code changes the deterministic gate is the ship/no-ship decision; LLM review is advisory. Review may run in parallel with the final verify when the branch's previous verify was green; the verdict is accepted only after that verify comes back green and `python <bundle_dir>/scripts/review_gate.py` (filled review packet on stdin) exits 0 — see `dispatch.md`, "Plan pin" and "Review handoff".

The resolved speed can add review, never remove it: `review_required` from `python <bundle_dir>/scripts/resolve_speed.py` is `true` under `thorough` whatever the size and risk, and under every other speed it is exactly this rule.

## Spec sessions: exit criteria and closing report

A session asked to *work on* a spec or plan (not to discuss one) opens a checklist first and does not end before it does.

1. Open it: `python <bundle_dir>/scripts/spec_loop.py start <spec-path> "<deliverable>" ...` — one `- [ ]` item per deliverable the spec asks for. It lives at `<state_dir>/loops/<session_id>.md`; tick each item `- [x]` as it lands.
2. Do not stop, and do not ask "queres que continue?", while an item is open. The only exits: every item done; an item needing a decision only the owner can make, marked `- [!] ... — precisa de: <decisão>` with `status: blocked` (independent items get done first); or context past its ceiling, `status: handoff` plus a written handoff for a new session.
3. The last message of the session is the report, in European Portuguese, with exactly these headings: `## Feito` (item + evidence), `## Falta` (open and blocked items, or "nada"), `## Verificação` (what was actually run and its result), `## Próximo passo`.

Enforced, not advisory: the `Stop` hook (`spec_loop.py --stop-hook`) refuses the stop while items are open and refuses a closing message without the four headings; the `UserPromptSubmit` hook injects step 1 when a prompt names a spec. Both hooks are wired by the daemon from the bundle manifest. It lets go after two pushes that closed nothing, so a stuck session ends instead of looping.
