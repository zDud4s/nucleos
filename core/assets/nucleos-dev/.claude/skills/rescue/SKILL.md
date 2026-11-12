---
name: rescue
description: Recover from failed implementation attempts by isolating wrong assumptions and proposing the next narrow experiment. Use when a task has hit repeated failure, unclear regressions, or implementation drift that spans systems — not for a bug localized to a single file.
---

<!-- Cockpit: `.claude/agents/wf-rescue.md`. Its `tools:` line is the wall the harness
     enforces; it is not repeated here, because a skill's `tools:` frontmatter is inert. -->

You are the rescue skill.

## Prerequisite

Check `<state_dir>/project.yaml` (`<state_dir>` is read from `.nucleos/workflow/project.json`). If `project_name` is `unknown` and `stack` is empty, STOP and tell the user: "Run the bootstrap skill first. The project metadata is empty."

## When NOT to use this skill

- **Problem is localized to one file/function**: Debug that single file directly; rescue is for cross-system failures
- **No error logs or failure evidence available**: Fix the missing data first (ask the controller to run the named command and capture its output)
- **Suspected environmental or permission issue**: Check prerequisites, toolchain setup, and credentials; not an implementation failure
- **You have not yet tried the simplest fix**: Attempt obvious solutions before escalating to rescue
- **Already at architectural redesign stage**: If the feature itself is wrong, escalate to review directly

## Rules

1. Do not continue patching blindly.
2. Identify which assumptions are likely wrong — assign confidence: high | medium | low.
3. Separate evidence from speculation.
4. Propose the narrowest next experiment — one small, testable step.
5. If the failure is architectural or cross-cutting and you've exhausted the narrowest experiment, emit `## Escalation` per `.nucleos/workflow/packets/README.md` and exit non-zero. The orchestrator decides whether to re-dispatch to `review` or to stop — rescue does not pick which model handles the next step.

## Token budget

Rescue output ≤40 lines.

## Output format

Emit a filled copy of `.nucleos/workflow/packets/rescue.md` (read-only template — read it for the field list, never Edit/Write it). Fill every field: `Task ID`, `What was attempted`, `What failed`, `Observed evidence`, `Assumptions likely wrong` (each with confidence high | medium | low), `Scope to re-open`, `Safer fallback`, `Next narrow experiment`, `Escalate to review model?` (yes | no + reason). The `Escalate to review model?` value is a recommendation only — the orchestrator may override it and decides whether to re-dispatch to `review` or stop.
