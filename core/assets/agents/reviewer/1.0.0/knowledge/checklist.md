Core review checklist: severity, verification, what to report and skip.

## Severity
- high: wrong result, data loss or corruption, security hole, crash on a normal path, broken public contract or migration. Must not merge. Example: a migration drops a column that a live query still reads.
- medium: wrong on a realistic edge or error path, regression risk, missing test for changed behavior, unhandled failure. Example: a retry loop swallows the final error and returns success.
- low: real but minor: misleading message, small leak, or suspicious but unverifiable. Example: an error text names the wrong field.

## What to check, in order
1. Correctness: does it do what the task says? Read the whole changed function, not only the hunk.
2. Regressions: Grep callers and other users of every changed symbol.
3. Boundaries: empty, zero, one, max, off-by-one, unicode, huge input.
4. Error paths: swallowed errors, partial failure, missing cleanup, bad retries.
5. Concurrency and resources: races, lock order, leaks, unbounded growth.
6. Contracts: API, schema, CLI or file-format changes, and every consumer of them.
7. Security: when input, paths, auth or secrets are touched, read the matching entries of the on-demand file index.
8. Tests: does each changed behavior have a test, and does it assert the behavior or merely run it? You cannot run tests; say which command would confirm a doubt.

## Verify before reporting
- Open file:line and confirm the line number comes from the real file, not the diff.
- Check callers and existing guards; a guard may sit outside the hunk.
- Try once to disprove your finding.
- If you cannot confirm it, drop it, or report it as low and state what you could not verify.

## Do not report
- Style, naming, formatting, taste.
- Speculative "might be slow".
- Problems in untouched code, unless the change interacts with them (say how).
- Anything the compiler or linter already enforces.
- A restatement of the diff, or praise.

## Scope
- Flag changes beyond the stated task (scope creep) and missing pieces of the task.
- Review the code, not the author's description of intent.

## Noise control
- One line per root cause; merge duplicates.
- Prefer 3 verified findings over 15 doubtful ones.
- If everything checks out, output `- none` per the output spec. Do not invent a low finding to look thorough.

## On demand
Read the matching entries of the on-demand file index (it gives absolute paths): rust, go, typescript-react, shell-powershell, sql-migrations, docs-specs, security, tests-and-compat, failure-modes.
