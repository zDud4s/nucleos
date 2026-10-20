Review a spec, plan or doc (not code) for defects that would mislead implementation.

## Reporting rules
- Cite file:line of the section line; line 0 only when no single line applies.
- Do not rewrite the document. Point at the defect; say what is missing or which two lines conflict (give both line numbers in the text).
- Do not flag wording preference or prose style.
- Check the doc against the repo rules given as context (project instructions, decisions). A doc that contradicts them is a finding.

## Consistency
- Same term with two meanings, or two terms for one thing.
- A number, name, path, limit or default that differs between sections.
- Table and prose disagreeing; summary and detail disagreeing.
- A decision recorded without the rejected alternative or the reason.

## Stale references
- File, function, type, flag or line number cited: Grep to confirm it exists before reporting it stale. Report only confirmed misses; say what the search found.
- Cited line numbers drift; if the symbol exists but the cited line drifted, report low at most.

## Completeness
- Missing cases: error path, empty state, rollback, upgrade of existing data, concurrency/ordering, who owns what, failure of a dependency.
- Undefined terms or acronyms used as if known.
- Unstated assumptions (environment, platform, prior step done, data shape).
- Security/privacy not mentioned where data, secrets, permissions, network or user content are touched.
- TODO/TBD/"decide later" that blocks implementation of a step that depends on it.

## Verifiability and scope
- Requirement that cannot be tested or verified ("fast", "robust", "should handle").
- Acceptance criterion not mapped to any step; step that serves no criterion.
- Scope that silently grew beyond the stated goal or non-goals.
- Step order where a prerequisite (migration, type, flag, file) appears after its use.
- Plan step too large to verify alone, or with no stated check that it is done.

## Repo-specific
- Plans/specs/notes are local working material and never committed (see project rules); a step that commits them, or memory/decisions entries, is a violation.
- Comments and commit messages in English; specs under .ai/specs stay Portuguese.
