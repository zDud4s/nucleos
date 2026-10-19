---
name: claude
description: "Use when a workflow phase resolves to tool `claude` and must be dispatched — the controller-side counterpart of the `codex` skill. Also for ad-hoc delegation to a second Claude context when a task needs an independent read."
---

<!-- Cockpit: this skill runs in the controller's own session, under the user's permissions
     and with the user watching, so it carries no tool wall — and its frontmatter must not
     pretend otherwise. A skill's `tools:` field is inert (`tools:` belongs to agent files),
     and the walls in this pipeline are on the dispatched phases: `.claude/agents/wf-*.md`. -->

# Claude Skill Guide

Symmetric counterpart of the `codex` skill, and deliberately NOT the same file as
`~/.agents/skills/claude/SKILL.md`. That one teaches Codex how to reach Claude across a
process boundary, so it is all about `claude -p`. This one is read by a controller that
**already is** Claude, whose normal route to another Claude is the in-process Task tool.
The two are the bridge skills `.nucleos/workflow/workflow.md` marks as intentionally divergent.

## Invocation modes

- **Controller (orchestrator-driven).** A phase in `<state_dir>/models.yaml` has `tool: claude` and
  the controller is dispatching it. See "## Controller mode".
- **Direct (user-driven).** The user asked for a second Claude context explicitly. See
  "## Direct mode".

## Controller mode

**Routing is not this skill's decision.** `.nucleos/workflow/dispatch.md` resolves the mode from
the session identity, and all three land here:

| Situation | Mode | What runs |
|---|---|---|
| `phase.model` == running model | `inline` | the phase, in this session |
| `phase.model` != running model | `agent` | the Task tool, in-process |
| explicit `mode: dispatcher` | `dispatcher` | `claude -p` subprocess |

### Agent mode — the normal one here

Where a project authenticates by OAuth and exports no `ANTHROPIC_API_KEY`, the subprocess
path cannot log in: it dies instantly and leaves an orphan "Not logged in" session in the
user's own session list. Such a project's `<state_dir>/models.yaml` carries an explicit
`mode: agent` on its claude-tool phases, and that is the normal route.

1. Call the Task tool with `subagent_type="<phase cockpit>"` and `model=<family>`. The
   cockpit is `wf-planner` / `wf-executor` / `wf-reviewer` / `wf-rescue` / `wf-maintenance` /
   `wf-bootstrap` — the table lives in dispatch.md under "The phase
   cockpit", and the `tools:` line in `.claude/agents/wf-<phase>.md` is the allowlist the
   harness enforces. **Never `general-purpose`**: it carries every tool, which hands `Edit`
   and `Write` to a reviewer and a rescue. `<family>` is the `opus | sonnet | haiku` prefix of
   `phase.model`. Intra-family versions collapse — `claude-opus-4-8` and `claude-opus-5` both
   route as `opus`. Exact-version pinning is not available in this mode; it needs
   `mode: dispatcher`.
2. Pass the whole assembled prompt as `prompt`. Assemble it in the cache-stable order from
   dispatch.md: phase skill body, packet schema, `project.yaml`, then task, memory slice,
   prior-phase artifacts.
3. No temp file, no `timeout` wrapper, no `rm` — those belong to the subprocess path only.
4. The Task tool returns the phase output directly. Apply the same exit handling as any
   other mode: an `## Escalation` block is a structured failure; empty output is a silent
   failure, never a success.
5. In the metric record (`mcp__nucleos__record`, `kind: metric`), leave `tokens_in`, `tokens_out`, `cache_read` and `cache_creation`
   null. The Task tool returns no token count, and inventing one poisons the adaptive
   scorer that reads the recorded metrics.

### Dispatcher mode

The exact command — flags, `timeout <T>s`, the stderr log path, and how the controller reads
`.result` and the four `.usage` fields out of `--output-format json` — lives in
`.nucleos/workflow/dispatch.md` under "Target = `claude`". **Follow it verbatim; do not paraphrase
it here.** Paraphrase drift is what put `2>/dev/null` and a missing `--output-format json`
into the Codex-side copy of this skill, where they still are.

Two things that file gets wrong and that must not be copied back:

- **Never `2>/dev/null`.** stderr goes to a log file. Suppressing it is precisely what hid
  the auth failure above — the controller saw an empty stdout and could not tell "not logged
  in" from "the model returned nothing".
- **Never `--bare`.** It reads like the right flag and it also skips keychain reads, so on an
  OAuth login it dies instantly and still leaves an orphan session behind.

### Both dispatched modes

- **Non-interactive invariant.** A dispatched phase never prompts the user — no
  `AskUserQuestion`, no confirmation, no blocking on stdin past the piped prompt. The
  controller is the only place user-facing questions live.
- **One-controller-per-node invariant** (dispatch.md, "One controller per node"). A dispatched
  phase never dispatches another phase, never invokes the `claude`/`codex` bridge skills for a
  second opinion, and never acts on `dispatch.md` or an `orchestrate*` skill it happens to read.
  The one exception is a dispatched sub-orchestrator node (`orchestrate_node`, `wf-orchestrator`):
  it dispatches only its direct children, per the `orchestrate-node` skill. Any other phase's only
  move when blocked is the `## Escalation` block, then exit non-zero.
- On success, emit the work followed by a complete `## Handoff` per `.nucleos/workflow/packets/execute.md`
  — `Validation evidence` is one `$ cmd` / `exit:` / `tail: <last 5 lines>` block per
  validation command; executors run no tests, and the test evidence is the controller's
  `verify` result (workflow.md Rule 3).

| Rationalization | Reality |
|---|---|
| "the phase says `tool: claude` and I am claude, so I can just do it myself" | Only `inline` runs in this session, and only when the models match; otherwise dispatch. Substituting your model for a configured phase breaks the dispatch contract and records the wrong model in metrics. |
| "`claude -p` is more explicit than the Task tool" | Under OAuth-only login it cannot authenticate. It exits instantly, produces no output, and leaves an orphan session the user did not start. |
| "stderr is noise, send it to /dev/null" | stderr is where "Not logged in" appears. Suppressed, an auth failure is indistinguishable from an empty answer. |
| "the subagent hit a blocker, I will dispatch rescue from inside it" | A dispatched phase is never a controller. It emits `## Escalation` and exits; the controller decides. |
| "`wf-reviewer` is not a known subagent type, I will use `general-purpose`" | The cockpit file is missing (the daemon materializes `.claude/agents/` from the bundle; a checkout it has not materialized into has none). STOP and say so. A phase run with every tool is worse than one not run, because nothing downstream can tell. |

## Direct mode

The user asked for a second Claude context — an independent read, a plan reviewed by someone
who did not write it, a large area summarised without spending this window on it.

Use the Task tool. Pick `subagent_type` by the job (`Explore` for read-only search,
`general-purpose` otherwise) and offer models from the `claude` catalog in `<state_dir>/models.yaml`.
Say what you dispatched and why before you do it.

Do not delegate work you can do directly — file edits, running a command, reading three
files. A subagent costs a whole context window to set up and returns only text.

### Treat the answer as a colleague's, not an authority's

Read the output as advisory input. It has its own cutoffs and did not see this conversation.
When it conflicts with something you verified, investigate before accepting it, and when the
disagreement is real, put both readings to the user rather than picking silently.

## Error handling

- `claude` not on PATH (dispatcher mode only) → STOP; tell the user to install the CLI or
  point the phase at an available tool in `<state_dir>/models.yaml`. Never fall back to running the
  phase in this session.
- Non-zero exit → report the error output before doing anything else. Do not loop on
  failures; report and ask.
