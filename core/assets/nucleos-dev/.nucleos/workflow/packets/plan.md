# Planning Packet

<!-- READ-ONLY TEMPLATE. See packets/README.md for the contract. -->

Task ID: <!-- short slug, e.g. fix-login-redirect -->
Task: <!-- one sentence -->
Size: <!-- trivial | small | medium | large -->
Risk level: <!-- low | elevated -->
Risk matches: <!-- matched boundary paths or `none` -->
Problem summary: <!-- 2-3 sentences: what is wrong, why it matters -->
Desired outcome: <!-- observable result, not implementation detail -->
Current behavior: <!-- what happens now, if applicable -->
Relevant files: <!-- exact paths, max 10. If >10, decompose the task. -->
Likely subsystem: <!-- e.g. auth, payments, CLI parser -->
Constraints: <!-- hard limits: do-not-touch zones, API contracts, perf budgets -->
Acceptance criteria: <!-- testable conditions, one per line -->
Tests to add: <!-- planned tests or `none` with reason -->
Risks: <!-- what could go wrong, one per line -->
Smallest safe plan: <!-- 2-5 sentences max -->
Escalation trigger: <!-- condition under which executor must stop -->
Memory candidates: <!-- durable facts worth appending, or `none` -->
Memory tags: <!-- `Memory tags: [tag1, tag2, ...]`. Topics predicted relevant to executing this task, drawn ONLY from the closed `## Topic vocabulary` block in `<state_dir>/memory.md`. At least one; `[]` is invalid. Omit only for `TRIVIAL:` outputs. -->

## Execution packet(s)

<!-- One or more execution packets inline, each using the schema from
     `execute.md`. For `Size: trivial` tasks, emit `TRIVIAL: [one-line
     instruction]` instead of a packet and skip the Selected models block. -->

## Selected models

<!-- Append ONLY when `<state_dir>/models.yaml` has `auto_select.enabled: true`.
     Format (one line per phase, lowercase + `:` + left-aligned):
       execute: tool=<v>  model=<v>  [reasoning_effort=<v>]  reason="<≤120 chars>"
       review:  tool=<v>  model=<v>  [reasoning_effort=<v>]  reason="<≤120 chars>"
       rescue:  tool=<v>  model=<v>  [reasoning_effort=<v>]  reason="<≤120 chars>"
     Empty header (zero phase lines) = "evaluated, no matches"; orchestrator
     falls back to `<state_dir>/models.yaml` for every phase. See `.nucleos/workflow/auto-models.md`
     for the lookup table and planner SKILL "Auto-select output block" for fill rules. -->
