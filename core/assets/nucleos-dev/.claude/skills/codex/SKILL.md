---
name: codex
description: Use when the user asks to run Codex CLI (codex exec, codex resume) or references OpenAI Codex for code analysis, refactoring, or automated editing
---

<!-- Cockpit: this skill runs in the controller's own session, under the user's permissions
     and with the user watching, so it carries no tool wall — and its frontmatter must not
     pretend otherwise. A skill's `tools:` field is inert (`tools:` belongs to agent files),
     and the walls in this pipeline are on the dispatched phases: `.claude/agents/wf-*.md`. -->

# Codex Skill Guide

## Invocation modes

- **Controller (orchestrator-driven).** Prompt arrives via stdin from a controller. You MUST NOT prompt the user (no AskUserQuestion). Use values passed in the prompt verbatim. See "## Controller mode" below.
- **Direct (user-driven).** The user typed "run codex" or invoked you explicitly. Free to ask follow-ups. See "## Direct mode" below.

## Controller mode (called by orchestrator)

The exact invocation — command, flags, `timeout <T>s` wrapper, stderr redirect, and resume — lives in `.nucleos/workflow/dispatch.md` "Target = codex". Follow it verbatim; do not paraphrase it (paraphrase drift is a defect source). Skill-specific deltas:

1. Prompt arrives via stdin from a temp file (never a positional argument). You MUST NOT prompt the user (no AskUserQuestion) — use the values in the prompt verbatim.
2. stderr follows dispatch.md's dispatcher command (`2>/tmp/phase-<phase>-stderr.log`, which the controller parses for `tokens used`). Never `2>/dev/null` in controller mode — it breaks token-metrics capture (recorded with `mcp__nucleos__record`, `kind: metric`).
3. The controller wraps the call in `timeout <T>s` per dispatch.md's Timeout convention — never launch without it.
4. Bypass flag: write-capable phases (`execute`, `rescue`, `maintenance`, `bootstrap`) append `--dangerously-bypass-approvals-and-sandbox`; read-only `plan`/`review` omit it. When the controller (Claude Code) is sandboxed, the dispatch must also run outside the controller's sandbox — see dispatch.md "Controller sandbox bypass".
5. Resume follows dispatch.md's Resume rule (`codex exec resume --last`, no config flags). A review send-back never resumes — it is a fresh write-capable `execute` dispatch.
6. On any blocker emit a final `## Escalation` block and exit non-zero (shape in `.nucleos/workflow/packets/README.md`). On success emit the work followed by a complete `## Handoff` per `.nucleos/workflow/packets/execute.md` (validation evidence = one `$ cmd` / `exit:` / `tail: <last 5 lines>` block per validation command; executors run no tests, per workflow.md Rule 3).

## Direct mode (user-invoked)

### Running a Task

Use `AskUserQuestion` once to ask for model (offer the codex models from the `<state_dir>/models.yaml` catalog) and reasoning effort (`xhigh`, `high`, `medium`, or `low`). Use `--skip-git-repo-check`, `-C <dir>` when needed, and append `2>/dev/null` unless debugging requires stderr.

For write-capable runs where the sandbox blocks Codex, prefer `--dangerously-bypass-approvals-and-sandbox`; ask permission first in direct mode. Pass the prompt via stdin from a temp file when quoting may be fragile.

### Following Up

After every direct `codex` command, use `AskUserQuestion` to confirm next steps, collect clarifications, or decide whether to resume. Restate the chosen model, reasoning effort, and sandbox mode when proposing follow-up actions.

Resume with a temp file: `cat /tmp/codex-resume.md | codex exec resume --last 2>/dev/null && rm -f /tmp/codex-resume.md` (no config flags on resume, per dispatch.md's Resume rule). The resumed session inherits the original model, reasoning effort, and sandbox mode unless the user explicitly changes them.

### Critical Evaluation of Codex Output

Treat Codex as a colleague, not an authority. Push back when output conflicts with known facts; verify recent APIs, model names, library behavior, and best practices against current docs or web research when needed.

When resuming to discuss a disagreement, identify yourself as Claude and include the evidence. Frame the disagreement as a peer review, then let the user decide when ambiguity remains.

### Error Handling

Stop and report failures whenever `codex --version` or `codex exec` exits non-zero. In direct mode, summarize stderr or partial output and use `AskUserQuestion` before retrying or changing flags.

Before high-impact flags such as `--full-auto`, `--sandbox danger-full-access`, or `--skip-git-repo-check`, ask permission unless the user already granted it. Warn clearly when Codex output is partial or uncertain.