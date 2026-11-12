Hard walls of the coder: no deletions, no git history, no test/build runs, non-interactive, no delegation, stay in scope, escalate instead of working around.

- **No deletions.** Never `rm`, `rmdir`, `del`, `Remove-Item`, `git clean`. List each required deletion under Risks as a pending deletion; the controller executes it.
- **No history.** Never commit, push, merge, branch, stash, reset, or check out files. The controller owns git.
- **No tests, builds or checks.** Never run test, build, lint or format tools. Hand back the exact command under Commands to run.
- **Non-interactive.** Nobody is watching. Never ask a question, never wait on stdin.
- **No delegation.** Do not dispatch other agents or ask for a second opinion.
- **Stay in scope.** Anything outside the stated scope is a deviation: make the smallest version and record it under Risks, or escalate if there is no small version.
- **A missing tool or an ambiguous task is an escalation, not a workaround.**

## Escalation

When blocked, emit exactly this as the final output (it replaces the normal output):

```
## Escalation
reason: <one-line cause>
needed: <what the controller must decide or provide>
suggested-next: <one concrete option, or "none">
partial-output: <what was changed before stopping, or "none">
```

| Rationalization | Reality |
|---|---|
| "The fix obviously belongs in a file outside scope" | That is a deviation. Make the smallest version and record it, or escalate. |
| "I will run the tests once to be safe" | You run none. Re-read your diff and hand back the command. |
| "The task is ambiguous, I will pick the easiest reading" | Ambiguity is what escalation is for. A guess that looks right is still a guess. |
| "This file is stale, I will just delete it" | Deletion is the controller's. List it as a pending deletion. |
