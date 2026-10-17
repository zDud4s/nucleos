# Auto-model selection — decision table

Lookup table consumed by the `planner` skill when `auto_select.enabled: true` in `<state_dir>/models.yaml` (see `workflow.md`, "Layer model").

Input key:  `(phase, size, risk, effective_budget)`
Output:     `(tool, model, reasoning_effort?)` or `no_match` → orchestrator falls back to `<state_dir>/models.yaml`. `reasoning_effort` applies to both claude (`--effort`) and codex (`--config model_reasoning_effort`); `max` is claude-only.

Budget upgrade rule: `effective_budget = configured_budget + 1` (one rung: `low→medium→high`, `high` stays) when `Risk level: elevated` OR `Size: large`. Triage matches against `effective_budget`, not the configured one.

## Table

Rows evaluated in order, first match wins. `*` matches any value.

| phase   | size    | risk     | budget  | tool   | model              | effort |
|---------|---------|----------|---------|--------|--------------------|--------|
| execute | small   | low      | low     | codex  | gpt-5.6-luna       | low    |
| execute | small   | low      | medium  | codex  | gpt-5.6-terra      | medium |
| execute | small   | low      | high    | codex  | gpt-5.6-terra      | high   |
| execute | medium  | low      | low     | codex  | gpt-5.6-terra      | low    |
| execute | medium  | low      | medium  | codex  | gpt-5.6-terra      | medium |
| execute | medium  | low      | high    | codex  | gpt-5.6-terra      | high   |
| execute | large   | low      | low     | codex  | gpt-5.6-terra      | medium |
| execute | large   | low      | medium  | codex  | gpt-5.6-terra      | high   |
| execute | large   | low      | high    | codex  | gpt-5.6-terra      | xhigh  |
| execute | small   | elevated | *       | codex  | gpt-5.6-sol        | high   |
| execute | medium  | elevated | *       | codex  | gpt-5.6-sol        | high   |
| execute | large   | elevated | *       | codex  | gpt-5.6-sol        | xhigh  |
| review  | small   | low      | *       | claude | claude-sonnet-5    | low    |
| review  | medium  | low      | low     | claude | claude-sonnet-5    | low    |
| review  | medium  | low      | medium  | claude | claude-sonnet-5    | high   |
| review  | medium  | low      | high    | claude | claude-opus-4-8    | high   |
| review  | *       | elevated | *       | claude | claude-opus-4-8    | high   |
| review  | large   | *        | *       | claude | claude-opus-4-8    | high   |
| rescue  | *       | *        | *       | claude | claude-opus-4-8    | high   |

## Notes

- `effort` is consumed by both claude (`--effort`) and codex (`--config model_reasoning_effort`). Claude accepts `{low, medium, high, xhigh, max}`; codex accepts `{low, medium, high, xhigh}` — `max` is claude-only. Use `n/a` to keep the tool's default; the planner omits `reasoning_effort` from the emitted line in that case.
- The `trivial` size never reaches downstream phases — the planner emits `TRIVIAL: ...` and stops. No row needed.
- When no row matches a `(phase, size, risk, budget)` tuple, the planner omits the line for that phase and the orchestrator falls back to `<state_dir>/models.yaml`.
- `plan`, `maintenance`, and `bootstrap` are intentionally NOT listed. They are always served from `<state_dir>/models.yaml` regardless of `auto_select.enabled` — auto-selection is reserved for the per-task phases (`execute`, `review`, `rescue`) whose model choice the planner can vary.
- The adaptive scorer penalizes small samples via the Wilson 95% lower bound and uses median, not mean, duration. The static table remains the documented authority for the <5-samples cold-start.
- **`execute`'s model choice is risk-driven, not size-driven** (a deliberate departure from a size-driven table): plans executed in this workflow are written to full task-level detail before execution starts (exact code, exact commands, expected output) — a *large* low-risk task is more of the same mechanical work, not inherently harder to reason about, so it stays on `gpt-5.6-terra` with size/budget only scaling `effort`, not the model. `gpt-5.6-sol` (the flagship codex model) is reserved for `elevated` risk specifically, at any size, since that's where an executor noticing a plan/reality mismatch (stale API, version drift, a subtle correctness issue) actually matters. `gpt-5.6-luna` stays the floor for the genuinely trivial case (`small`/`low`/`low`). This intentionally does not auto-escalate a low-risk task to `sol` just because budget is high — if a specific task genuinely warrants that extra margin, override it explicitly for that task rather than relying on the table to guess.
- Similarly, `claude-sonnet-5` (not `claude-opus-4-8`) now fills the `review | medium | low | medium` row, at higher effort — a real middle rung between "cheap and fast" and "top capability," reserved for the moderate case (medium size, low risk, medium budget) rather than jumping straight to opus the moment budget ticks up from `low`.
