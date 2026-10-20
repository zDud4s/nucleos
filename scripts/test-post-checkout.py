#!/usr/bin/env python3
"""Cover the post-checkout hook `scripts/install-hooks.sh` writes: a worktree made by hand asks the daemon for its workflow.

The daemon materializes the pinned workflows into the trees it opens itself
(`workflows::materialize::into_worktree`), and `nucleos-core --workflow-sync <tree>` does the same
for any other tree -- but nothing called it, so a `git worktree add` by hand produced a checkout
with no `.nucleos/workflow`, no skills and no cockpits. The hook calls it on the one checkout that
creates a tree, and on no other.

What is asserted is what a mistake would cost: a hand-made tree must be synced; a tree the daemon
opened (its git carries NUCLEOS_QUEUE_EXEC) must not be, or the merge trees the queue resets would
get untracked rules beside the commit they measure; a branch switch must not call anything; and a
missing or failing daemon must never fail the `git worktree add` that ran the hook.

Temp repositories and a fake `nucleos-core` script only -- the real binary is never run.

Run:  python scripts/test-post-checkout.py
"""

import os
import subprocess
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
INSTALLER = ROOT / "scripts" / "install-hooks.sh"


def hook_text():
    """The post-checkout body exactly as scripts/install-hooks.sh writes it: `.githooks/` is
    gitignored, so the installer is the only committed copy, and the one worth testing."""
    lines = INSTALLER.read_text(encoding="utf-8").splitlines(keepends=True)
    start = lines.index("cat > \"$target/post-checkout\" <<'HOOK_EOF'\n") + 1
    end = lines.index("HOOK_EOF\n", start)
    return "".join(lines[start:end])

FAKE_OK = '#!/bin/sh\necho "$@" >> "$(dirname "$0")/calls.log"\n'
FAKE_FAILING = '#!/bin/sh\necho "$@" >> "$(dirname "$0")/calls.log"\necho refused >&2\nexit 1\n'

failures = []


def git(cwd, *args, env=None):
    return subprocess.run(["git", *args], cwd=cwd, env=env, capture_output=True, text=True)


def check(name, condition, detail=""):
    if not condition:
        failures.append(f"{name}: {detail}")


def make_repo(temp):
    repo = temp / "repo"
    repo.mkdir()
    git(repo, "init", "-q", "-b", "master")
    git(repo, "config", "user.email", "t@example.com")
    git(repo, "config", "user.name", "t")
    (repo / "a.txt").write_text("a\n")
    git(repo, "add", "a.txt")
    git(repo, "commit", "-q", "-m", "init")
    hooks = temp / "hooks"
    hooks.mkdir()
    hook = hooks / "post-checkout"
    hook.write_text(hook_text(), newline="\n")
    hook.chmod(0o755)
    git(repo, "config", "core.hooksPath", hooks.as_posix())
    return repo


def make_home(temp, body):
    run = temp / "home" / "run"
    run.mkdir(parents=True)
    fake = run / "nucleos-core"
    fake.write_text(body, newline="\n")
    fake.chmod(0o755)
    return temp / "home", run / "calls.log"


def env_with(home, **extra):
    env = {k: v for k, v in os.environ.items() if k != "NUCLEOS_QUEUE_EXEC"}
    env["NUCLEOS_DEV_HOME"] = str(home)
    env.update(extra)
    return env


def calls(log):
    return log.read_text().splitlines() if log.exists() else []


def test_a_hand_made_worktree_is_synced():
    with tempfile.TemporaryDirectory() as raw:
        temp = Path(raw)
        repo = make_repo(temp)
        home, log = make_home(temp, FAKE_OK)
        added = git(repo, "worktree", "add", "-q", "-b", "b1", str(temp / "wt1"), env=env_with(home))
        check("hand-made: add succeeds", added.returncode == 0, added.stderr)
        seen = calls(log)
        check("hand-made: one sync", len(seen) == 1, repr(seen))
        check(
            "hand-made: names the new tree",
            bool(seen) and seen[0].startswith("--workflow-sync ") and seen[0].rstrip().endswith("/wt1"),
            repr(seen),
        )


def test_a_tree_the_daemon_opened_is_left_to_the_daemon():
    with tempfile.TemporaryDirectory() as raw:
        temp = Path(raw)
        repo = make_repo(temp)
        home, log = make_home(temp, FAKE_OK)
        env = env_with(home, NUCLEOS_QUEUE_EXEC="1")
        added = git(repo, "worktree", "add", "-q", "-b", "b2", str(temp / "integration-x"), env=env)
        check("daemon tree: add succeeds", added.returncode == 0, added.stderr)
        check("daemon tree: no sync", calls(log) == [], repr(calls(log)))


def test_a_branch_switch_calls_nothing():
    with tempfile.TemporaryDirectory() as raw:
        temp = Path(raw)
        repo = make_repo(temp)
        home, log = make_home(temp, FAKE_OK)
        switched = git(repo, "checkout", "-q", "-b", "b3", env=env_with(home))
        check("switch: succeeds", switched.returncode == 0, switched.stderr)
        check("switch: no sync", calls(log) == [], repr(calls(log)))


def test_no_daemon_binary_never_fails_the_add():
    with tempfile.TemporaryDirectory() as raw:
        temp = Path(raw)
        repo = make_repo(temp)
        empty = temp / "empty-home"
        (empty / "run").mkdir(parents=True)
        added = git(repo, "worktree", "add", "-q", "-b", "b4", str(temp / "wt4"), env=env_with(empty))
        check("no binary: add succeeds", added.returncode == 0, added.stderr)
        check("no binary: says so", "workflow" in added.stderr, repr(added.stderr))


def test_a_refusing_daemon_never_fails_the_add():
    with tempfile.TemporaryDirectory() as raw:
        temp = Path(raw)
        repo = make_repo(temp)
        home, log = make_home(temp, FAKE_FAILING)
        added = git(repo, "worktree", "add", "-q", "-b", "b5", str(temp / "wt5"), env=env_with(home))
        check("refused: add succeeds", added.returncode == 0, added.stderr)
        check("refused: it did ask", len(calls(log)) == 1, repr(calls(log)))
        check("refused: says so", "workflow" in added.stderr, repr(added.stderr))


if __name__ == "__main__":
    for name, test in list(globals().items()):
        if name.startswith("test_") and callable(test):
            test()
    if failures:
        print("\n".join(failures))
        sys.exit(1)
    print("post-checkout: ok")
