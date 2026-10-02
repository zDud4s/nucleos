#!/usr/bin/env python3
"""Cover `scripts/run-daemon.ps1`: the daemon runs from a copy, never out of a cargo target dir.

A daemon started with `cargo run` holds <target>/debug/nucleos-core.exe open, and every other build
on the machine that relinks it fails with os error 5 -- which pushed sessions onto private target
dirs and saturated CPU and disk on 2026-10-01. The script stages the daemon and its sidecars into
a stable runtime dir and runs that. What is asserted here is what a mistake in it would cost: a
staged copy must be complete and leave the source alone, a running copy must never be overwritten,
and a bad argument must be refused before anything is made.

Temp directories and fake executables only -- cargo is never invoked (every case passes
-TargetDir with -NoBuild) and nucleos-core.exe is never run. Windows PowerShell only.

Run:  python scripts/test-run-daemon.py
"""

import os
import shutil
import socket
import subprocess
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
SCRIPT = ROOT / "scripts" / "run-daemon.ps1"
SIDECARS = ["browser", "echo", "email", "quota", "telegram", "web"]

if os.name != "nt" or shutil.which("powershell.exe") is None:
    print("run daemon from copy: skipped (Windows only)")
    sys.exit(0)


def run(*args):
    return subprocess.run(
        ["powershell.exe", "-NoProfile", "-ExecutionPolicy", "Bypass", "-File", str(SCRIPT), *args],
        capture_output=True,
        text=True,
        timeout=120,
    )


def fake_target(base: Path) -> Path:
    """<base>/target with a debug dir of fake executables, each with distinct bytes."""
    debug = base / "target" / "debug"
    debug.mkdir(parents=True)
    for name in ["nucleos-core"] + [f"{s}-sidecar" for s in SIDECARS]:
        (debug / f"{name}.exe").write_bytes(b"fake " + name.encode())
    return base / "target"


def test_staging_copies_the_daemon_and_every_sidecar_and_touches_no_source():
    with tempfile.TemporaryDirectory() as tmp:
        base = Path(tmp)
        target = fake_target(base)
        run_dir = base / "run"
        before = {p.name: p.read_bytes() for p in (target / "debug").iterdir()}
        done = run("-TargetDir", str(target), "-RunDir", str(run_dir), "-NoBuild", "-NoStart")
        assert done.returncode == 0, (done.returncode, done.stdout, done.stderr)
        for name, data in before.items():
            assert (run_dir / name).read_bytes() == data, name
        assert (run_dir / "built-from.txt").is_file()
        assert len(list(run_dir.glob("*-sidecar.exe"))) == len(SIDECARS)
        after = {p.name: p.read_bytes() for p in (target / "debug").iterdir()}
        assert after == before, "the source directory changed"


def test_a_run_dir_exe_in_use_is_refused_and_left_alone():
    with tempfile.TemporaryDirectory() as tmp:
        base = Path(tmp)
        target = fake_target(base)
        run_dir = base / "run"
        first = run("-TargetDir", str(target), "-RunDir", str(run_dir), "-NoBuild", "-NoStart")
        assert first.returncode == 0, first.stderr
        held = run_dir / "nucleos-core.exe"
        (target / "debug" / "nucleos-core.exe").write_bytes(b"newer build")
        with open(held, "rb"):
            done = run("-TargetDir", str(target), "-RunDir", str(run_dir), "-NoBuild", "-NoStart")
        assert done.returncode == 3, (done.returncode, done.stdout, done.stderr)
        assert str(run_dir.resolve()).lower() in done.stderr.lower(), done.stderr
        assert "stop it first" in done.stderr, done.stderr
        assert held.read_bytes() == b"fake nucleos-core", "the run dir file changed"


def test_a_relative_or_nested_run_dir_is_refused_before_anything_is_made():
    with tempfile.TemporaryDirectory() as tmp:
        base = Path(tmp)
        target = fake_target(base)
        relative = run("-TargetDir", str(target), "-RunDir", "relative/run", "-NoBuild", "-NoStart")
        assert relative.returncode == 2, (relative.returncode, relative.stderr)
        assert "absolute" in relative.stderr, relative.stderr
        assert not Path("relative/run").exists()
        nested = target / "debug" / "run"
        inside = run("-TargetDir", str(target), "-RunDir", str(nested), "-NoBuild", "-NoStart")
        assert inside.returncode == 2, (inside.returncode, inside.stderr)
        assert "outside" in inside.stderr, inside.stderr
        assert not nested.exists(), "a refused call still created the run dir"


def test_a_missing_build_output_is_named():
    with tempfile.TemporaryDirectory() as tmp:
        base = Path(tmp)
        target = base / "empty-target"
        (target / "debug").mkdir(parents=True)
        run_dir = base / "run"
        done = run("-TargetDir", str(target), "-RunDir", str(run_dir), "-NoBuild", "-NoStart")
        assert done.returncode == 4, (done.returncode, done.stdout, done.stderr)
        assert "nucleos-core.exe" in done.stderr, done.stderr
        assert "empty-target" in done.stderr, done.stderr


def test_a_busy_port_is_refused_before_staging():
    with tempfile.TemporaryDirectory() as tmp:
        base = Path(tmp)
        target = fake_target(base)
        run_dir = base / "run"
        with socket.socket() as listener:
            listener.bind(("127.0.0.1", 0))
            listener.listen(1)
            port = listener.getsockname()[1]
            # No -NoStart: had the check regressed, the script would try to launch the run dir's
            # fake exe (garbage bytes, not a valid Win32 application) -- never a real binary.
            done = run("-TargetDir", str(target), "-RunDir", str(run_dir), "-NoBuild", "-Port", str(port))
        assert done.returncode == 3, (done.returncode, done.stdout, done.stderr)
        assert str(port) in done.stderr, done.stderr
        assert not run_dir.exists(), "a refused call still staged files"


test_staging_copies_the_daemon_and_every_sidecar_and_touches_no_source()
test_a_run_dir_exe_in_use_is_refused_and_left_alone()
test_a_relative_or_nested_run_dir_is_refused_before_anything_is_made()
test_a_missing_build_output_is_named()
test_a_busy_port_is_refused_before_staging()
print("run daemon from copy: ok")
