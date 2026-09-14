#!/usr/bin/env python3
"""Cover `scripts/own-cargo-target.sh`: a cargo target directory per checkout, and only its own.

Job 27's second gate, 2026-09-14, ran a test binary compiled in another worktree: every checkout
the daemon gates shared one CARGO_TARGET_DIR, and cargo reuses a workspace member's artifact across
checkouts whenever none of the second checkout's sources is newer. The script keys a directory by
the checkout's path and prunes the ones nobody has built in a day. What is asserted here is what a
mistake in it would cost: two checkouts must never share a directory, and pruning must never touch
one the script did not create.

Temp directories only — never C:/Projects, never a real target tree, and nothing is built.

**Under a POSIX bash, never the `bash` on PATH on Windows**, which is WSL: see
`scripts/test-gates-summary.py`. `GIT_BASH` overrides the search.

Run:  python scripts/test-own-cargo-target.py
"""

import os
import re
import shutil
import subprocess
import sys
import tempfile
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
SCRIPT = ROOT / "scripts" / "own-cargo-target.sh"
MARKER = ".own-cargo-target"
# The command every case runs: it prints what it was handed. Plain `bash`, as the documented gate
# line has it, so this also shows that name resolving to a working POSIX bash inside the script.
REPORT = ["bash", "-c", 'printf "%s" "$CARGO_TARGET_DIR"']


def posix_bash() -> str:
    override = os.environ.get("GIT_BASH")
    if override:
        return override
    if os.name != "nt":
        found = shutil.which("bash")
        if found:
            return found
        sys.exit("no bash on PATH")
    candidates = []
    git = shutil.which("git")
    if git:
        here = Path(git).resolve().parent
        candidates += [folder / "bin" / "bash.exe" for folder in (here.parent, here.parent.parent)]
    candidates.append(Path("C:/Program Files/Git/bin/bash.exe"))
    for candidate in candidates:
        if candidate.is_file():
            return str(candidate)
    sys.exit("Git's bash.exe not found; set GIT_BASH to it (not the bash on PATH, which is WSL)")


BASH = posix_bash()


def own(cwd: Path, *arguments: str, env: dict | None = None) -> subprocess.CompletedProcess:
    return subprocess.run(
        [BASH, SCRIPT.as_posix(), *arguments],
        cwd=cwd,
        capture_output=True,
        text=True,
        env=env if env is not None else dict(os.environ),
        timeout=60,
    )


def same(a, b) -> bool:
    return os.path.normcase(os.path.abspath(a)) == os.path.normcase(os.path.abspath(b))


def age(path: Path, hours: float) -> None:
    then = time.time() - hours * 3600
    os.utime(path, (then, then))


def checkouts(tmp: Path) -> tuple[Path, Path, str]:
    # The same basename under two parents: the name alone must not be what tells them apart.
    one = tmp / "one" / "checkout"
    two = tmp / "two" / "checkout"
    one.mkdir(parents=True)
    two.mkdir(parents=True)
    return one, two, (tmp / "targets").as_posix()


def test_each_checkout_gets_its_own_directory_and_keeps_it() -> None:
    with tempfile.TemporaryDirectory() as tmp:
        one, two, root = checkouts(Path(tmp))
        first = own(one, root, *REPORT)
        other = own(two, root, *REPORT)
        again = own(one, root, *REPORT)
        for done in (first, other, again):
            assert done.returncode == 0, (done.returncode, done.stderr)

        assert not same(first.stdout, other.stdout), (first.stdout, other.stdout)
        assert same(first.stdout, again.stdout), (first.stdout, again.stdout)
        for directory in (Path(first.stdout), Path(other.stdout)):
            assert same(directory.parent, root), directory
            assert re.fullmatch(r"checkout-[0-9a-f]{8}", directory.name), directory.name
            assert (directory / MARKER).is_file(), f"no marker in {directory}"


def test_the_command_gets_its_arguments_its_directory_and_its_status() -> None:
    with tempfile.TemporaryDirectory() as tmp:
        one, _, root = checkouts(Path(tmp))
        done = own(one, root, "bash", "-c", 'printf "%s|" "$@"; exit 7', "_", "a b", "c")
        assert done.returncode == 7, (done.returncode, done.stderr)
        assert done.stdout == "a b|c|", done.stdout

        # A CARGO_TARGET_DIR the caller already set — the gate line used to carry one — loses.
        expected = own(one, root, *REPORT).stdout
        elsewhere = (Path(tmp) / "elsewhere").as_posix()
        done = own(one, root, *REPORT, env=dict(os.environ, CARGO_TARGET_DIR=elsewhere))
        assert done.returncode == 0, done.stderr
        assert same(done.stdout, expected), (done.stdout, expected)


def test_pruning_takes_only_what_the_script_made_and_not_this_checkouts() -> None:
    with tempfile.TemporaryDirectory() as tmp:
        one, _, root = checkouts(Path(tmp))
        mine = Path(own(one, root, *REPORT).stdout)
        (mine / "keep.me").write_text("built here")
        age(mine / MARKER, 25)

        base = Path(root)
        stale = base / "gone-0000aaaa"
        (stale / "debug").mkdir(parents=True)
        (stale / "debug" / "artifact").write_text("x")
        (stale / MARKER).touch()
        age(stale / MARKER, 25)

        fresh = base / "kept-0000bbbb"
        fresh.mkdir()
        (fresh / MARKER).touch()
        age(fresh / MARKER, 1)

        unmarked = base / "unmarked"
        unmarked.mkdir()
        (unmarked / "file").write_text("not the script's")
        age(unmarked / "file", 25)
        age(unmarked, 25)

        loose = base / "notes.txt"
        loose.write_text("not the script's either")
        age(loose, 25)

        deep = base / "nest" / "inner"
        deep.mkdir(parents=True)
        (deep / MARKER).touch()
        age(deep / MARKER, 25)

        done = own(one, root, *REPORT)
        assert done.returncode == 0, done.stderr
        assert same(done.stdout, mine), (done.stdout, mine)

        assert not stale.exists(), "a marked directory idle for over a day was kept"
        assert "pruned" in done.stderr and "gone-0000aaaa" in done.stderr, done.stderr
        assert fresh.is_dir() and (fresh / MARKER).is_file(), "a freshly marked sibling was pruned"
        assert (unmarked / "file").is_file(), "a directory without the marker was pruned"
        assert loose.is_file(), "a file under the root was removed"
        assert (deep / MARKER).is_file(), "a marker deeper than one level was acted on"
        assert (mine / "keep.me").is_file(), "this checkout's own directory was pruned"
        assert time.time() - (mine / MARKER).stat().st_mtime < 3600, "the marker was not refreshed"


def test_a_missing_or_relative_root_is_refused_before_anything_is_made() -> None:
    with tempfile.TemporaryDirectory() as tmp:
        one, _, root = checkouts(Path(tmp))
        for arguments in ([], [root], ["targets", *REPORT], ["relative/targets", *REPORT]):
            done = own(one, *arguments)
            assert done.returncode == 2, (arguments, done.returncode, done.stderr)
            assert "usage:" in done.stderr, (arguments, done.stderr)
            assert done.stdout == "", (arguments, "the command ran anyway", done.stdout)
        assert list(one.iterdir()) == [], list(one.iterdir())
        assert not Path(root).exists(), "a refused call still created the root"


test_each_checkout_gets_its_own_directory_and_keeps_it()
test_the_command_gets_its_arguments_its_directory_and_its_status()
test_pruning_takes_only_what_the_script_made_and_not_this_checkouts()
test_a_missing_or_relative_root_is_refused_before_anything_is_made()
print("own cargo target: ok")
