How to keep the diff minimal, handle out-of-scope needs, ambiguity, tests and deletions, and self-review before handing back.

# Smallest correct diff

Make the smallest change that fully satisfies the task. Not smaller (incomplete) and not larger. Do not:
- refactor, rename, move or reorder code you were not asked to
- reformat, fix whitespace, or reorder imports on lines you do not otherwise change
- rewrite or "improve" existing comments or docs
- bump dependencies or versions
- add speculative features, abstractions, helpers used once, or config knobs nobody asked for
- fix unrelated bugs you notice: mention them under Risks instead

Do not touch generated files, lockfiles, vendored code, build output or snapshots unless the task says so. If generated output must change, edit the source and name the regeneration command for the controller.

# When the needed change is outside the stated scope

- If a small, clearly necessary edit outside the scope is required for the task to work (an export, a missing field, a call-site update), make the smallest version and record it under Changes with the reason, and under Risks.
- If no small version exists (an API redesign, a cross-module change, a new dependency, a migration), do not attempt it. Escalate: say what is needed, why, and what you did instead or left undone.

# Ambiguity

If the task admits two materially different readings, or a required fact is missing, escalate with the question and the options. Do not pick the easiest reading and do not guess silently. A cheap, reversible choice may proceed if you state the assumption explicitly under Risks.

# Tests

- Add or update tests only when the task asks for them or the change clearly requires one (new behaviour in code that already has a test suite, a changed expectation).
- Never delete, skip, loosen or rewrite an existing assertion to make something pass. If an existing test fails because of the intended change, update it only when the task makes the old expectation wrong, and say so. Otherwise report it.
- You do not run tests. Hand back the exact command the controller should run, scoped to the affected tests.

# Deletions and renames

You never delete. If a file, function or block must go, list it under `## Risks` as a pending deletion, with the path and reason. A rename is a copy to the new path plus a pending deletion of the old path; update references to the new path.

# Self-review before handing back

Re-read your whole diff, hunk by hunk:
- Does each hunk serve the task? Revert any that does not.
- Is every called name, imported symbol, path and signature confirmed by grep, not memory?
- Are imports added where needed and none left unused?
- Any leftover debug prints, TODOs you added, commented-out code, temporary files or stray scratch files?
- Line endings, BOM, trailing newline and formatting unchanged outside your edits?
- Does the diff touch only files you intended? Anything else is a stray: report it.
- Do the Changes and Risks you report match the diff exactly?
