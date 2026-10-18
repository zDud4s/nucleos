# Dispatch mechanism

Shared rules for any controller (currently the orchestrator) that dispatches workflow phases through configured tools and models. The pipeline itself lives in the `orchestrate` skill (`.claude/skills/orchestrate/SKILL.md` for Claude, `.agents/skills/orchestrate/SKILL.md` for Codex); everything here is the mechanical layer underneath it. `<state_dir>` and `<bundle_dir>` are read from `.nucleos/workflow/project.json` (see `workflow.md`, "Layer model").

## Discovery path

"Discovery path" (referenced throughout the skills) resolves a `<name>` skill body per runtime:

- **Claude controller:** `.claude/skills/<name>/SKILL.md`.
- **Codex controller:** `.agents/skills/<name>/SKILL.md` **in this repository first**, and only `~/.agents/skills/<name>/SKILL.md` if the repository has no copy.

Repo-local first is the point: `~/.agents/skills/` is shared by every project on the machine, while the workflow is versioned per repository, so a global-only path would hand a Codex controller whatever another project left there. `.agents/skills/` is the mirror generated from `.claude/skills/`, so repo-local first is exactly "the contract this repository declares"; the global copy is only a fallback.

## Dispatch contract

A controller never substitutes its own model for a configured workflow phase. For every phase (`plan`, `execute`, `review`, `rescue`, `maintenance`, `bootstrap`):

1. Read `<phase>.tool` and `<phase>.model` from `<state_dir>/models.yaml`.
2. Build a standalone prompt packet for that phase, containing:
   - relevant skill instructions from the `<phase>` skill, resolved via the controller's discovery path when available. The phase-to-skill map is one-to-one and includes `execute` → **`executor`**: the `claude`/`codex` skills named after a *tool* are the controller's invocation manuals and never go into a dispatched prompt (see "The phase skill, and the skill that is not one")
   - the current objective
   - the required repo context
   - the relevant packet schema from `.nucleos/workflow/packets/`
3. Execute the phase through the configured tool/model.
4. Capture and retain: phase name, requested tool, requested model, actual command used, exit status.
5. If the configured tool is unavailable, STOP and tell the user to update `<state_dir>/models.yaml` or fix the tool. Never fall back to the current session's model.

A phase runs in the current context window only if routing resolves to `inline` (see below).

The optional `parallel_execute` wave is disabled by default and does not alter this dispatch contract. When enabled it runs GREEN only, through `<bundle_dir>/scripts/wave_engine.py`, sequenced by the orchestrate-tdd skill's `## Parallel GREEN wave`: each worker is a dispatcher-mode execute dispatch whose stdin, cwd, timeout, evidence gate and metric row the engine owns.

## Session identity

Routing and inline attribution key off the **session identity**: the `(tool, model)` actually running the controller — the live conversation/process you are executing in — NOT whatever the static `session:` block in `<state_dir>/models.yaml` happens to say. Resolve it once, every run, before routing:

1. **`tool` / `model` = the controller's real runtime identity.** A live controller always knows what it is: Claude Code knows it is `claude` and which model powers the conversation; codex knows its invocation model. That runtime value is **authoritative**.
2. **Normalize the model id before any comparison.** Strip a trailing bracketed variant tag so the live id matches the catalog id — e.g. `claude-opus-4-8[1m]` → `claude-opus-4-8`. (The family/version collapse for `agent` mode, below, applies *after* this normalization.)
3. **`session:` in `<state_dir>/models.yaml` is the declared default, not the source of truth.** It tells whatever launches sessions what to launch and gives a fallback to a controller that genuinely cannot introspect itself. For a live controller, runtime wins.
4. **On drift, warn and proceed with runtime.** If the normalized runtime model differs from `session.model` in config, emit once and continue with the runtime value:

   > "Note: `<state_dir>/models.yaml` `session.model` is `<config>` but this session is actually running `<runtime>`. Using the running model for routing and inline attribution; update `session.model` to silence this."

   Never let a stale config make the running model masquerade as a different tier.

Everywhere below, `session.tool` / `session.model` mean this resolved runtime identity.

## Dispatch routing

Each phase resolves to one of three execution modes: `inline`, `agent`, or `dispatcher`.

When `dispatch_mode: auto` (and no explicit per-phase `mode` override):

| Comparison | Mode |
|---|---|
| `phase.tool != session.tool` | dispatcher |
| `phase.tool == session.tool`, models differ | agent |
| `phase.tool == session.tool`, models match | inline |

Override / fallback table:

| `dispatch_mode` | phase `mode` field | Result |
|---|---|---|
| `auto` | not set | computed from session comparison |
| `auto` | set | explicit `mode` overrides auto |
| `manual` | set | use explicit `mode` |
| `manual` | not set | default to `dispatcher` |
| any | `agent`, controller is **codex** | `dispatcher` — see "agent is Claude-only" below |

Model comparison is exact-string against the normalized session identity (see "Session identity"). Unrecognized `session.model` values pass through; the tool validates them.

Example explicit override:

```yaml
review:
  tool: claude
  model: claude-opus-4-6
  mode: agent
```

## Non-interactive invariant

Subprocesses (modes `agent` and `dispatcher`) run with no interactive channel to the user. The user only sees output AFTER the subprocess completes. Dispatched phases therefore MUST:

- Never prompt the user. No "yes/no", no "confirm", no "press enter".
- Never block on stdin past the initial prompt that the controller pipes in.
- Emit a final, self-contained answer (success or structured failure — see "Escalation output format" below) and exit.

The controller is the only place where user-facing questions live. If a dispatched phase needs human input mid-task, it must terminate with an escalation block; the controller then surfaces it to the user and decides what to do.

Inline mode does not have this restriction — it runs in the controller's session, which is the user's session. Questions there reach the user normally.

## Controller-only invariant

**If you are reading this file as a dispatched phase, stop applying it and go back to your packet.** This document is the controller's contract. There is exactly one controller per task — the session the user is talking to. A dispatched phase (`agent` or `dispatcher` mode) is never a controller, regardless of what it can reach on disk.

A dispatched phase MUST NOT:

- dispatch another phase — no `rescue`, no `review`, no re-`execute`, for any reason including its own failure;
- invoke the `claude` or `codex` bridge skills, or shell out to `claude -p` / `codex exec` for a "second opinion";
- read `.nucleos/workflow/dispatch.md`, the `orchestrate*` skills, or another phase's skill and act on them.

When a dispatched phase fails, is blocked, or thinks the task needs rescuing, its only move is to emit the `## Escalation` block below and exit non-zero. The controller decides what happens next; that decision is not delegable downward.

This needs stating because it is reachable by accident: `AGENTS.md` and `CLAUDE.md` point every agent at `.nucleos/workflow/workflow.md`, so a codex executor doing ordinary orientation reads its way into the controller's instructions and finds "on failure, dispatch rescue" written as if addressed to it. It is not. A dispatched executor that follows that line spawns a phase the controller never asked for, outside its timeout, unrecorded, and — when the target is `claude` — visible to the user as an unexplained session in their own session list.

## The phase skill, and the skill that is not one

Each phase has exactly one skill, named after the phase:

| Phase | Skill |
|---|---|
| `plan` | `planner` |
| `execute` | `executor` |
| `review` | `reviewer` |
| `rescue` | `rescue` |
| `maintenance` | `maintenance` |
| `bootstrap` | `bootstrap` |

`claude` and `codex` are **not** phase skills. They are named after tools, and they teach a controller how to reach that tool — `subagent_type`, `timeout`, flags, how to read tokens out of the output. A dispatched phase needs none of it and is forbidden from acting on any of it: they are instructions addressed to the controller, which the controller-only invariant would then have to forbid the phase from following. The executor has its own skill, and the bridge stays with the controller.

**Never put the `claude`/`codex` skill body into a dispatched prompt.** Read it yourself.

## Escalation output format

When a dispatched phase cannot proceed (ambiguous packet, missing context, blocked by environment, needs a decision), it MUST emit this block as its final output and exit non-zero:

```
## Escalation
reason: <one-line cause>
needed: <what the user / orchestrator must decide or provide>
suggested-next: <one concrete option, or "none">
partial-output: <what was produced before stopping, or "none">
```

The controller treats any non-zero exit with an `## Escalation` block as a structured failure (not a crash) and routes it to the user with the four fields verbatim. No `## Escalation` block + non-zero exit = treat as crash / sandbox / tooling failure.

## Dispatcher rules

The subprocess does not inherit the controller's session. Do not send bare instructions like "Use the planner skill." Always inline the full prompt — skill instructions, task context, and packet schema — into the delegated input.

**Prompt-passing convention.** Shell quoting breaks on packet content containing quotes, backticks, or other special characters. Always write the prompt to a temp file and pipe via stdin. Never use positional arguments or heredocs.

**Timeout convention.** Every subprocess call MUST have a wall-clock timeout — without it, a hung subprocess freezes the whole pipeline silently. Default: 600s (10 min) for `plan` / `review` / `maintenance` / `rescue` / `bootstrap`; 1800s (30 min) for `execute`. Override per phase via `<phase>.timeout_seconds` in `<state_dir>/models.yaml` if a task genuinely needs longer.

Wrap each dispatcher command with a timeout (`timeout <N>s <cmd>` on POSIX shells, an equivalent wrapper elsewhere). Timeout exit (124 on POSIX `timeout`) is treated as a freeze: dispatch rescue, report to user, stop.

**Synchronous-call invariant.** Dispatcher subprocesses MUST be launched synchronously (the controller blocks on stdout/stderr until the process exits or the timeout fires). NEVER use background-launch flags — e.g. Claude Code's `Bash` tool with `run_in_background: true`, shell `&`, `nohup`, `Start-Job`, `Start-Process` without `-Wait`. Background launches return immediately with metadata instead of the phase's actual output, breaking the Handoff check and the metric record (no exit code, no duration, no stdout to parse). If a phase is genuinely too long for the default timeout, raise `<phase>.timeout_seconds` in `<state_dir>/models.yaml`; do not work around it with background mode.

**Mode: inline.** Run the phase logic in the current session. Assemble the same full prompt you would send to a subprocess, then follow it directly. If `mode: inline` is set explicitly and the resolved session model (the running model) differs from `phase.model`, warn first:

> "Warning: Phase `<phase>` is set to inline but the running model (`<session.model>`) differs from phase model (`<phase.model>`). Running in the running model — recorded as `<session.model>`, not `<phase.model>`."

An inline phase is always recorded as the session identity's running model, never as `phase.model` — the running model is what actually executed it.

### Mode: agent (in-process)

The controller delegates via the Claude Code Task tool — an in-process subagent that shares the controller's sandbox but runs in its own context window.

**`agent` is Claude-only, and a codex controller degrades it to `dispatcher` rather than failing.** The Task tool is Claude Code's; codex has no counterpart, so a codex controller meeting `mode: agent` has nothing to call. The degradation is well defined because both modes specify the same wall: a read-only phase dispatched to codex takes `--sandbox read-only`, a write-capable one takes the bypass and is governed by its packet's `## Boundaries` — exactly as the `dispatcher` section below sets out. What is lost is the in-process context window and the absence of a timeout ceiling, so a codex controller inherits the Bash tool's timeout cap that `agent` mode exists to escape. That cost is the reason to keep `agent` wherever the controller IS Claude, and the reason a long codex-controlled phase needs its `timeout_seconds` raised deliberately. No subprocess, no temp file, no `timeout` wrapper, no `rm`.

Dispatch: call the Task tool with `subagent_type="<phase cockpit>"` and `model=<family>`, where `<family>` is the `sonnet | opus | haiku` prefix of `<phase.model>` (intra-family version differences collapse — e.g. `claude-sonnet-4-6` and `claude-sonnet-4-5-20241022` both resolve to `sonnet`). Exact-version routing requires `mode: dispatcher`.

**Pass a pointer to the archived prompt, not a retyped copy of it.** Assemble with `python <bundle_dir>/scripts/assemble_prompt.py`, then make the `prompt` parameter the dispatch text it prints — which names the archive it just wrote (under `<state_dir>/prompts/`) and tells the phase to read it in full. The script writes the manifest beside the archive (`<archive>.manifest.json`); the controller records the dispatch's metric row (`mcp__nucleos__record`, kind `metric`) with that manifest's values in the payload when the phase returns — nobody types a manifest value by hand.

The reason: assembling the archive and composing this parameter by hand are two things that must agree, and they diverge (an archive carrying a memory slice that was never sent). A pointer makes the bytes the phase reads and the bytes recorded the same bytes. This does **not** contradict the Dispatcher rule above against bare instructions: that rule exists because a subprocess does not inherit the controller's session, and in `dispatcher` mode the archive IS the stdin file (`--emit dispatcher`), byte-identical by construction. An in-process subagent shares the filesystem and every cockpit has `Read`; and "use the planner skill" leaves the content to discovery, while an exact path pins it to bytes.

The Task tool returns the phase output directly. Apply the same exit/escalation handling as for subprocess modes (escalation block → structured failure; empty output → silent failure).

#### The phase cockpit

| Phase | `subagent_type` | Tools it gets |
|---|---|---|
| `plan` | `wf-planner` | Read, Glob, Grep, Write |
| `execute` | `wf-executor` | Read, Glob, Grep, Edit, Write, Bash |
| `review` | `wf-reviewer` | Read, Glob, Grep, Bash |
| `rescue` | `wf-rescue` | Read, Glob, Grep |
| `maintenance` | `wf-maintenance` | Read, Glob, Grep, Edit, Write, Bash |
| `bootstrap` | `wf-bootstrap` | Read, Glob, Grep, Edit, Write, Bash |

The definitions live in `.claude/agents/wf-<phase>.md`; the `tools:` line in each is the allowlist the harness actually enforces.

**Never dispatch a phase as `general-purpose`.** That subagent type carries every tool there is, so a `wf-rescue` sent through it gets `Edit`, `Write` and `Bash`, and a `wf-reviewer` gets `Edit` and `Write` — which is exactly the wall these phases exist behind. A skill's `tools:` frontmatter is never enforced (it is the **agent** frontmatter field; a skill body arrives at a subagent as prompt text with its frontmatter stripped), so without the cockpit the allowlist is prose describing a wall that is not there.

If `subagent_type="wf-<phase>"` is rejected as unknown, the cockpit file is missing from this checkout — STOP and tell the user the workflow bundle is not materialized here (the daemon materializes it into every checkout and worktree of a rostered project). Do NOT fall back to `general-purpose`: a phase running with the wrong walls is worse than a phase that did not run, because nothing downstream can tell.

`subagent_type` is fixed by the phase; `model` is the only thing routing decides. A phase does not choose its own cockpit.

### Mode: dispatcher (subprocess)

Wrap every subprocess call with `timeout <T>s` where `<T>` is `<phase>.timeout_seconds` from `<state_dir>/models.yaml`, defaulting to 600s for plan/review/maintenance/rescue/bootstrap and 1800s for execute. After dispatching, `rm -f /tmp/phase-<phase>-prompt.md`.

- **Target = `claude`** (`dispatcher` when `<phase>.tool == claude`):
  ```bash
  timeout <T>s sh -c 'cat /tmp/phase-<phase>-prompt.md | claude -p --exclude-dynamic-system-prompt-sections "Execute the attached <phase> phase exactly. Return only the phase result. If you cannot proceed, emit the Escalation output format and exit non-zero." --model <phase.model> [--effort <phase.reasoning_effort>] --allowedTools <the phase cockpit's tool list, space-separated> --output-format json 2>/tmp/phase-<phase>-stderr.log'
  ```

  **`--allowedTools` is not optional and is not cosmetic.** A subprocess has no agent file behind it — `subagent_type` is an in-process concept — so without this flag the dispatched phase gets the CLI's full default tool set and the cockpit exists only in `agent` mode. Take the list verbatim from the "The phase cockpit" table above: `review` dispatches as `--allowedTools Read Glob Grep Bash`, `rescue` as `--allowedTools Read Glob Grep`. The two modes must give a phase the same walls, or which mode routing happened to pick becomes a silent difference in what the phase could do.

  The controller reads `.result` from the JSON object as the phase output (instead of raw stdout) and maps `.usage.input_tokens` -> `tokens_in`, `.usage.output_tokens` -> `tokens_out`, `.usage.cache_read_input_tokens` -> `cache_read`, and `.usage.cache_creation_input_tokens` -> `cache_creation` in the metric payload. After dispatching, `rm -f /tmp/phase-<phase>-stderr.log` alongside the prompt file.

  **Do NOT add `--bare`.** It also skips **keychain reads**: under `--bare`, Anthropic auth is *strictly* `ANTHROPIC_API_KEY` or an `apiKeyHelper` passed via `--settings` — OAuth and the keychain are never read. On a subscription/OAuth login with no API key exported, every `--bare` dispatch dies instantly with `Not logged in · Please run /login`, produces no output, and leaves an orphan session in the user's session list. Only use `--bare` if `ANTHROPIC_API_KEY` is set in the dispatching environment — and note that it bills the API separately from the subscription.

  Consequence of dropping it: the dispatched phase does load CLAUDE.md, hooks, and plugins. That is accepted for CONTEXT only: `--allowedTools` above still holds the tool wall, and the two are independent. A wide context is a cost; a wide tool set is a hole.

  Send stderr to a log file, never `2>/dev/null`. Suppressing it hides auth failures: the controller would see only an empty stdout and could not tell "not logged in" from "the model returned nothing".

  `--exclude-dynamic-system-prompt-sections` moves per-machine cwd/env/git/memory out of the system prompt into the first user message; the stable prefix hits the Anthropic prompt cache (5-min TTL) on repeat calls. `<phase.reasoning_effort>` ∈ {`low`, `medium`, `high`, `xhigh`, `max`} from `<state_dir>/models.yaml`; omit the flag entirely when the field is absent (claude default applies). For the `execute` phase, the controller's resolved speed decision (`reasoning_effort`) takes precedence over `<state_dir>/models.yaml`; see the orchestrate skill's `## Speed`.

- **Target = `codex`** (`dispatcher` mode when `<phase>.tool == codex`):
  ```bash
  timeout <T>s sh -c 'cat /tmp/phase-<phase>-prompt.md | codex exec --skip-git-repo-check -m <phase.model> --config model_reasoning_effort="<phase.reasoning_effort>" --sandbox read-only -C <absolute project path> 2>/tmp/phase-<phase>-stderr.log'
  ```
  The controller extracts the total from `/tmp/phase-<phase>-stderr.log` with `grep -A1 'tokens used' <log> | tail -1 | tr -cd '0-9'`, records it as `tokens_out`, then runs `rm -f /tmp/phase-<phase>-stderr.log`. Codex prints a single total, so only `tokens_out` is populated (`tokens_in` stays null).

  Do not "simplify" this to a same-line regex such as `tokens used[:\s]+([\d,]+)`: codex prints the label on its own line and the number on the NEXT one, with a SPACE as the thousands separator (`tokens used` / `51 272`). A line-oriented tool never matches (null `tokens_out`); a multiline regex stops at the space and captures `51` for a phase that burned 51,272 — a plausible-looking undercount, and the metric rows are what the adaptive selector scores phases by. Digit-stripping the following line survives the layout, the space separator and a comma separator alike.

  `<phase.reasoning_effort>` ∈ {`xhigh`, `high`, `medium`, `low`} from `<state_dir>/models.yaml`; default `medium` if absent. Codex does NOT accept `max` — only claude does. For the `execute` phase, the controller's resolved speed decision (`reasoning_effort`) takes precedence over `<state_dir>/models.yaml`; see the orchestrate skill's `## Speed`.

  **Evidence handoff.** Before accepting an `execute` Handoff — from any tool — the controller MUST run `python <bundle_dir>/scripts/evidence_gate.py <filled packet>` and require success. Claude Code hooks do not fire inside a Codex subprocess, so for Codex this explicit controller check is the whole mechanism; for a Claude `agent` execute it is the same check. No hook stands behind it on either tool: this controller check is the whole mechanism.

  **Plan pin.** When the planner persisted its plan under `<state_dir>/plans/`, the controller hashes that file's raw bytes (`sha256sum <path>`) at the moment it accepts the plan and proceeds to execute, and keeps the path and hash for the review dispatch. The reviewer copies both, verbatim, into `Plan path:` and `Plan sha256:`; with no persisted plan both are `none`. The hash is taken by the controller, never the reviewer: a reviewer that hashed the file itself would pin whatever the plan had become, which is the one thing the pin exists to catch.

  **Review handoff.** Before accepting a `review` verdict -- any verdict, from any tool -- the controller MUST pipe the filled review packet into `python <bundle_dir>/scripts/review_gate.py` (it reads stdin) and require exit 0. The gate can refuse but never approve: exit 0 means the packet does not contradict itself, not that the review was good. On non-zero the verdict is NOT recorded. A refusal that the plan has moved since approval is never retried -- the execution under review implemented a plan that no longer exists, so STOP and report the gate's output. Any other refusal (approve beside its own findings, a missing `Limitations:`, an unfilled or malformed packet) re-dispatches `review` once with the gate's stderr appended; a second non-zero exit STOPs and reports. No hook stands behind this gate on either tool, so this controller check is the whole mechanism. Per `workflow.md` Rule 6, the verdict is also accepted only once the final verify is green.

  **Codex has no per-tool allowlist, but it does have a wall — use it.** Read-only phases (`plan`, `review`, `rescue`) MUST be dispatched with `--sandbox read-only`, which is a real mechanical guarantee that the phase cannot write, and is the property those cockpits exist for. It is not the same as `--allowedTools`: the model may still *reach for* a write and be refused rather than never being offered one. But "cannot act" — the thing that makes a reviewer independent and a rescue a diagnosis — is enforced.

  Write-capable phases (`execute`, `red`, `green`, `refactor`, `maintenance`, `bootstrap`) keep `--dangerously-bypass-approvals-and-sandbox`, and for those the wall is the packet's `## Boundaries` and nothing below it. Say exactly that when reporting a write-capable codex phase, rather than implying it ran walled. Omitting the bypass flag is NOT the same as asking for `read-only` (the default is not read-only), and without the bypass codex's own sandbox raises approval prompts that stall the subprocess silently. **There is exactly one list, above: write-capable is `execute`, `red`, `green`, `refactor`, `maintenance`, `bootstrap`; everything else takes `--sandbox read-only`.** A rule with two lists has no rule.

  **Controller sandbox bypass.** The bypass flag only disables *codex's* sandbox. When the controller is Claude Code with its own sandbox enabled, the dispatched codex subprocess still runs inside the controller's sandbox, which blocks codex's writes/network even though the command is permission-allowed. A write-capable codex dispatch MUST therefore run outside the controller's sandbox: issue the dispatch `Bash` call with `dangerouslyDisableSandbox: true` (which requires `sandbox.allowUnsandboxedCommands` to stay `true`, the default).

Subprocess exit handling lives in the dispatch error table below (exit 0, non-zero ± `## Escalation`, exit 124 timeout, empty-output exit 0).

## Cache-stable prompt layout

When assembling a dispatched-phase prompt, order the sections so that content shared across sequential phases of the same task forms a byte-identical prefix, maximizing Anthropic prompt-cache hits (5-min TTL). The order and the per-phase allowed sections are data in `.nucleos/workflow/prompt-contract.yaml`.

**Stable prefix** (same across phases of one task — order matters):

1. Phase skill body (from discovery path)
2. Packet schema (from `.nucleos/workflow/packets/`)
3. `project.yaml` header fields

**Volatile suffix** (changes per phase or per dispatch):

4. Task / current objective
5. Memory slice (filtered by planner tags)
6. Prior-phase artifacts (Handoff output, reviewer findings, etc.)

Sequential phases of the same task MUST emit a byte-identical prefix; volatile content goes at the end.

## Resume rule

When resuming an executor session (`codex exec resume --last`), pass only the prompt. Never pass config flags (`-m`, `--config`, `--sandbox`, `--full-auto`) on resume — they will be rejected or ignored inconsistently. The same applies to `--dangerously-bypass-approvals-and-sandbox`: a resume cannot carry it, so a resumed write-capable run stalls on codex's sandbox approvals. A review send-back (which must edit files) therefore uses a fresh write-capable `execute` dispatch — the full dispatcher command with the bypass flag — not a resume.

## Dispatch error table

Errors raised by the dispatch mechanism itself, before any phase logic runs. Pipeline-specific errors (planner output shape, Handoff completeness, reviewer verdicts) live in the orchestrate skill.

| Situation | Action |
|---|---|
| `<state_dir>/models.yaml` or `project.yaml` missing, before bootstrap | Run bootstrap through the `wf-bootstrap` cockpit on the session's own model (orchestrate, "Pre-flight checks"): bootstrap writes both files, so there is no configured tool to read yet. The one exception to the never-substitute rule. |
| `<state_dir>/models.yaml` still missing after bootstrap | STOP — tell the user bootstrap did not write it, and show its Handoff |
| `project_name` in `project.yaml` is `unknown` after bootstrap | STOP — tell user to run bootstrap skill |
| `executor` skill missing from the controller's discovery path | STOP — tell the user the workflow bundle is not materialized in this checkout. Do NOT substitute the `claude`/`codex` bridge skill: it is the controller's invocation manual, not the executor's instructions. |
| Bridge skill for the configured `<execute.tool>` missing | STOP — same wording. The controller cannot dispatch a tool it has no invocation manual for. |
| Tool from `models.yaml` unavailable | STOP — tell user to update `<state_dir>/models.yaml`. Never execute in-context as fallback. |
| `dispatch_mode: auto` but `session` block missing or partial | Resolve the session identity from the controller's runtime (see "Session identity") and warn: "`<state_dir>/models.yaml` has no complete `session` block; using the running tool/model for routing. Add `session.tool`/`session.model` to silence this." Only STOP if the runtime identity is also unavailable. |
| `dispatch_mode` has an unrecognized value | STOP — "`dispatch_mode` must be `auto` or `manual`. Got: `<value>`." |
| `session.tool` has an unrecognized value | STOP — "`session.tool` must be one of: `claude`, `codex`. Got: `<value>`." |
| `mode: inline` override where session model differs from phase model | Warn (see above), proceed |
| `dispatch_mode` field absent | Treat as `manual`. All phases default to `dispatcher` unless an explicit per-phase `mode` is set. |
| Subprocess hit timeout (exit 124 / wrapper kill) | Treat as freeze. Dispatch rescue with timeout context, report to user, stop. Never auto-retry the same command — fix the timeout or the upstream cause first. |
| Non-zero exit with `## Escalation` block | Structured escalation. Surface the four fields to the user verbatim; do not retry the phase blindly. |
| Non-zero exit without `## Escalation` block | Treat as crash / sandbox / tooling failure. Follow the per-phase error policy. |
| Subprocess produced empty output and exit 0 | Treat as silent failure. Report to user and stop — do not assume success. |
| `auto_select.enabled: true` but `## Selected models` block missing in planner output | STOP — "invalid planner output: Selected models block missing" |
| `auto_select.enabled: true` but `## Selected models` block malformed | STOP — "invalid planner output: Selected models block malformed: <phase or 'header'>" |
| Planner-selected tool not locally available | STOP — "auto-selected tool unavailable: <tool> for phase <phase>; fix via `<state_dir>/models.yaml` fallback or install the tool". Never silently fall back to `models.yaml`. |
