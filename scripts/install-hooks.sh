#!/usr/bin/env bash
# Recreate this repo's local git hooks and point git at them.
#
# The hooks live in .githooks/, which .gitignore excludes, and git finds them through
# core.hooksPath, which is *local* config. Neither travels with a clone. That is deliberate — they
# are local tooling, not part of the product — but it leaves every fresh clone and every new
# machine silently unguarded, indistinguishable from a guarded one right up to the commit that
# should have been blocked. So the recipe is committed even though the artefact is not.
#
# Idempotent: re-running overwrites the hooks with the versions below and re-points the config.
#
# Usage: scripts/install-hooks.sh
set -euo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$repo_root"

hooks_dir="$repo_root/.githooks"
mkdir -p "$hooks_dir"

cat > "$hooks_dir/pre-commit" <<'HOOK'
#!/bin/sh
# Reject commits that stage files under the workflow-local, gitignored directories.
forbidden_re='^(\.ai|\.claude|\.agents)/'
# Except these two, which are NucleOS rather than workflow footprint: core/src/autopilot.rs reads
# settings.json and will not activate autopilot unless it registers ask_daemon.py and that file
# exists. Blocking them made the security hook uncommittable — editable, never fixable.
# .gitignore carves out the same two paths; change one and change the other.
allowed_re='^\.claude/(settings\.json|hooks/ask_daemon\.py)$'
bad="$(git diff --cached --name-only | grep -E "$forbidden_re" | grep -vE "$allowed_re")"

if [ -n "$bad" ]; then
  echo "commit blocked: staged file(s) under .ai/, .claude/, or .agents/ (must stay untracked):" >&2
  printf '%s\n' "$bad" >&2
  echo "unstage with: git restore --staged <file>" >&2
  exit 1
fi
HOOK

cat > "$hooks_dir/commit-msg" <<'HOOK'
#!/bin/sh
# Reject commits whose message carries a Co-Authored-By trailer.
# This repo's convention is no co-authors on commits, from any agent or tool.
msg_file="$1"

if grep -qiE '^co-authored-by:' "$msg_file"; then
  echo "commit blocked: message contains a Co-Authored-By trailer (not used in this repo)." >&2
  echo "remove the trailer and retry the commit." >&2
  exit 1
fi
HOOK

cat > "$hooks_dir/pre-push" <<'HOOK'
#!/bin/sh
# Run the gates before anything leaves this machine.
#
# Dormant while the repo has no remote — `git push` never happens, so this never fires. It becomes
# the enforcement point the moment one is added, which is why it is written now rather than
# remembered later.
#
# Escape hatch for a deliberate push of known-red work:  NUCLEOS_SKIP_GATES=1 git push
if [ "${NUCLEOS_SKIP_GATES:-0}" = "1" ]; then
  echo "pre-push: gates skipped (NUCLEOS_SKIP_GATES=1)" >&2
  exit 0
fi

repo_root="$(git rev-parse --show-toplevel)"
# Invoked through bash rather than executed: everything under scripts/ is mode 100644 in this
# repo, so exec'ing the file directly would fail anywhere the executable bit is honoured.
exec bash "$repo_root/scripts/gates.sh" all
HOOK

chmod +x "$hooks_dir"/pre-commit "$hooks_dir"/commit-msg "$hooks_dir"/pre-push

git config core.hooksPath .githooks

echo "installed: .githooks/{pre-commit,commit-msg,pre-push}"
echo "core.hooksPath = $(git config core.hooksPath)"
