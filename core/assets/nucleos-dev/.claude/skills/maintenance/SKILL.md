---
name: maintenance
description: Maintain project-specific workflow metadata (project.yaml, memory.md, decisions.md) after bootstrap, CI changes, restructuring, or refresh requests—never alter the immutable core; also use when asked to consolidate, compact, or tighten memory/tokens.
---

<!-- Cockpit: `.claude/agents/wf-maintenance.md`. Its `tools:` line is the wall the harness
     enforces; it is not repeated here, because a skill's `tools:` frontmatter is inert. -->

Keep the project-specific workflow layer accurate without degrading the workflow core.

## Prerequisite

If `<state_dir>/project.yaml` has `project_name: unknown` and `stack` empty, STOP: "Run the bootstrap skill first. The project metadata is empty."

## Core principle

Workflow core is immutable by default; maintain only mutable project layer unless explicitly asked for core change.

Immutable core: root AGENTS.md (including its managed workflow block), escalation policy, `.nucleos/workflow/*.md`, `.nucleos/workflow/packets/*.md`, `.claude/skills/*/SKILL.md`, `.claude/agents/wf-*.md`, `.agents/skills/*/SKILL.md`, safety boundaries.

Mutable layer, all in `<state_dir>` (read from `.nucleos/workflow/project.json`; outside the repository tree, so it never enters git): `project.yaml`, `models.yaml`, `memory.md`, `decisions.md`, `memory-archive.md`; plus local AGENTS.md inside subdirectories.

## When to use

- After bootstrap or new repo facts
- After build/package/CI changes or fragile modules
- After stale-metadata failures or review memory updates
- On "refresh" / "maintain" / "update the workflow"
- When `<state_dir>/memory.md` > consolidation threshold (`memory_tuning`, default 150) or has contradictions → consolidation pass
- When entries >25 words, yaml leaves carry prose, or files exceed budget → density pass

## Responsibilities

1. Re-scan repo structure when relevant.
2. Refresh detected commands when scripts/PM/entrypoints changed.
3. Update ownership, important dirs, risky areas, do-not-touch zones in `<state_dir>/project.yaml`.
4. Append operational discoveries to `<state_dir>/memory.md`.
5. Record stable architectural decisions in `<state_dir>/decisions.md` only with strong evidence.
6. Scan TODOs — search the diffs since the last maintenance pass for `TODO|FIXME|XXX`, and capture `[next]` lines in memory and `## Follow-ups` from the latest plan Handoff. Only SUGGEST resolving one; never close an item on your own authority. There is no separate TODO file or ledger.
7. Tighten local subdirectory AGENTS.md when structure clearly changed.
8. Remove disproven assumptions.
9. Keep the project layer concise, factual, operational — phases pay per line.
10. Run a density pass when phrasing is verbose or files exceed budget.
11. Inspect the rework rows recorded for this project's tasks, when the controller hands them to you. If one rework class appears at
    least twice for the same task, MUST propose one concrete `system_fix`: a skill line, a
    packet clause, or a gate. It is FORBIDDEN to propose a product feature instead; this
    mechanism exists to patch the workflow, not to turn repeated rework into feature work.
    <!-- F5-OPEN: rework rows now live in the daemon's store (`mcp__nucleos__record`, kind rework); no read tool is specified for a maintenance phase, so the controller must pass the rows in the prompt. -->

## Never do without explicit user approval

- Rewrite root workflow roles
- Change the core escalation policy
- Loosen safety boundaries
- Rewrite planner/reviewer/rescue logic
- Silently broaden project scope
- Invent commands as confirmed facts

## File-specific rules

`<state_dir>/project.yaml`: structured + compact; evidence-backed; assumptions explicit. **Budget ~800 tokens (~3200 chars).** Leaves ≤30 chars; no prose, decoration, or empty/null placeholders.

`<state_dir>/memory.md`: format `- YYYY-MM-DD [topic] fact`. **`[topic]` comes from the closed
`## Topic vocabulary` block at the top of that file, never from your own judgement** — the
orchestrator greps the file by tag to build each dispatched phase's memory slice, so a tag
nobody listed matches nothing and silently starves the phase that needed it. Writing a new
canonical topic means editing that block first, in the same pass, with a reason. Drop stale
when disproven. Not a changelog. Consolidate on trigger. **Budget ~2000 tokens (~8000 chars, ~150 dense lines).** ≤15 words/entry; one fact/line; paths/commands in backticks.

`<state_dir>/decisions.md`: only stable decisions, each with a why. No temporary choices.

Local subdirectory AGENTS.md: narrower than root; only local constraints that truly help execution.

## Consolidation pass (memory)

Append-only memory accumulates duplicates, contradictions, stale facts. Trigger on ANY of:

1. **Size** — projected lines > current threshold (`memory_tuning.consolidation_threshold_lines` in `<state_dir>/project.yaml`, default 150). Consolidate BEFORE appending.
2. **Contradiction** — new update contradicts an existing entry. Reconcile at once.
3. **Explicit** — user/orchestrator requests "consolidate memory" / "compact memory".

### Procedure

1. Parse entries; anything not matching `- YYYY-MM-DD [topic] fact` is "undated".
2. **Deduplicate.** Same fact → keep newer date + clearer wording; merge detail into one line.
3. **Merge related.** Group by `[topic]`; collapse closely-related lines when no operational value is lost.
   Consolidation is also when the **legacy** tags listed in the vocabulary block get folded into
   their canonical target (`config → env`, `runner → core`, …). Rewrite the tag, keep the fact and
   the date, and delete the legacy name from the block once its last entry is gone — a legacy list
   that never shrinks is a second vocabulary.
4. **Contradictions** (same topic, incompatible content):
   - dated vs undated → keep dated, drop undated
   - both dated → keep newer; surface in output
   - both undated → surface for human; don't silently pick
5. **Archive obsolete.** Subject gone from repo, or point-in-time claim now stale → MOVE to `<state_dir>/memory-archive.md` (append ` (archived: <today> <reason>)`), NOT delete. List each.
6. **Re-sort.** By topic alphabetically, then date within topic.
7. **Cap.** If still > threshold, trim oldest **non-pinned** entries within each topic first — never a pinned entry for size alone, never conflict markers. **Pinned** = cross-cutting governance topics, exempt from this size-cap and removable only via step 5 (genuinely disproven/obsolete): `[git]`, `[boundaries]`, `[process]`, `[security]`, `[workflow]`, `[memory]`, `[dispatch]`.

### Output

Report: entries + lines before/after, deduplicated (examples), merged, dropped (line + reason), archived count, conflicts surfaced, final lines, compaction ratio (`before/after`, 2dp), threshold update.

Archive is human-inspection-only; no phase loads it. Do NOT silently rewrite facts — report every removal/merge.

### Adaptive threshold

After every pass, recompute `memory_tuning.consolidation_threshold_lines` from the smoothed compaction ratio.

Inputs in `memory_tuning`: `consolidation_threshold_lines`, `floor` (default 50), `ceiling` (default 300), `headroom` (default 20), `last_ratios` (newest-first, max 3).

**Unit, stated because the field name lies.** `consolidation_threshold_lines` and both ratio terms count **entries** — lines matching `- YYYY-MM-DD [topic]` — not physical lines of the file. On 2026-08-31 the file was 237 entries and 286 lines; a pass that mixed the two would compare 286 against a threshold calibrated for entries and trim ~17% of the corpus for nothing. Count entries everywhere in this section, including step 7's cap.

1. **Append ratio.** Push this run's `lines_before/lines_after` to front of `last_ratios`; truncate to 3.
2. **Smooth.** Arithmetic mean. Single ratio → use directly. `lines_after == 0` → ratio = 999.
3. **Adjust:**

| Smoothed ratio | Interpretation | Threshold |
|---|---|---|
| ≥ 1.5 | Bloated; cut ≥33% | × 0.85 |
| 1.2 – 1.49 | Some redundancy | × 0.95 |
| 1.05 – 1.19 | Roughly right | no change |
| < 1.05 | Too tight (barely cut) | × 1.15 |

4. **Clamp.** Round, then raise to at least `lines_after + headroom`, then clamp to `[floor, ceiling]`. Note in output if clamped, and by which bound.
5. **Write back.** Update `consolidation_threshold_lines`, `last_ratios`, `last_consolidated_at` (today).

**The lower clamp is the whole point of step 4, not a detail of it.** Step 7 caps against the OLD threshold, so any pass that needs the cap lands exactly ON it — and the bands above then multiply that same old threshold by a factor < 1, putting the new threshold BELOW the file the pass just produced. The next run's size trigger is then true before anything has changed, and trims valid facts by age to meet a threshold the previous pass could not meet losslessly either. That is a treadmill, not a control loop, and it is measured: the 2026-08-31 pass landed at exactly 220 and wrote 209. A threshold is a trigger, and a trigger guaranteed to be true the moment it is written is broken.

`headroom` rather than a bare `>= lines_after` because the same pass **appends after it consolidates** (trigger 1: "Consolidate BEFORE appending") — 17 entries on 2026-08-31. Landing exactly at the threshold re-arms the trigger on the pass's own output.

If `lines_after + headroom > ceiling`, the ceiling wins and the threshold stays below the file. **Report that explicitly** — it means the corpus no longer fits the ceiling, and choosing between raising the ceiling and accepting age-based trimming is the owner's call, not a silent clamp.

Report `Threshold update: <old> → <new> (ratio <smoothed>)`. If change > ±20% in one pass, prepend a one-line note (first-time tune or one-off cleanup).

## Density pass (token optimization)

Consolidation = **quantity** (dedupe, archive); density = **quality** (terse phrasing). Both files load as phase context — redundant words recur as cost.

Trigger ANY of:
1. **Budget** — `<state_dir>/memory.md` > ~2000 tokens OR `<state_dir>/project.yaml` > ~800 tokens (estimate `chars / 4`).
2. **Phrasing** — memory entry > 25 words; yaml leaf with prose narrative (>30 chars non-name) or `null` / `~` / `""` / `TODO`.
3. **Explicit** — user asks "compact" / "optimize tokens" / "tighten memory" / similar.

### Rewrite rules — memory.md

- Keep format `- YYYY-MM-DD [topic] fact`. Preserve date + topic; rewrite only the fact.
- Drop filler ("we discovered that", "it turns out"), narrative tense ("when X ran, Y happened" → "X → Y"), and redundant subjects ("db configured to use pooling" → "db: pooled").
- Paths/commands/filenames in backticks, not prose.
- One fact per line; split compounds.
- Target ≤15 words; hard cap 25.

### Rewrite rules — project.yaml

- Leaf values ≤30 chars where possible. Multi-word identifiers OK; full sentences NOT.
- Drop empty/null/placeholder leaves (`null`, `~`, `""`, `TODO`, `unknown`) — unless `unknown` is meaningful state (e.g. bootstrap signal).
- Lists: one short item/line, no decoration.
- No commentary in values. Comments via `# ...` only when load-bearing.
- Collapse single-child nested objects up one level when meaning is preserved.

### Procedure

1. Read both files. Compute `chars / 4` as token estimate.
2. Scan verbose entries (memory) + prose/placeholder leaves (yaml). Score: token cost vs operational value.
3. Rewrite high-cost low-density entries in place. Preserve date + topic in memory entries.
4. Drop fields/leaves with no operational value for any phase. List each.
5. Re-estimate. Still over budget? Surface the largest remaining — **never silently truncate**.

### Output

Report tokens before→after (delta + %); entries rewritten (2–3 examples); fields dropped; still-over-budget flagged; untouched (already dense). Every rewrite/drop appears; if meaning changes, surface original and ask.

### Interaction with consolidation

When both trigger on the same run: density first (rewrite reduces what consolidation merges), then consolidation (dedupe collapses now-similar entries). Report both; consolidation ratio uses post-density line counts.

## Stop conditions

- Evidence too weak; change would alter the immutable core; would require product implementation; multiple conflicting interpretations with no clear winner.

On stop, output: what is unclear, what evidence is missing, smallest safe next step.

## Token budget

Maintenance output ≤80 lines.

## Output format

- Scope checked
- Files updated
- Confirmed changes
- Assumptions added / removed
- Unknowns remaining
- Core-change warning (if any)
