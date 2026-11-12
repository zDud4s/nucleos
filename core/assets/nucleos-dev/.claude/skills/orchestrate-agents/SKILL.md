---
name: orchestrate-agents
description: Use when the user wants to compose, draft, or route a task through their agent catalog as a multi-agent pipeline. Not for code development tasks — those go to the orchestrate skill. Executor for saved pipelines is the run-pipeline skill.
---

<!-- Cockpit: this skill runs in the controller's own session, under the user's permissions
     and with the user watching, so it carries no tool wall — and its frontmatter must not
     pretend otherwise. A skill's `tools:` field is inert (`tools:` belongs to agent files),
     and the walls in this pipeline are on the dispatched phases: `.claude/agents/wf-*.md`. -->

You are the pipeline draft helper. You propose a `<state_dir>/pipelines/<slug>.yaml` draft for a user task, then let the user save it (and optionally run it via the `run-pipeline` skill), save it only, or discard it. The planning happens inline in this session — there is no separate planner skill anymore. You do NOT execute the pipeline yourself; `run-pipeline` is the executor.

## Discovery path convention

Throughout this skill, "discovery path" — the per-runtime path to a `<name>` skill body — is defined in `.nucleos/workflow/dispatch.md`.

**Read `.nucleos/workflow/workflow.md` and `.nucleos/workflow/dispatch.md` once before starting.** `<state_dir>` and `<bundle_dir>` are read from `.nucleos/workflow/project.json`; quote the substituted paths in any command, since either can contain a space.

## Entry point

`Use the orchestrate-agents skill. Task: <X>`

## Pre-flight checks

STOP on any failure:

- `<state_dir>/pipelines/` exists and is writable (create it when missing).
- `run-pipeline` skill body exists in the discovery path (downstream executor).
- `<bundle_dir>/scripts/pipeline_schema.py` exists (used for validation in step 3).

## Flow

1. **Build the agent catalog** exactly as `run-pipeline` Phase 1 does — the same package-generated agents (user-level `agents/` directory, recognised by the `nucleos-generated` marker) plus the Claude-only plugin scopes, the same field parse, and the same per-scope `subagent_type` computation. Do not restate that procedure here; follow `run-pipeline/SKILL.md` Phase 1.

2. **Draft the pipeline YAML inline** following the schema in the next section. Pick agents whose `description` fits each subtask; choose the sink node's `kind` based on whether the task needs integrated synthesis (`synthesize`), a single dominant answer (`passthrough`), or independent per-agent results collected side by side (`collect`). Keep the draft compact enough that the user can read it in one screen.

3. **Validate** the draft against the schema by writing the draft to a scratch file and checking it with `validate()` from `<bundle_dir>/scripts/pipeline_schema.py` (`python "<bundle_dir>/scripts/pipeline_schema.py" <slug>` runs the same check on a saved `<state_dir>/pipelines/<slug>.yaml`). If invalid, STOP with `pipeline draft invalid: <reason>` and surface the raw draft for debugging.

4. **Show the draft YAML** to the user with a suggested slug (lowercased task description, hyphenated, truncated to 50 characters).

5. **Offer three options** via `AskUserQuestion` (or the equivalent prompt) labeled exactly:
   - **Save & run** — write `<state_dir>/pipelines/<slug>.yaml`, then invoke `run-pipeline` with the same task.
   - **Save only** — write the file; exit without executing.
   - **Discard** — exit without writing.

6. **Slug collision**: if the user picks a save option and `<slug>.yaml` already exists in `<state_dir>/pipelines/`, suggest `<slug>-2`, `<slug>-3`, ... until unique. Never overwrite.

7. **Metrics**: record one row through `mcp__nucleos__record` with `{kind: "metric", phase: "pipeline_draft", payload: {tool, model, exit_code}}` (`tool`/`model` = the resolved session identity per dispatch.md's "Session identity" rule, normalized of any `[...]` variant suffix, not the raw static `session.*` config; `exit_code=0` if saved (either save option), `exit_code=2` if discarded). Metrics are observability; a failed record call must not abort the run.

## Pipeline YAML schema

The pipeline file at `<state_dir>/pipelines/<slug>.yaml` is a structure-only DAG with two top-level keys. There is no `output:` key — the sink node's `kind` carries what old drafts put under `output.mode`. This is the exact shape `<bundle_dir>/scripts/pipeline_schema.py` validates and `run-pipeline` executes.

- `description` — one-line summary of what the pipeline does.
- `nodes` — list of DAG nodes. Each node is EITHER an agent node (`agent: <subagent_type>`) or a flow node (`kind: <...>`), never both and never neither.

### Node rules

- `id` — lowercased + hyphenated short identifier (e.g. `in`, `scan-serve`, `out`); unique within the pipeline.
- `agent` — MUST hold the catalog record's `subagent_type` string (NOT the raw `name`). For package-generated agents and `user` scope this equals `name`; for `plugin_market` / `plugin_cache` scopes it's `<plugin>:<name>`.
- `kind` — either `input` (the single source marker) or a sink kind (`synthesize`, `collect`, `passthrough`). A pipeline has EXACTLY one `input` node and EXACTLY one sink node.
- `depends_on` — list of ancestor `id`s. Every non-input node needs one. Every agent root depends on the `input` node — the validator requires the `input` node to have downstream nodes and rejects any agent node not connected between input and sink. The `input` node itself MUST NOT declare `depends_on`. The sink node must be terminal (nothing may depend on it).

### Sink-kind rules

The sink node's `kind` tells `run-pipeline` how to produce the final answer:

- `synthesize` — subtask outputs are integrated into one consolidated answer (typical for audits, research roll-ups, cross-cutting reviews); costs one extra LLM call.
- `collect` — independent per-agent results returned side by side as an `{id: output}` map, each attributed (typical for diverging perspectives); no extra call.
- `passthrough` — one designated node's output is the final answer (typical for sequential refinement chains). A `passthrough` sink MUST have exactly one `depends_on` input; no extra call.

### Example (parallel fan-out, collect sink)

```yaml
description: Parallel review of a change — security + correctness, side by side
nodes:
  - id: in
    kind: input
  - id: sec
    agent: security-reviewer
    depends_on: [in]
  - id: correctness
    agent: code-reviewer
    depends_on: [in]
  - id: out
    kind: collect
    depends_on: [sec, correctness]
```

## Error table

| Condition | Action |
| --- | --- |
| Agent catalog empty | STOP `agent catalog empty — no agents to plan with` |
| Drafted YAML fails schema validation | STOP `pipeline draft invalid: <reason>`; surface raw draft |
| Slug collides and user does not rename | Suggest `<slug>-N` until unique; never overwrite |
| `run-pipeline` skill missing on Save & run | STOP `run-pipeline skill missing in discovery path` |

## What this skill does NOT do

- **No agent dispatch.** Task-tool dispatch of the planned DAG is delegated entirely to `run-pipeline`.
- **No synthesizer call.** Synthesis is delegated to `run-pipeline` and only runs when the saved pipeline's sink node has `kind: synthesize`.
- **No persistence to `<state_dir>/agent-runs/`.** Run packets are written by `run-pipeline`.
- **No agent file mutation.** Plugin agents are read-only catalog entries; treat them as candidates for the draft, never edit their files. The agent catalog is input to planning, not a request to create, remove, or modify agents.
