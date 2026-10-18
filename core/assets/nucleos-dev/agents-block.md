# >>> AI WORKFLOW MANAGED BLOCK >>>

## AI workflow integration

This repository uses an AI workflow. See `.nucleos/workflow/workflow.md` for full pipeline details.

Roles: configured in `models.yaml` under the project state directory (`state_dir` in `.nucleos/workflow/project.json`)

## Orchestration entry point

For any development task, follow the `orchestrate` skill:
- Claude: invoke "Use the orchestrate skill. Task: <description>" (skill at `.claude/skills/orchestrate/SKILL.md`)
- Codex: read `.agents/skills/orchestrate/SKILL.md` **in this repository**, and only fall back to `~/.agents/skills/orchestrate/SKILL.md` if the repo ships no mirror

Both paths resolve to the same pipeline contract; only the discovery path differs — and the repo's own copy wins. `~/.agents/skills/` is shared by every project on the machine while the workflow is versioned per repository, so a global-only entry point could hand a Codex session another project's copy.

Execution rules (a short copy for readers without hooks; `workflow.md` is the authority):
1. Prefer the smallest correct change; touch only relevant files; do not broaden scope silently.
2. If blocked, stop and report the blocker.
3. Fill the Handoff section before declaring done, with one evidence block (exit code + output tail) per validation command. Do not run tests or build tools: the controller runs the single final verification through the daemon's `verify`.
4. After implementation, report: summary, files changed, validation, risks, assumptions.
5. Never execute delete or mass-removal commands (rm -rf, rmdir, del, Remove-Item, git clean). List required deletions in Handoff `Pending deletions` — the orchestrator executes them after review.
6. Never commit/push history; the orchestrator owns that after review.

# <<< AI WORKFLOW MANAGED BLOCK <<<
