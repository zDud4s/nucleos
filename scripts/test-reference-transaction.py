#!/usr/bin/env python3
"""Cover the reference-transaction hook `scripts/install-hooks.sh` writes: master is closed.

Nothing but the queue moves `refs/heads/<integration branch>`. The hook refuses every other writer
of that one ref -- a plain commit, `reset --hard`, `update-ref`, `branch -f`, a fast-forward merge
-- and `--no-verify` does not skip it. The queue's own git processes carry NUCLEOS_QUEUE_EXEC and
pass silently; a person who sets NUCLEOS_ALLOW_DIRECT_GIT=1 passes too, but leaves a `bypass` line
in `<git-common-dir>/nucleos-direct-writes.log` for the daemon to read. Every other ref, a detached
HEAD and a fetch are untouched, and a repository with no `nucleos.integrationBranch` is not guarded.

The installer is covered too: it writes the hook and derives the target into the repository config.

Temp repositories only -- no real repository's config or hooks are touched.

Run:  python scripts/test-reference-transaction.py
"""

import os
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
INSTALLER = ROOT / "scripts" / "install-hooks.sh"
LOG_NAME = "nucleos-direct-writes.log"


def hook_text():
    """The reference-transaction body exactly as scripts/install-hooks.sh writes it: `.githooks/`
    is gitignored, so the installer is the only committed copy, and the one worth testing."""
    lines = INSTALLER.read_text(encoding="utf-8").splitlines(keepends=True)
    start = lines.index("cat > \"$target/reference-transaction\" <<'HOOK_EOF'\n") + 1
    end = lines.index("HOOK_EOF\n", start)
    return "".join(lines[start:end])


failures = []

# Variables that would change what the hook or git sees; the tests set them explicitly.
SCRUBBED = (
    "NUCLEOS_QUEUE_EXEC",
    "NUCLEOS_ALLOW_DIRECT_GIT",
    "NUCLEOS_INTEGRATION_BRANCH",
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_COMMON_DIR",
    "GIT_INDEX_FILE",
)


def env_with(**extra):
    env = {k: v for k, v in os.environ.items() if k not in SCRUBBED}
    env.update(extra)
    return env


def git(cwd, *args, env=None):
    return subprocess.run(
        ["git", *args], cwd=cwd, env=env if env is not None else env_with(), capture_output=True, text=True
    )


def rev(repo, ref):
    return git(repo, "rev-parse", "--verify", "--quiet", ref).stdout.strip()


def check(name, condition, detail=""):
    if not condition:
        failures.append(f"{name}: {detail}")


def commit_file(repo, name, env=None, *extra):
    (repo / name).write_text(name + "\n")
    git(repo, "add", name, env=env)
    return git(repo, "commit", "-q", *extra, "-m", name, env=env)


def make_repo(temp, target="master"):
    """A repo on master with one commit and a branch `other` one commit ahead, then the hook armed
    (when `target` is set). The setup commits happen before the hook exists."""
    repo = temp / "repo"
    repo.mkdir()
    git(repo, "init", "-q", "-b", "master")
    git(repo, "config", "user.email", "t@example.com")
    git(repo, "config", "user.name", "t")
    commit_file(repo, "a.txt")
    git(repo, "checkout", "-q", "-b", "other")
    commit_file(repo, "b.txt")
    git(repo, "checkout", "-q", "master")
    hooks = temp / "hooks"
    hooks.mkdir()
    hook = hooks / "reference-transaction"
    hook.write_text(hook_text(), newline="\n")
    hook.chmod(0o755)
    git(repo, "config", "core.hooksPath", hooks.as_posix())
    if target:
        git(repo, "config", "nucleos.integrationBranch", target)
    return repo


def log_lines(repo):
    log = repo / ".git" / LOG_NAME
    return log.read_text().splitlines() if log.exists() else []


def test_direct_commit_on_target_is_refused():
    with tempfile.TemporaryDirectory() as raw:
        repo = make_repo(Path(raw))
        before = rev(repo, "master")
        done = commit_file(repo, "c.txt")
        check("direct commit: refused", done.returncode != 0, done.stderr)
        check("direct commit: says why", "blocked" in done.stderr, repr(done.stderr))
        check("direct commit: ref intact", rev(repo, "master") == before, rev(repo, "master"))
        skipped = git(repo, "commit", "-q", "--no-verify", "-m", "c2")
        check("--no-verify: still refused", skipped.returncode != 0, skipped.stderr)
        check("--no-verify: ref intact", rev(repo, "master") == before, rev(repo, "master"))
        check("direct commit: nothing logged", log_lines(repo) == [], repr(log_lines(repo)))


def test_other_writers_of_the_target_are_refused():
    cases = {
        "reset --hard": lambda repo: git(repo, "reset", "-q", "--hard", "other"),
        "update-ref": lambda repo: git(repo, "update-ref", "refs/heads/master", rev(repo, "other")),
        "branch -f": lambda repo: (git(repo, "checkout", "-q", "other"), git(repo, "branch", "-f", "master", "other"))[1],
        "merge --ff-only": lambda repo: git(repo, "merge", "-q", "--ff-only", "other"),
        "branch -D": lambda repo: (git(repo, "checkout", "-q", "other"), git(repo, "branch", "-D", "master"))[1],
    }
    for label, write in cases.items():
        with tempfile.TemporaryDirectory() as raw:
            repo = make_repo(Path(raw))
            before = rev(repo, "master")
            done = write(repo)
            check(f"{label}: refused", done.returncode != 0, done.stderr)
            check(f"{label}: ref intact", rev(repo, "master") == before, rev(repo, "master") or "<deleted>")


def test_other_branches_detached_head_and_fetch_pass():
    with tempfile.TemporaryDirectory() as raw:
        temp = Path(raw)
        repo = make_repo(temp)
        master = rev(repo, "master")
        git(repo, "checkout", "-q", "other")
        before = rev(repo, "other")
        done = commit_file(repo, "c.txt")
        check("other branch: commit passes", done.returncode == 0, done.stderr)
        check("other branch: moved", rev(repo, "other") != before, rev(repo, "other"))

        git(repo, "checkout", "-q", "--detach", "master")
        done = commit_file(repo, "d.txt")
        check("detached: commit passes", done.returncode == 0, done.stderr)
        check("detached: master intact", rev(repo, "master") == master, rev(repo, "master"))

        # An upstream whose master is ahead: fetching writes refs/remotes/origin/*, never the target.
        upstream = temp / "upstream"
        cloned = git(temp, "clone", "-q", str(repo), str(upstream))
        check("fetch: clone for upstream", cloned.returncode == 0, cloned.stderr)
        git(upstream, "config", "user.email", "t@example.com")
        git(upstream, "config", "user.name", "t")
        git(upstream, "checkout", "-q", "master")
        commit_file(upstream, "e.txt")
        git(repo, "remote", "add", "origin", str(upstream))
        fetched = git(repo, "fetch", "-q", "origin")
        check("fetch: passes", fetched.returncode == 0, fetched.stderr)
        check("fetch: remote ref updated", rev(repo, "refs/remotes/origin/master") == rev(upstream, "master"), fetched.stderr)
        check("fetch: master intact", rev(repo, "master") == master, rev(repo, "master"))
        check("passing writes: nothing logged", log_lines(repo) == [], repr(log_lines(repo)))


def test_queue_marker_lets_the_write_through_unlogged():
    with tempfile.TemporaryDirectory() as raw:
        repo = make_repo(Path(raw))
        before = rev(repo, "master")
        env = env_with(NUCLEOS_QUEUE_EXEC="1")
        done = git(repo, "update-ref", "refs/heads/master", rev(repo, "other"), before, env=env)
        check("queue: update-ref passes", done.returncode == 0, done.stderr)
        check("queue: master moved", rev(repo, "master") == rev(repo, "other"), rev(repo, "master"))
        git(repo, "reset", "-q", "--hard", "master", env=env)
        done = commit_file(repo, "c.txt", env)
        check("queue: commit passes", done.returncode == 0, done.stderr)
        check("queue: nothing logged", log_lines(repo) == [], repr(log_lines(repo)))


def test_escape_passes_and_is_logged_as_bypass():
    with tempfile.TemporaryDirectory() as raw:
        repo = make_repo(Path(raw))
        before = rev(repo, "master")
        done = commit_file(repo, "c.txt", env_with(NUCLEOS_ALLOW_DIRECT_GIT="1"))
        after = rev(repo, "master")
        check("escape: commit passes", done.returncode == 0, done.stderr)
        check("escape: master moved", after != before, after)
        lines = log_lines(repo)
        check("escape: one line logged", len(lines) == 1, repr(lines))
        if lines:
            parts = lines[0].split()
            check(
                "escape: line shape",
                len(parts) == 5 and parts[0] == "bypass" and parts[1].isdigit(),
                repr(lines[0]),
            )
            check(
                "escape: old, new, ref",
                parts[2:] == [before, after, "refs/heads/master"],
                repr(lines[0]),
            )


def test_no_configured_target_is_a_no_op():
    with tempfile.TemporaryDirectory() as raw:
        repo = make_repo(Path(raw), target=None)
        before = rev(repo, "master")
        done = commit_file(repo, "c.txt")
        check("no target: commit passes", done.returncode == 0, done.stderr)
        check("no target: master moved", rev(repo, "master") != before, rev(repo, "master"))
        check("no target: nothing logged", log_lines(repo) == [], repr(log_lines(repo)))


def git_bash():
    """Git's own bash: on Windows the `bash` on PATH is WSL, which sees neither these paths nor git."""
    chosen = os.environ.get("NUCLEOS_GIT_BASH")
    if chosen:
        return chosen
    default = Path("C:/Program Files/Git/bin/bash.exe")
    if default.exists():
        return str(default)
    return "bash"


def installed_repo(temp, name):
    """A repo with one master commit, ready for `run_installer`."""
    repo = temp / name
    repo.mkdir()
    git(repo, "init", "-q", "-b", "master")
    git(repo, "config", "user.email", "t@example.com")
    git(repo, "config", "user.name", "t")
    commit_file(repo, "a.txt")
    return repo


def run_installer(repo, **env_extra):
    (repo / "scripts").mkdir(exist_ok=True)
    shutil.copy(INSTALLER, repo / "scripts" / "install-hooks.sh")
    return subprocess.run(
        [git_bash(), "scripts/install-hooks.sh"],
        cwd=repo,
        env=env_with(**env_extra),
        capture_output=True,
        text=True,
    )


def config(repo, key):
    return git(repo, "config", "--get", key).stdout.strip()


def test_installer_writes_config_and_hook():
    with tempfile.TemporaryDirectory() as raw:
        temp = Path(raw)

        repo = installed_repo(temp, "plain")
        done = run_installer(repo)
        check("installer: runs", done.returncode == 0, done.stdout + done.stderr)
        check("installer: master by default", config(repo, "nucleos.integrationBranch") == "master",
              config(repo, "nucleos.integrationBranch"))
        hook = repo / ".githooks" / "reference-transaction"
        check("installer: hook written", hook.exists(), str(hook))
        if hook.exists():
            check("installer: hook text", hook.read_text(encoding="utf-8") == hook_text(), "differs")
        check("installer: listed", "reference-transaction" in done.stdout, done.stdout)
        # The installed hook is live in this temp repo: a direct commit on master is refused.
        before = rev(repo, "master")
        refused = commit_file(repo, "z.txt")
        check("installer: hook armed", refused.returncode != 0 and rev(repo, "master") == before, refused.stderr)

        repo = installed_repo(temp, "main-only")
        git(repo, "branch", "-m", "master", "main")
        done = run_installer(repo)
        check("installer: main fallback runs", done.returncode == 0, done.stdout + done.stderr)
        check("installer: main fallback", config(repo, "nucleos.integrationBranch") == "main",
              config(repo, "nucleos.integrationBranch"))

        repo = installed_repo(temp, "remote-head")
        git(repo, "symbolic-ref", "refs/remotes/origin/HEAD", "refs/remotes/origin/develop")
        done = run_installer(repo)
        check("installer: origin/HEAD runs", done.returncode == 0, done.stdout + done.stderr)
        check("installer: origin/HEAD wins over master", config(repo, "nucleos.integrationBranch") == "develop",
              config(repo, "nucleos.integrationBranch"))

        repo = installed_repo(temp, "override")
        git(repo, "symbolic-ref", "refs/remotes/origin/HEAD", "refs/remotes/origin/develop")
        done = run_installer(repo, NUCLEOS_INTEGRATION_BRANCH="trunk")
        check("installer: override runs", done.returncode == 0, done.stdout + done.stderr)
        check("installer: override wins", config(repo, "nucleos.integrationBranch") == "trunk",
              config(repo, "nucleos.integrationBranch"))


if __name__ == "__main__":
    for name, test in list(globals().items()):
        if name.startswith("test_") and callable(test):
            test()
    if failures:
        print("\n".join(failures))
        sys.exit(1)
    print("reference-transaction: ok")
