# Execution Packet — FILLED REFERENCE (not a live packet)

<!--
WHAT THIS IS. A worked copy of `execute.md`, kept as the "what good looks like" anchor for
the execute phase. Read it beside the schema: the schema says which fields exist, this says
what an acceptable value in each one reads like.

WHY IT EXISTS. Every packet in `.nucleos/workflow/packets/` is an empty form, and a form
teaches shape without teaching standard. What anchors a model is the format and distribution
of the input, not more instruction prose (Min et al., EMNLP 2022).

THE EVIDENCE. The two `git check-ignore` blocks are pasted, unedited output from a real run.
The third block (the red leg) is an ILLUSTRATIVE placeholder, marked as such in place: a
project-supplied `python` check, not a test. Executors run no tests, so evidence is only what the
packet asks for, such as a read-only `python` or `git` command, or a quoted `mcp__nucleos__verify`
result the controller handed in.

NOTE WHAT THIS EXAMPLE DOES NOT DO: it does not end all-green. One declared command exits 1,
for a reason that predates the task. That is the point. The gate demands an exit code and a
tail per command — it never demands that the code be zero. A Handoff that hides a red leg to
look finished is the failure mode; one that records it, names it as pre-existing and says so
in `Deviations from plan` is the standard.

This example declares no test command, so batched mode (executors run no tests) does not
show here; a test command would carry `deferred: batched` in place of an exit and a tail.

DO NOT dispatch this file, and do not edit it during a task. It is read-only reference,
same as the schema beside it.
-->

Task ID: reference-stale-ignore-rule
Objective: Stop a stray root `node_modules/` from showing up as an untracked path in every task's diff.
Size: trivial

## Steps
1. Reproduce: run `git status --short` and confirm the root `node_modules/` is listed.
2. Find which ignore rule should have covered it.
3. Fix the cause, not the symptom: un-anchor the rule rather than adding a per-path exception elsewhere.
4. Confirm the root path now resolves to an ignore rule.
5. Confirm the previously-ignored sibling path is still ignored.

## File Context
.gitignore:51 - `/shell/node_modules/` — anchored to `shell/`, so a root `node_modules/` is NOT ignored
.gitignore:49 - `.cargo-target*/` — the in-file precedent for un-anchoring, commented "Any depth on purpose"

## Boundaries
Allowed files: `.gitignore`
Do-not-touch files/dirs: `node_modules/` itself (a regenerable tool cache — the ignore fixes the diff without deleting anything)

## Tests
To add: none — the change is a `.gitignore` pattern; the assertion is `git check-ignore`, recorded below.

## Validation
Commands:
- `git check-ignore -v node_modules`
- `git check-ignore -v shell/node_modules`
- `python tools/check_layout.py` (illustrative: a read-only project check that was already red before this task)
Expected result: both paths resolve to an ignore rule; the layout check's result is unchanged by this task.

## Handoff

Files changed:
- `.gitignore` — replaced the `shell/`-anchored `node_modules` rule with an unanchored one, plus a comment recording the failure and the date.

Tests added:
- none, as planned. `git check-ignore` is the assertion, and it is evidenced below.

Validation evidence:
$ git check-ignore -v node_modules
exit: 0
tail: `.gitignore:59:node_modules/	node_modules`
$ git check-ignore -v shell/node_modules
exit: 0
tail: `shell/.gitignore:10:node_modules	shell/node_modules`
$ python tools/check_layout.py   (ILLUSTRATIVE output, see the note at the top)
exit: 1
tail:
```
legacy/parse.py:41: 'result' is assigned a value but never used
1 problem
```
Deviations from plan: `python tools/check_layout.py` exits 1 and this task did not make it green. The error is in a file this task never touched and reproduces on a clean checkout of the branch — so it is pre-existing, not a regression. Recorded rather than fixed: widening the change to repair unrelated code is scope creep, and hiding the red leg to look finished is the opposite of what the evidence is for.
New risks discovered: none from the change itself; the pre-existing layout error will keep any check built on it red until somebody owns it.
Memory updates:
- 2026-01-01 [tooling] An ignore rule anchored to a subdirectory (`/shell/node_modules/`) leaves the root copy unignored; a tool run from the repo root writes `<cwd>/node_modules/` and `git status` lists it. Prefer an unanchored pattern, as `.cargo-target*/` already does.
- 2026-01-01 [process] `evidence_gate.py` checks that each declared validation command carries an `exit:` and a `tail:`; it never checks that the exit is 0. An honest red leg passes the gate and a hidden one is what it catches.
Pending deletions: none. The stray `node_modules/` holds one cache file and is left in place — ignoring it was enough.

## Follow-ups
- `legacy/parse.py:41` carries an unused variable that keeps the layout check red; it predates this task and needs an owner, not a silent edit inside an unrelated change.
