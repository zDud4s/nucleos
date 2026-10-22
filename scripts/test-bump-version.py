#!/usr/bin/env python3
"""Cover `scripts/bump-version.py`: one `VERSION` file, seven manifests that must agree with it.

The shell, the nucleo and the lock files each carry the same version in a different syntax. A
release whose installer says 0.2.0 while the daemon inside says 0.1.0 would defeat the version
handshake, so the bump script writes all of them or none, and `--check` names the one that drifted.

Every case copies the REAL files into a temporary root and drives the script through a subprocess,
so a change to a manifest layout that the script no longer understands fails here, not at release.

Run:  python scripts/test-bump-version.py
"""

import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
SCRIPT = ROOT / "scripts" / "bump-version.py"
FILES = [
    "VERSION",
    "core/Cargo.toml",
    "core/crates/nucleos-daemon/Cargo.toml",
    "shell/src-tauri/Cargo.toml",
    "shell/package.json",
    "shell/src-tauri/tauri.conf.json",
    "Cargo.lock",
    "shell/src-tauri/Cargo.lock",
    "shell/package-lock.json",
]


def temp_root(crlf: bool = False) -> Path:
    tmp = Path(tempfile.mkdtemp(prefix="bump-version-"))
    for rel in FILES:
        dest = tmp / rel
        dest.parent.mkdir(parents=True, exist_ok=True)
        data = (ROOT / rel).read_bytes().replace(b"\r\n", b"\n")
        if crlf:
            data = data.replace(b"\n", b"\r\n")
        dest.write_bytes(data)
    return tmp


def run(root: Path, *args: str) -> subprocess.CompletedProcess:
    return subprocess.run(
        [sys.executable, str(SCRIPT), "--root", str(root), *args],
        capture_output=True,
        text=True,
    )


def snapshot(root: Path) -> dict:
    return {rel: (root / rel).read_bytes() for rel in FILES}


def changed_lines(before: bytes, after: bytes) -> int:
    a = before.splitlines(keepends=True)
    b = after.splitlines(keepends=True)
    assert len(a) == len(b), "line count changed"
    return sum(1 for x, y in zip(a, b) if x != y)


def current_version() -> str:
    return (ROOT / "VERSION").read_text(encoding="utf-8").strip()


def bump_writes_every_manifest_and_nothing_else():
    for crlf in (False, True):
        root = temp_root(crlf)
        try:
            before = snapshot(root)
            res = run(root, "9.8.7")
            assert res.returncode == 0, res.stderr
            after = snapshot(root)
            assert after["VERSION"].decode().strip() == "9.8.7"
            for rel in FILES[1:]:
                want = 2 if rel in ("shell/package-lock.json", "Cargo.lock") else 1
                got = changed_lines(before[rel], after[rel])
                assert got == want, f"{rel}: {got} lines changed, wanted {want} (crlf={crlf})"
                assert b"9.8.7" in after[rel]
                if crlf:
                    assert b"\n" not in after[rel].replace(b"\r\n", b""), f"{rel} lost its CRLF"
            assert run(root, "--check").returncode == 0
        finally:
            shutil.rmtree(root, ignore_errors=True)


def check_names_the_file_that_disagrees():
    root = temp_root()
    try:
        assert run(root, "--check").returncode == 0
        assert run(root, "--check", "--expect", current_version()).returncode == 0
        pkg = root / "shell/package.json"
        pkg.write_text(pkg.read_text(encoding="utf-8").replace(f'"version": "{current_version()}"', '"version": "7.7.7"', 1), encoding="utf-8")
        res = run(root, "--check")
        assert res.returncode == 1, res.returncode
        assert "shell/package.json" in res.stderr, res.stderr
        assert "core/Cargo.toml" not in res.stderr, res.stderr
        # `--expect` compares VERSION itself with the tag, whatever the manifests say.
        res = run(root, "--check", "--expect", "3.3.3")
        assert res.returncode == 1 and "3.3.3" in res.stderr, res.stderr
    finally:
        shutil.rmtree(root, ignore_errors=True)


def bad_version_or_missing_anchor_writes_nothing():
    root = temp_root()
    try:
        before = snapshot(root)
        for bad in ("1.2", "v1.2.3", "01.2.3", "1.2.3.4", ""):
            res = run(root, bad)
            assert res.returncode == 2, f"{bad!r}: exit {res.returncode}"
            assert snapshot(root) == before, f"{bad!r} wrote something"
        # An anchor that vanished is a failure, and the manifests earlier in the table stay untouched.
        lock = root / "shell/src-tauri/Cargo.lock"
        lock.write_text(lock.read_text(encoding="utf-8").replace('name = "shell"', 'name = "renamed"'), encoding="utf-8")
        before = snapshot(root)
        res = run(root, "5.5.5")
        assert res.returncode == 1, res.returncode
        assert "shell/src-tauri/Cargo.lock" in res.stderr, res.stderr
        assert snapshot(root) == before, "a failed bump left a partial write"
    finally:
        shutil.rmtree(root, ignore_errors=True)


def a_prerelease_version_is_accepted():
    root = temp_root()
    try:
        assert run(root, "1.0.0-rc.1").returncode == 0
        assert run(root, "--check", "--expect", "1.0.0-rc.1").returncode == 0
    finally:
        shutil.rmtree(root, ignore_errors=True)


CASES = [
    bump_writes_every_manifest_and_nothing_else,
    check_names_the_file_that_disagrees,
    bad_version_or_missing_anchor_writes_nothing,
    a_prerelease_version_is_accepted,
]

if __name__ == "__main__":
    failed = 0
    for case in CASES:
        try:
            case()
            print(f"ok   {case.__name__}")
        except Exception as exc:  # noqa: BLE001 - report every failing case, not just the first
            failed += 1
            print(f"FAIL {case.__name__}: {type(exc).__name__}: {exc}")
    sys.exit(1 if failed else 0)
