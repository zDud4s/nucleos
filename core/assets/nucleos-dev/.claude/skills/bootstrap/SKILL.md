---
name: bootstrap
description: "Use when onboarding a repository to the AI workflow, when <state_dir>/project.yaml is missing or has project_name: unknown, or when project metadata is incomplete or stale."
---

<!-- Cockpit: `.claude/agents/wf-bootstrap.md`. Its `tools:` line is the wall the harness
     enforces; it is not repeated here, because a skill's `tools:` frontmatter is inert. -->

You are the bootstrap skill.

Goal:
Adapt the workflow to the current repository without implementing product changes, and never rewrite the workflow core (`.nucleos/workflow/`, skills, cockpits, packets).

`<state_dir>` is read from `.nucleos/workflow/project.json` (key `state_dir`). It lives outside the repository tree, so nothing you write there can enter git.

## You must

1. Detect the stack, package managers, and likely entrypoints.
2. Identify install, dev, build, lint, format, and typecheck commands. If a command cannot be confirmed from repo evidence, mark it as an assumption. **Do not write test commands for the final verification into `project.yaml`:** that belongs to `nucleos.tests.yaml`, which the daemon reads. Test commands may appear only as informational entries, marked as assumptions when unconfirmed.
3. Identify major directories and assign rough ownership domains.
4. Identify risky areas, generated files, do-not-touch zones, and security-sensitive areas.
5. Update only:
   - AGENTS.md (only project-specific sections if needed) — edits MUST stay OUTSIDE the managed workflow block that the daemon maintains in that file. Read first; never replace or move the managed block. If the file doesn't exist, leave it for the daemon to create. <!-- F5-OPEN: exact start/end markers of the agents-block.md managed block (the source named `# >>> AI WORKFLOW MANAGED BLOCK >>>`); confirm against bundle.yaml. -->
   - CLAUDE.md (only project-specific sections if needed) — never touch a block the daemon manages there; read first, never replace, never duplicate it. <!-- F5-OPEN: whether the daemon still writes a managed import into CLAUDE.md, and its markers. -->
   - `<state_dir>/project.yaml` — write it as **valid YAML** following the rules in "Writing valid `project.yaml`" below. Ensure the `memory_tuning` block exists with defaults (`consolidation_threshold_lines: 150`, `floor: 50`, `ceiling: 300`, `last_ratios: []`, `last_consolidated_at: ""`). Leave existing values intact if the block is already populated.
   - `<state_dir>/models.yaml` — create it if absent, with tool/model assignments per phase; leave existing assignments intact.
   - `<state_dir>/memory.md` (and `<state_dir>/decisions.md` if absent: header, format line, empty `## Entries`). `memory.md` MUST carry, before `## Entries`, a `## Topic vocabulary` block with exactly two fenced lists: the canonical tags first, then the legacy ones (`(none)` when there are none). Every entry is one line, `- YYYY-MM-DD [tag] fact`, with one canonical tag. The controller slices memory by tag and `assemble_prompt.py` refuses a file without this block, so a `memory.md` without it stops the first execute dispatch. Keep the canonical list short and drawn from the project (e.g. `build tests workflow`), plus `workflow` and `memory`, which later phases write to. Example:

     ````markdown
     ## Topic vocabulary

     ```
     build tests workflow memory
     ```

     ```
     (none)
     ```

     ## Entries
     - 2026-01-01 [tests] The test map is nucleos.tests.yaml; only the daemon's verify runs it.
     ````
6. Preserve the workflow role structure (planner, executor, reviewer). Do not hardcode tool or model names anywhere except `<state_dir>/models.yaml`.
7. Record uncertainty explicitly in `assumptions` and `unknowns`.
8. After filling `project.yaml`, prompt the user to review `<state_dir>/models.yaml` and adjust tool/model assignments if the defaults do not match their setup.
9. If the project has clear subdirectories with distinct domains (e.g. `backend/`, `frontend/`, `packages/*`), create local AGENTS.md files for each with domain-specific constraints. Do NOT create subdirectory AGENTS.md files for projects without clear subdomain separation (CLI tools, single-purpose libraries, monoliths with flat structure). Track created files in `subdirectory_agents` in `project.yaml`.
10. Do not implement features or fixes.
11. Inform the model (via `<state_dir>/memory.md`) that it can invoke the **`codex` skill** to run Codex CLI for implementation tasks, and the **`orchestrate` skill** to run the full plan→execute→review pipeline from a single prompt. These are the primary execution paths for delegating work.
12. If the repository has no `nucleos.tests.yaml`, do not invent one as a confirmed fact. <!-- F5-OPEN: should bootstrap draft a `nucleos.tests.yaml` (units, covered paths, `full_sweep`) from the detected commands when the repo has none? Not decided; until then, report its absence under Unknowns. -->

## Writing valid `project.yaml`

`<state_dir>/project.yaml` is parsed by the workflow scripts, any harness driving them, and every later
phase, so it MUST be valid YAML. The failure mode is always the same: free-text
descriptions get written into list fields and break the parser. Follow these rules:

- Every item in a list field (`stack`, `commands.*`, `entrypoints`,
  `important_dirs`, `important_files`, `ownership.*`, `boundaries.*`,
  `conventions.*`, `assumptions`, `unknowns`) is a single **plain string** — never
  a nested mapping.
- A bare `: ` (colon + space) inside an unquoted list item makes YAML parse it as a
  `{key: value}` mapping (or hard-error). If an item must contain a colon,
  **single-quote the whole item**:
  `- 'phase-pinned dispatch: every phase runs in its pinned model'`.
- Never put text after a quoted string on the same line — `- "foo" (bar)` is a hard
  parse error (`bad indentation of a mapping entry`). Quote the whole item instead:
  `- 'foo (bar)'`.
- For annotations use a trailing ` # comment` (with a space before `#`), not a colon.
- `boundaries.generated_files` is the **host project's** generated files (build/dist
  output, codegen, vendored bundles) — discovered per project. Do NOT list the
  workflow's own files (`.nucleos/`, `.claude/skills/`, `.claude/agents/`). Leave `[]` if the project has none.

### Validate before finishing (mandatory)

After writing `project.yaml`, prove it parses and that every list entry is a
plain string. Do not declare done until this exits 0 (pass the real `<state_dir>` path as the argument):

```bash
python - "<state_dir>/project.yaml" <<'PY'
import sys, yaml
d = yaml.safe_load(open(sys.argv[1], encoding='utf-8'))
bad = []
def check(seq, path):
    if isinstance(seq, list):
        for i, x in enumerate(seq):
            if not isinstance(x, (str, type(None))):
                bad.append(f'{path}[{i}]={x!r}')
for k, v in (d or {}).items():
    check(v, k)
    if isinstance(v, dict):
        for k2, v2 in v.items():
            check(v2, f'{k}.{k2}')
sys.exit('STRAY COLON / non-string list items: ' + '; '.join(bad) if bad else 0)
PY
```

If it errors or reports items, fix the offending line(s) (usually a stray `: `) and
re-run until clean. A `SyntaxError`/parse exception means the YAML is malformed;
non-string items mean a list entry was read as a mapping. If PyYAML is unavailable,
`pip install pyyaml` transiently or fall back to `python -c` structural checks — do
not skip validation.

## Token budget

Bootstrap output ≤150 lines.

## Output format

The skill returns a **markdown report** with the following sections:

- Detected stack
- Commands (confirmed vs assumed)
- Important dirs/files
- Risks and boundaries
- Subdirectory AGENTS.md files created (if any)
- Updated files
- Assumptions
- Unknowns (including whether `nucleos.tests.yaml` exists)
