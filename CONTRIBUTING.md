# Working in this repository

This file exists because the rule it describes is enforced by a running daemon, and a rule
you meet as a refusal is worse than one you meet as a sentence.

It is tracked deliberately. The instruction files an agent normally reads — `AGENTS.md`,
`CLAUDE.md` — are local here and never enter git, so they cannot be where this lives: a
fresh clone would have the enforcement and not the explanation.

## Git operations go through a queue

Several sessions work in this repository at once, each in its own worktree, all sharing one
`.git`. The queue exists so their git operations happen **one at a time, in order**, rather
than racing. It is not an approval gate — it is a scheduler.

A `PreToolUse` hook (`.claude/hooks/ask_daemon.py`) asks the daemon about git commands. It
can refuse and it can never approve, so it grants nothing that was not already yours.

**Six operations the queue performs.** Run them as you would anyway; the hook takes them
over and hands you a ticket number instead of running them:

| | |
|---|---|
| `git merge <ref>` | brings `<ref>` into the branch your worktree is on |
| `git push <remote> [<branch>]` | |
| `git tag <name> [<branch>]` | |
| `git fetch <remote>` | |
| `git branch -d <branch>` | `-d` only — git's own refusal is the safety |
| `git rebase <onto>` | recognised, and always blocked from a session; see below |

**What it refuses outright**, because the same operation in a spelling it cannot order is
worse than one it can: a forced push, a bare `git push`, `git pull`, and `git branch -D`.
The refusal names the spelling to use instead.

**What it says nothing about**: everything else, including `git merge --squash`, `--abort`,
and every read-only command. Those touch only your own worktree or index.

## Delivering finished work

```
nucleos-core --land
```

Run it from inside your worktree. It asks the queue to merge your branch into the branch
the project is on. There is no git command for this from inside a worktree — you would
have to check out the integration branch, which is the isolation you must not break — so
this is the door.

Asking is authorisation: your completion is the decision that the work is ready. What stays
with the queue is *when*.

## Conflicts are not yours to resolve

The queue computes every merge in an integration worktree you do not have. If it conflicts,
it aborts there, publishes nothing, and leaves **no conflicted state anywhere** — including
in your copy. There is nothing for you to fix, and manufacturing something to fix is the
one wrong response.

The right one is the other direction: bring the target branch into yours (`git merge master`,
an ordinary queue operation), resolve it in your own worktree on your own branch, and ask
again.

A rebase of your own branch is always blocked for the same reason in reverse. The queue
publishes with a command that refuses rather than destroys; a rebase has no such command,
and the branch it would rewrite is the one your session resumes onto.

## If you are blocked

Stop and say what the block said. Do not reword the command, do not reach for another
spelling, and do not route around it with `-C` or a different tool. Every one of these
refusals carries the reason and, where there is one, the thing to do instead.
