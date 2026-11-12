---
name: skill-authoring
description: Use when creating, editing, or auditing a workflow skill of this bundle (core/assets/nucleos-dev/.claude/skills/*/SKILL.md), before writing or changing any SKILL.md content
---

<!-- Cockpit: this skill runs in the controller's own session, under the user's permissions
     and with the user watching, so it carries no tool wall — and its frontmatter must not
     pretend otherwise. A skill's `tools:` field is inert (`tools:` belongs to agent files),
     and the walls in this pipeline are on the dispatched phases: `.claude/agents/wf-*.md`. -->

# Skill authoring

The skills this governs are the workflow bundle's source, under `core/assets/nucleos-dev/.claude/skills/` in the NucleOS repository. The copies in a project's `.claude/skills/` are materialized from there and are never edited in place.

## Iron Law

> **NO NEW OR EDITED SKILL WITHOUT A RECORDED BASELINE FAILURE**

Before authoring or changing a skill, define a realistic pressure scenario and run it
through a Task subagent **WITHOUT** the proposed skill. Record the excerpt that shows
the failure as RED evidence. Then expose the draft skill, repeat the same scenario,
and record the corrected excerpt as GREEN evidence.

Put both excerpts in the task's Handoff/report. A summary such as "failed before" is
not evidence: preserve the words that demonstrate the behavior. Editing an existing
skill counts; run RED against the **CURRENT** text before changing it. If no baseline
failure is recorded, stop before editing.

## CSO description rules

Keep frontmatter `description` limited to when-to-use conditions and concrete trigger
contexts. Put every workflow step, command, ordering rule, and expected output in the
body. A description that narrates procedure gets followed **INSTEAD of the body**, so
procedural metadata can silently bypass the real instructions.

Audit the description separately: each clause must answer "when should this load?"
Delete or move any clause that answers "what should the agent do next?"

## Building Rationalization -> Reality tables

Use a guard table at consequential decision points where an agent can self-justify
skipping or weakening the workflow. Harvest the exact excuses observed in the RED
baseline and quote them verbatim, one excuse per row. Do not invent generic excuses
when the pressure run supplied concrete language.

Each Reality cell states both the governing rule and the consequence of violating it.
Use this exact house-style header:

| Rationalization | Reality |
|---|---|
| "<exact excuse from RED>" | <rule>; <consequence>. |

Match the established placements: `orchestrate` at the fast path,
`orchestrate-tdd` at the RED gate and REFACTOR phase, and `reviewer` before hard
gates. Keep rows short enough to scan under pressure.

## Bundle integration checklist

- List the skill in the bundle manifest (`bundle.yaml`) with its class: `shared`, `bridge`, or
  `claude-only`. The `.agents/skills/` mirror that Codex reads is generated at packaging from
  `.claude/skills/` and is never authored by hand. An unlisted skill is silently absent from
  that mirror, and a Codex controller reaching for it finds nothing and improvises.
- Declare **no** `tools:` in the frontmatter. That field is inert for skills — it
  belongs to agent files — and a skill body reaches a dispatched phase as prompt text
  with its frontmatter stripped. If the skill is a phase's, its wall is
  `.claude/agents/wf-<phase>.md`, and the tool list there must match dispatch.md's
  `#### The phase cockpit` table. Cockpit consistency checks run at packaging, not in a phase.
- Quote the `description` if it contains a colon. `description: Use when X resolves
  to \`tool: claude\`` is not valid YAML, and a skill whose frontmatter does not parse
  loses its description silently.
- Do not name paths of the old layout, machine-specific tooling, or a project's own test commands in skill text: workflow rules live in `.nucleos/workflow/`, project state in `<state_dir>`.
- Include the RED and GREEN excerpts of your own targeted checks in the Handoff/report. Never run the final verification yourself: the controller runs it once, at the end (workflow.md Rule 3).
