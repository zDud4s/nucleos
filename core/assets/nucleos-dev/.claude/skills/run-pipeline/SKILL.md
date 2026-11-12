---
name: run-pipeline
description: Use when the user asks to run or execute a saved agent pipeline by name from <state_dir>/pipelines/. Drafting new pipelines is the orchestrate-agents skill's job, not this one's.
---

<!-- Cockpit: this skill runs in the controller's own session, under the user's permissions
     and with the user watching, so it carries no tool wall — and its frontmatter must not
     pretend otherwise. A skill's `tools:` field is inert (`tools:` belongs to agent files),
     and the walls in this pipeline are on the dispatched phases: `.claude/agents/wf-*.md`. -->

You are the pipeline executor. You load a user-authored pipeline from `<state_dir>/pipelines/<name>.yaml`, resolve every node against the live agent catalog, topo-dispatch the DAG per the runtime branch below, and apply the output behavior declared by the pipeline's sink node `kind`.

**Runtime branch.** Detect the runtime before dispatch. Claude sessions dispatch ready DAG layers through the `Task` tool (Phase 2A below). Codex sessions dispatch ready DAG layers through `codex exec` subprocesses orchestrated by `<bundle_dir>/scripts/pipeline_fanout.py` (Phase 2B below). Both paths reuse the same DAG resolution, sink-kind output handling, and Wrap-up steps.

**Read `.nucleos/workflow/workflow.md` and `.nucleos/workflow/dispatch.md` once before starting.** `workflow.md` introduces `<state_dir>` and `<bundle_dir>`, both read from `.nucleos/workflow/project.json`; quote the substituted paths in any command, since either can contain a space. `dispatch.md` defines the dispatch contract used for the `synthesize` phase. Do not duplicate those rules here.

## Discovery path convention

"Discovery path" — the per-runtime path to a `<name>` skill body — is defined in `.nucleos/workflow/dispatch.md`, which you read at start.

## Entry point

`Use the run-pipeline skill. Pipeline: <name>. Task: <description>`

## Pre-flight checks

STOP on any failure:

- Run `python "<bundle_dir>/scripts/pipeline_schema.py" <name>`; any non-zero exit is a STOP, surface its stderr verbatim. It checks that `<state_dir>/pipelines/<name>.yaml` exists, that the slug matches `^[a-z0-9-]+$`, that the YAML parses, and that the schema validates.
- Only when the sink node's `kind` is `synthesize`: `<state_dir>/models.yaml` has `run_pipeline.synthesize` configured AND the `synthesizer` skill body exists in the discovery path. Other sink kinds skip both checks.

## Phase 1 - Load + catalog

Build the agent catalog from these scopes:

- `user = ~/.claude/agents/*.md` (or `$CLAUDE_CONFIG_DIR/agents/*.md` when set; Claude sessions only) — this is also where the daemon generates the agents of the installed agent packages. A generated file carries the marker comment `<!-- nucleos-generated: <name>@<version>; ... -->` right after its frontmatter; recognise package agents by that marker. The marker is provenance only and this skill never edits the file.
- `plugin_market = ~/.claude/plugins/marketplaces/**/agents/*.md` (Claude sessions only)
- `plugin_cache = ~/.claude/plugins/cache/**/agents/*.md` (Claude sessions only)

For each discovered agent, parse frontmatter into `{name, subagent_type, source, tools, description}`. Use the filename stem as `name` when frontmatter omits it. Agents carry no model: the model of a dispatch is decided by the dispatch, never by the agent.

**Exclude agents whose name starts with `wf-`.** Those are the workflow's own phase cockpits (the project's `.claude/agents/wf-*.md`, listed in dispatch.md's "The phase cockpit" table): they exist to be dispatched by the orchestrate controller with an assembled phase prompt, and their walls assume it. Offered as pipeline nodes they are a trap — `wf-executor` would arrive with `Edit`, `Write` and `Bash` and no packet, `wf-rescue` with nothing to diagnose. They are not part of the user's agent catalog and must never appear in one.

Compute `subagent_type` per scope:

- `user`: bare `name`.
- `plugin_market` + `plugin_cache`: `<plugin>:<name>`, where `<plugin>` is the directory two levels above the agent file (parent of the enclosing `agents/` directory). Example: `~/.claude/plugins/cache/<owner>/<plugin>/agents/foo.md` yields `<plugin>:foo`.

Resolve each pipeline `node.agent` string against the catalog's `subagent_type` values. STOP with `agent '<x>' not found in any scope at runtime` if any node is unresolvable.

## Dispatch note (applies to Phase 2A and 2B)

Flow nodes (`kind: input` and the sink node) are never dispatched as agents — they are not dispatched. The `input` node is a no-op source marker; nodes that `depends_on` it are the roots that receive the runtime task. Catalog resolution and `Task`/`codex exec` dispatch apply only to nodes with an `agent` field.

## Phase 2A - Dispatch (Claude / Task fan-out)

Topo-sort the DAG. For each ready layer (every `depends_on` is `completed`), dispatch independent subtasks in a SINGLE assistant message with multiple Task calls (parallel). Dependent subtasks await their ancestors.

Prompt template (literal — the `2000` is the chosen runtime value):

```
ANCESTOR_OUTPUT_CHAR_LIMIT = 2000

prompt = """<original user task description>

You are <agent.description from catalog>.

<if has ancestors>
Relevant context from prior steps:
  - <ancestor.id> (<ancestor.agent>):
    <ancestor.output, truncated to ANCESTOR_OUTPUT_CHAR_LIMIT chars>
</if>

Apply your specialty to the task above. Return your specialized output.
"""

Task(subagent_type=node.agent, prompt=prompt)
```

`<node.agent>` is the computed `subagent_type` carried in the resolved DAG node, not a raw catalog `name`. There is no code-orchestrator escape hatch; Phase 2 never uses a subprocess.

Failure handling — three distinct node statuses:

| Status | When | Downstream |
| --- | --- | --- |
| `completed` | Task call returned non-empty output without error | Output flows to descendants |
| `failed` | Task call errored, refused, or returned empty | Descendants whose `depends_on` includes this node are marked `skipped` (not dispatched) |
| `skipped` | Any ancestor in the node's transitive `depends_on` is `failed` or `skipped` | Not dispatched; surfaced in sink-kind output reporting |

Independent branches (no shared failed ancestor) continue normally. The pipeline halts only when no further progress is possible (every remaining node is `skipped`).

## Phase 2B - Dispatch (Codex / subprocess fan-out)

### Pre-flight (Codex-specific)

STOP with an explicit message naming the missing piece if any check fails:

- The `codex` binary is on PATH (`shutil.which("codex") is not None`).
- `<state_dir>/models.yaml` has a top-level `run_pipeline.codex_dispatch` block with at least `model`, `reasoning_effort`, and `timeout_seconds`.
- Catalog source is the package agents: the files in the user-level agents directory (`~/.claude/agents/` or `$CLAUDE_CONFIG_DIR/agents/`) carrying the `nucleos-generated` marker, plus every package with a generated prefix in `<state_dir>/agent-prefix/*.json`; `wf-*` excluded. Hand-made user agents and the plugin scopes are Claude-only and are NOT scanned in Codex sessions.
- For every agent used by a node, `<state_dir>/agent-prefix/<name>.json` exists, i.e. `agent_prefix.load_prefix(Path("<state_dir>/agent-prefix"), "<name>")` does not return `None`. Otherwise STOP `no generated Codex prefix for agent <name>: run `nucleos-core --agents-sync` from the project root, then retry`.
- The effective Codex model of every node is `run_pipeline.codex_dispatch.model` from `<state_dir>/models.yaml`; agents carry no model.

### Dispatch

Topo-sort the DAG with the same readiness rules as Phase 2A. For each ready layer, build a JSON spec for `<bundle_dir>/scripts/pipeline_fanout.py` with one `node` per ready DAG node. Each node is built with `agent_prefix.build_node` from `<bundle_dir>/scripts/agent_prefix.py`, which takes the sandbox and web-search settings from the agent's generated prefix and never emits a sandbox-bypass flag.

**Platform note (Windows):** `cmd[0]` must be the absolute path returned by `shutil.which("codex")` (e.g. `C:\Users\…\AppData\Roaming\npm\codex.CMD`). Python's `subprocess.run` does not consult `PATHEXT` when the first arg is a bare name, so the literal `"codex"` fails with `FileNotFoundError [WinError 2]` on Windows. Resolve once at pre-flight and reuse for every node. On POSIX, `shutil.which("codex")` returns the same string the shell would resolve, so the same pattern is portable.

```
import sys
from pathlib import Path
sys.path.insert(0, "<bundle_dir>/scripts")
import agent_prefix

codex_bin = shutil.which("codex")
prefix_dir = Path("<state_dir>/agent-prefix")
cfg = models["run_pipeline"]["codex_dispatch"]   # parsed <state_dir>/models.yaml

nodes = []
for node in ready_nodes:
    prefix = agent_prefix.load_prefix(prefix_dir, node.agent)   # None was a pre-flight STOP
    stdin = agent_prefix.build_stdin(
        prefix, task, [(a.id, a.agent, a.output) for a in node.ancestors]
    )
    nodes.append(
        agent_prefix.build_node(
            node.id, codex_bin, project_root, prefix,
            model=cfg["model"], effort=cfg["reasoning_effort"],
            timeout=cfg["timeout_seconds"], stdin=stdin,
        )
    )
spec = {"nodes": nodes}
```

`build_node` returns the plain `{"id", "cmd", "stdin", "timeout"}` node the helper accepts.

Invoke the helper synchronously. The helper itself already pins UTF-8 encoding when piping stdin to each node (Codex rejects non-UTF-8 input); callers do not need to set `encoding=` on this outer call.

```
subprocess.run(
    [sys.executable, "<bundle_dir>/scripts/pipeline_fanout.py"],
    input=json.dumps(spec),
    capture_output=True,
    text=True,
    encoding="utf-8",
)
```

Block until the helper exits. If it exits non-zero, STOP and report stderr. Otherwise parse stdout as JSON and map each per-node result:

| Helper result | DAG node status |
| --- | --- |
| `status=ok` | `completed` |
| `status=error` | `failed` |
| `status=timeout` | `failed` |

Descendants of failed nodes become `skipped`, using the same rule as Phase 2A. Independent branches continue normally.

### Metrics (Codex dispatch)

For each dispatched node, call `mcp__nucleos__record` once:

```
{kind: "metric", task_id: "<run slug>", phase: "pipeline_node",
 payload: {tool: "codex", model: "<resolved Codex model>", exit_code: <helper result exit_code>,
           agent: "<node.agent catalog name>", node_id: "<node.id>", duration_s: <helper result duration_s>}}
```

Skipped nodes record nothing. A failed record call must not abort the run.

## Phase 3 - Output handling

Read the sink node's `kind`:

| Sink kind | Behavior | Cost |
| --- | --- | --- |
| `passthrough` | Return the single `depends_on` node's output. Failed and skipped nodes are listed as notes appended to the returned text. | 0 extra LLM calls |
| `synthesize` | Dispatch the `synthesizer` skill over the sink's `depends_on` outputs via the tool/model in `<state_dir>/models.yaml` `run_pipeline.synthesize` (per the dispatch contract referenced above). Pass the original task, every node's output, the DAG with final statuses, and the failed/skipped sets. | 1 extra LLM call |
| `collect` | Return the `{id: output}` map of the sink's `depends_on` nodes without fusion. Skipped and failed nodes carry their status verbatim. | 0 extra LLM calls |

## Wrap-up

1. Persist the filled run to `<state_dir>/agent-runs/<YYYY-MM-DD>-<task_slug>.md` (create the directory when missing). The packet includes a top-level `pipeline: <name>` field. New file only — never overwrite; if the path exists, append `-2`, `-3`, ... before `.md` until unique. `<YYYY-MM-DD>` is today's UTC date.
2. Record metrics through `mcp__nucleos__record` (observability only; a failed call must not abort the run):
   - One row per dispatched Claude node: `{kind: "metric", task_id: "<run slug>", phase: "pipeline_dispatch", payload: {tool: "<agent name>", model: "<dispatch model, omit when unknown>", exit_code, agent, node_id, duration_s}}`. Skipped nodes record nothing. Codex runs already recorded their per-node rows in Phase 2B.
   - One row for the synthesis step when the sink node's `kind` is `synthesize`. Assemble the prompt with `python "<bundle_dir>/scripts/assemble_prompt.py" --phase synthesize --slug <run slug> --n 1 --objective <file> --prior-artifacts <file>`, dispatch what it wrote through the `wf-synthesizer` cockpit (`Read`, `Glob`, `Grep`, nothing else) per the dispatch contract, then record `{kind: "metric", task_id: "<run slug>", phase: "synthesize", payload: {tool, model, exit_code, duration_ms, prompt_file, prompt_sections}}`, taking `prompt_file` and `prompt_sections` from the manifest the assembler left beside the archive. Never compose the manifest by hand.
3. If the `synthesize` phase produced memory updates, append them as `- YYYY-MM-DD [pipeline] <fact>` lines to `<state_dir>/memory.md` (`pipeline` is in the closed `## Topic vocabulary` block at the top of that file — check it before writing any other tag; an unlisted tag makes the entry unreachable by every memory slice). If the memory size after the append would cross `memory_tuning.consolidation_threshold_lines` from `<state_dir>/project.yaml`, dispatch the `maintenance` skill per `<state_dir>/models.yaml` `run_pipeline.maintenance` to compact memory; otherwise append inline. If there are no updates, report `none`.

Report to the user: final output (per sink kind), per-node statuses, failed and skipped nodes with reasons, files changed (if any agent changed files), memory updates, and the phase execution log.

## Error table

| Condition | Action |
| --- | --- |
| `<state_dir>/pipelines/<name>.yaml` missing | STOP `pipeline '<name>' not found at <state_dir>/pipelines/<name>.yaml`. |
| Slug fails regex | STOP `invalid slug '<name>' (must match ^[a-z0-9-]+$)`. |
| YAML parse error | STOP `pipeline invalid: <yaml parse error>`. |
| Schema validation failure | STOP and surface every `pipeline invalid: ...` error from the validator. |
| sink `kind` is `synthesize` but `run_pipeline.synthesize` missing in `<state_dir>/models.yaml` | STOP `run_pipeline.synthesize not configured`. |
| `synthesizer` skill body missing when needed | STOP and report the expected discovery path. |
| Node `agent` unresolvable in catalog | STOP `agent '<x>' not found in any scope at runtime`. |
| Agent Task call failure / empty output | Mark node `failed`; mark dependents `skipped`; continue independent branches; surface in output. |
| All remaining nodes are `skipped` | Halt Phase 2; still run Phase 3 over whatever completed and persist the run. |
| `codex` binary not on PATH when running Codex Phase 2B | STOP with an explicit message naming the missing `codex` binary. |
| `<state_dir>/models.yaml` missing `run_pipeline.codex_dispatch` block when running Codex Phase 2B | STOP `run_pipeline.codex_dispatch not configured`. |
| Helper subprocess crashes or exits non-zero during Codex Phase 2B | STOP and report stderr from `<bundle_dir>/scripts/pipeline_fanout.py`. |
| Codex node's agent has no `<state_dir>/agent-prefix/<name>.json` | STOP `no generated Codex prefix for agent <name>: run `nucleos-core --agents-sync` from the project root, then retry`. |

## Notes

- Pipelines are structure-only. Do NOT add per-node prompt templates or variables — prompts are built at runtime from the entry-point task + catalog descriptions + ancestor outputs.
- Plugin agents are read-only catalog entries; dispatching them is fine, editing their files is not.
- The agent catalog is an input to dispatch, not a request to create, remove, or modify agents.
