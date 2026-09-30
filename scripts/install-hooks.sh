#!/usr/bin/env bash
# Recreate this repo's local git hooks and point git at them.
#
# The hooks live in .githooks/, which .gitignore excludes, and git finds them through
# core.hooksPath, which is *local* config. Neither travels with a clone. That is deliberate — they
# are local tooling, not part of the product — but it leaves every fresh clone and every new
# machine silently unguarded, indistinguishable from a guarded one right up to the commit that
# should have been blocked. So the recipe is committed even though the artefact is not.
#
# **This file is the one source of the hooks.** It once drifted: it still wrote a pre-commit that
# refused every staged path under `.claude/` (master unmergeable, since `settings.json` is
# tracked), no plan/memory guard, no queue guard, and a relative core.hooksPath that left every
# linked worktree unguarded. Edit a hook HERE, re-run, and `--check` says whether the installed
# copies still match.
#
# core.hooksPath is set ABSOLUTE: git resolves a relative one against the working tree it runs in,
# and .githooks/ exists only in the main checkout, so a relative value guards no linked worktree.
# Worktrees share the repository config, so one absolute value arms all of them.
#
# Idempotent: re-running overwrites the hooks with the versions below and re-points the config.
#
# Usage: scripts/install-hooks.sh           install
#        scripts/install-hooks.sh --check   exit 1 if the installed hooks differ from these
set -euo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$repo_root"

# The MAIN checkout's .githooks/, found through the shared git directory, so this does the same
# thing from a linked worktree as from the main checkout. `pwd -W` (Git bash) gives the drive-letter
# spelling git itself stores; elsewhere it does not exist and `pwd` is already right.
main_root="$(cd "$(git rev-parse --git-common-dir)/.." && (pwd -W 2>/dev/null || pwd))"
hooks_dir="$main_root/.githooks"
mode="install"
if [ "${1:-}" = "--check" ]; then
  mode="check"
  target="$(mktemp -d)"
  trap 'rm -rf "$target"' EXIT
else
  target="$hooks_dir"
  mkdir -p "$target"
fi

cat > "$target/pre-commit" <<'HOOK_EOF'
#!/bin/sh
# Reject commits that ADD files under the workflow-local, gitignored directories.
#
# Additions only (`--diff-filter=A`), and the narrowing is load-bearing. A few files under
# `.claude/` are tracked and have been since before this guard existed -- `hooks/ask_daemon.py`
# and `settings.json` ship with the repo. Checking every staged path made master unmergeable for
# anyone running this hook: master changes `ask_daemon.py`, the merge stages that change, and the
# commit is refused. The only ways out were to bypass the guard or to record the merge as reverting
# master's work in a file the gate itself tests -- both worse than what the guard is for.
#
# What it is for is unchanged: nothing NEW under these directories enters git. The agent's own
# working material -- memory, decisions, plans, specs -- is created, never inherited, so it is
# caught by exactly this filter. See CLAUDE.md.
forbidden_re='^(\.ai|\.claude|\.agents)/'
bad="$(git diff --cached --name-only --diff-filter=A | grep -E "$forbidden_re")"

if [ -n "$bad" ]; then
  echo "commit blocked: NEW file(s) under .ai/, .claude/, or .agents/ (must stay untracked):" >&2
  printf '%s\n' "$bad" >&2
  echo "unstage with: git restore --staged <file>" >&2
  exit 1
fi

# Reject commits that stage plans, specs, spikes or notes. Hard project constraint:
# the agent's working material is per-developer state and never enters git. Same rule
# as .ai/ above — these are the copies that ended up under docs/. See CLAUDE.md.
local_only_re='^docs/(plans|specs|spikes|notes)/'
local_bad="$(git diff --cached --name-only | grep -E "$local_only_re")"

if [ -n "$local_bad" ]; then
  echo "commit blocked: staged plan/spec/spike/note file(s) (must stay untracked):" >&2
  printf '%s\n' "$local_bad" >&2
  echo "unstage with: git restore --staged <file>" >&2
  exit 1
fi

# Reject commits that stage a memory or decisions file carrying entries.
# Hard project constraint: these files may only ever reach git as empty templates
# (header + format + "## Entries" heading). The content is per-developer state and
# never leaves this machine. See CLAUDE.md.
#
# Entry shapes matched: "- YYYY-MM-DD [topic] fact" (memory) and "- Date: YYYY-MM-DD"
# (decisions). A FILLED date is what makes it an entry — the empty template's bare
# "- Date:" line must still pass, so the second alternative demands a digit.
# The check reads the STAGED blob, not the worktree, so a dirty local copy neither
# triggers nor hides the guard.
knowledge_re='(^|/)(memory|decisions)(-archive)?\.md$'
entry_re='^[[:space:]]*[-*][[:space:]]+([0-9]{4}-[0-9]{2}-[0-9]{2}|Date:[[:space:]]*[0-9])'
dirty=''

for f in $(git diff --cached --name-only --diff-filter=ACMR | grep -iE "$knowledge_re"); do
  if git show ":$f" 2>/dev/null | grep -qE "$entry_re"; then
    dirty="$dirty$f
"
  fi
done

if [ -n "$dirty" ]; then
  echo "commit blocked: memory/decisions file(s) staged WITH entries:" >&2
  printf '%s' "$dirty" >&2
  echo "" >&2
  echo "These files go to git empty — header and format only, zero entries." >&2
  echo "Strip the entries and re-stage, or: git restore --staged <file>" >&2
  exit 1
fi
HOOK_EOF

cat > "$target/commit-msg" <<'HOOK_EOF'
#!/bin/sh
# Reject commits whose message carries a Co-Authored-By trailer.
# This repo's convention is no co-authors on commits, from any agent or tool.
msg_file="$1"

if grep -qiE '^co-authored-by:' "$msg_file"; then
  echo "commit blocked: message contains a Co-Authored-By trailer (not used in this repo)." >&2
  echo "remove the trailer and retry the commit." >&2
  exit 1
fi
HOOK_EOF

cat > "$target/pre-merge-commit" <<'HOOK_EOF'
#!/bin/sh
# See nucleos-queue-guard beside this file for what this is and what it is not.
#
# Known limits, stated here so nobody mistakes this for the guarantee:
#   - `--no-verify` skips it, as it skips every client-side git hook.
#   - it does NOT fire on a fast-forward merge, which creates no commit.
#   - there is no pre-tag, pre-fetch or pre-branch-delete hook in git, so those three are covered
#     by the daemon gate alone (`vcs::unqueueable_but_shared`).
. "$(dirname "$0")/nucleos-queue-guard"
queue_guard "merge" "git merge <branch>  /  git merge --no-ff <branch>" "git merge --abort"
HOOK_EOF

cat > "$target/pre-push" <<'HOOK_EOF'
#!/bin/sh
# See nucleos-queue-guard beside this file for what this is and what it is not.
#
# Known limits, stated here so nobody mistakes this for the guarantee:
#   - `--no-verify` skips it, as it skips every client-side git hook.
#   - pre-merge-commit does NOT fire on a fast-forward merge, which creates no commit.
#   - there is no pre-tag, pre-fetch or pre-branch-delete hook in git, so those three are covered
#     by the daemon gate alone (`vcs::unqueueable_but_shared`).
. "$(dirname "$0")/nucleos-queue-guard"
queue_guard "push" "git push <remote>  /  git push <remote> <branch>"
HOOK_EOF

cat > "$target/pre-rebase" <<'HOOK_EOF'
#!/bin/sh
# See nucleos-queue-guard beside this file for what this is and what it is not.
#
# Known limits, stated here so nobody mistakes this for the guarantee:
#   - `--no-verify` skips it, as it skips every client-side git hook.
#   - pre-merge-commit does NOT fire on a fast-forward merge, which creates no commit.
#   - there is no pre-tag, pre-fetch or pre-branch-delete hook in git, so those three are covered
#     by the daemon gate alone (`vcs::unqueueable_but_shared`).
. "$(dirname "$0")/nucleos-queue-guard"
queue_guard "rebase" "git rebase <onto>"
HOOK_EOF

cat > "$target/nucleos-queue-guard" <<'HOOK_EOF'
# Sourced by pre-merge-commit, pre-push and pre-rebase. Not a hook itself, and not executable.
#
# **The second layer under `hooks::session_git_decision`, and it exists because the first one is
# advice.** A PreToolUse hook is only ever as good as the harness that chooses to load it; a git
# hook runs whatever ran git — the VSCode git panel, a plain terminal, another agent's tool, a
# script. Measured on 2026-09-05: the client-side hook fired correctly in an editor session and the
# merge still landed, because the DAEMON allowed a spelling its parsers could not read. That hole is
# closed in `vcs::unqueueable_but_shared`; this layer is what catches the clients that never ask.
#
# **It is a convention, not a boundary.** Anything that can run git can set an environment variable,
# so this stops nobody who means to get past it. What it stops is the case that actually happened —
# a client performing a queue operation because nothing told it not to.
queue_guard() {
  what="$1"
  spelling="$2"
  cleanup="$3"

  # The queue marks every git process it runs; see QUEUE_MARKER in core/src/git_exec.rs. Without
  # this the queue would refuse itself: `compute_merge` runs its merge in the integration worktree,
  # and `publish` and the push run in the project root.
  if [ -n "$NUCLEOS_QUEUE_EXEC" ]; then
    return 0
  fi

  # The queue's own structural signal, and the reason it is here rather than the marker alone:
  # `compute_merge` performs every merge in a worktree named `integration-<project>` (see
  # INTEGRATION_PREFIX in core/src/git_exec.rs), and nothing else has any business merging there.
  # The marker is an environment variable, so it is lost by any daemon built before it existed —
  # this arm is what keeps the queue from refusing itself across that upgrade, and it keeps working
  # if the variable is ever dropped by something in between.
  case "$(git rev-parse --show-toplevel 2>/dev/null)" in
    */integration-*) return 0 ;;
  esac

  # A person acting as themselves, deliberately and in one command. The queue is for ordering work
  # between sessions; a human who has decided to step around it should not have to edit a hook.
  if [ -n "$NUCLEOS_ALLOW_DIRECT_GIT" ]; then
    return 0
  fi

  echo "blocked: a $what goes through the VCS queue, not by hand." >&2
  echo "" >&2
  echo "  the queue orders this operation between sessions. Performing it directly is what the" >&2
  echo "  queue exists to prevent, and it is invisible to every other session while it happens." >&2
  echo "" >&2
  echo "  to deliver a branch:    nucleos-core --land" >&2
  echo "  the queueable spelling: $spelling" >&2
  echo "" >&2
  echo "  to act as yourself, deliberately:  NUCLEOS_ALLOW_DIRECT_GIT=1 git ..." >&2

  # `pre-merge-commit` runs after the merge is computed and staged, so refusing there leaves
  # MERGE_HEAD and a merged index behind. That is recoverable and the recovery is not obvious,
  # so it is named here rather than left to be discovered.
  if [ -n "$cleanup" ]; then
    echo "" >&2
    echo "  this left the merge half-applied. To undo it:  $cleanup" >&2
  fi

  exit 1
}
HOOK_EOF

if [ "$mode" = "check" ]; then
  if diff -r "$target" "$hooks_dir" >/dev/null 2>&1; then
    echo "hooks: installed copies match scripts/install-hooks.sh"
    exit 0
  fi
  echo "hooks: .githooks/ differs from scripts/install-hooks.sh — re-run it, or port the change here first:" >&2
  diff -r "$target" "$hooks_dir" >&2 || true
  exit 1
fi

# nucleos-queue-guard is sourced, not run, so it is deliberately left non-executable.
chmod +x "$target"/pre-commit "$target"/commit-msg "$target"/pre-merge-commit \
  "$target"/pre-push "$target"/pre-rebase

git config core.hooksPath "$hooks_dir"

echo "installed: .githooks/{pre-commit,commit-msg,pre-merge-commit,pre-push,pre-rebase,nucleos-queue-guard}"
echo "core.hooksPath = $(git config core.hooksPath)"
