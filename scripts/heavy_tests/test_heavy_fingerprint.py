#!/usr/bin/env python3
"""RED tests for the heavy broker's per-crate fingerprint, weight-0 hits and `report`
(`scripts/heavy.py`): what the fingerprint covers, how the crate dir is resolved, the
target-dir registry's last-writer rule, the weight-0 / recording / `fp_miss` semantics and
`heavy.py report`.

The fingerprint registry and `report` do not exist yet, so every test here fails for that
reason. Hermetic: state lives in a temp NUCLEOS_HEAVY_DIR, each "worktree" is a throwaway git
repo shaped like this project (`core/`, `shell/src-tauri/`, `scripts/`, `Cargo.toml`), and
`cargo` is a fake launcher around a python script that optionally prints `Compiling x` and
exits with a chosen code. Nothing real is compiled.

Observable contract used here (the registry's file layout beyond `td/*.json` holding
{fingerprint, worktree} is deliberately not asserted):
- a run is a "hit" when the broker's log row carries `fp_hit: true` and `weight: 0`;
- `compiled` is true when a `^\\s*Compiling ` line was seen;
- `fp_miss` is true only when a hit was granted AND a `Compiling` line was seen anyway.
"""

from __future__ import annotations

import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import time
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
HEAVY = ROOT / "scripts" / "heavy.py"

# Fake cargo body. Controlled by env so one launcher serves every scenario.
FAKE = r'''
import os, pathlib, sys, time

start = os.environ.get("FAKE_START_FILE")
if start:
    pathlib.Path(start).write_text(str(os.getpid()))
out = os.environ.get("FAKE_STDOUT", "")
err = os.environ.get("FAKE_STDERR", "")
if os.environ.get("FAKE_COMPILE"):
    err = "   Compiling nucleos-core v0.1.0\n" + err
if out:
    sys.stdout.write(out + "\n")
if err:
    sys.stderr.write(err + "\n")
sys.stdout.flush()
sys.stderr.flush()
stop = os.environ.get("FAKE_STOP_FILE")
if stop:
    while not pathlib.Path(stop).exists():
        time.sleep(0.05)
sys.exit(int(os.environ.get("FAKE_EXIT", "0")))
'''

FILES = {
    "Cargo.toml": '[workspace]\nmembers = ["core"]\n',
    "Cargo.lock": "# lock v1\n",
    "rust-toolchain.toml": '[toolchain]\nchannel = "stable"\n',
    ".gitignore": "*.log\nignored/\n",
    "core/Cargo.toml": '[package]\nname = "nucleos-core"\n',
    "core/src/main.rs": "fn main() {}\n",
    "shell/src-tauri/Cargo.toml": '[package]\nname = "shell"\n',
    "shell/src-tauri/src/main.rs": "fn main() {}\n",
    "shell/src/app.ts": "export {};\n",
    "scripts/gates.sh": "#!/bin/sh\necho gates\n",
}

TEST_ONLY_FAILURE = "test result: FAILED. 3 passed; 1 failed; 0 ignored"


def git(path: Path, *args: str) -> None:
    subprocess.run(["git", "-C", str(path), *args], check=True, capture_output=True)


def make_repo(path: Path) -> Path:
    path.mkdir(parents=True, exist_ok=True)
    subprocess.run(["git", "init", "-q", str(path)], check=True, capture_output=True)
    git(path, "config", "user.email", "t@example.com")
    git(path, "config", "user.name", "t")
    git(path, "config", "commit.gpgsign", "false")
    for rel, text in FILES.items():
        f = path / rel
        f.parent.mkdir(parents=True, exist_ok=True)
        f.write_text(text, encoding="utf-8")
    git(path, "add", "-A")
    git(path, "commit", "-q", "-m", "init")
    return path


def wait_for(pred, timeout: float = 30.0, step: float = 0.1) -> bool:
    deadline = time.time() + timeout
    while time.time() < deadline:
        if pred():
            return True
        time.sleep(step)
    return pred()


def same_path(a: str, b: Path) -> bool:
    return os.path.normcase(os.path.realpath(a)) == os.path.normcase(os.path.realpath(str(b)))


class FingerprintTests(unittest.TestCase):
    def setUp(self) -> None:
        self.tmp = Path(tempfile.mkdtemp(prefix="heavy-f-"))
        self.state = self.tmp / "state"
        self.marks = self.tmp / "marks"
        self.marks.mkdir()
        self.cargo_home = self.tmp / "cargo-home"
        self.cargo_home.mkdir()
        self.repo = make_repo(self.tmp / "wt-a")
        self.td = self.tmp / "target-1"
        self.procs: list[subprocess.Popen] = []
        (self.tmp / "fake.py").write_text(FAKE, encoding="utf-8")
        if os.name == "nt":
            self.cargo = self.tmp / "cargo.cmd"
            self.cargo.write_text(
                f'@echo off\r\n"{sys.executable}" "{self.tmp / "fake.py"}" %*\r\n'
                "exit /b %ERRORLEVEL%\r\n",
                encoding="utf-8",
            )
        else:
            self.cargo = self.tmp / "cargo"
            self.cargo.write_text(
                f'#!/bin/sh\nexec "{sys.executable}" "{self.tmp / "fake.py"}" "$@"\n',
                encoding="utf-8",
            )
            self.cargo.chmod(0o755)
        self.addCleanup(self._teardown)

    def _teardown(self) -> None:
        (self.marks / "all.stop").write_text("x")
        for p in self.procs:
            try:
                p.wait(timeout=15)
            except Exception:
                p.kill()
        shutil.rmtree(self.tmp, True)

    # ---- helpers -------------------------------------------------------------------
    def env(self, capacity: int = 4, target_dir: Path | None = None, **extra: str) -> dict:
        env = dict(os.environ)
        for k in list(env):
            if (k in ("NUCLEOS_HEAVY", "NUCLEOS_HEAVY_TOKEN", "NUCLEOS_HEAVY_HELD",
                      "NUCLEOS_HEAVY_PRIO", "NUCLEOS_HEAVY_WAIT_MAX", "NUCLEOS_HEAVY_MAIN",
                      "RUSTFLAGS", "CARGO_TARGET_DIR", "CARGO_BUILD_TARGET_DIR")
                    or k.startswith(("CARGO_PROFILE_", "CARGO_FEATURES", "FAKE_"))):
                env.pop(k)
        env.update(
            NUCLEOS_HEAVY_DIR=str(self.state),
            NUCLEOS_HEAVY_CAPACITY=str(capacity),
            NUCLEOS_HEAVY_POLL_S="0.1",
            CARGO_HOME=str(self.cargo_home),
            CARGO_TARGET_DIR=str(target_dir or self.td),
        )
        env.update(extra)
        return env

    def log_rows(self) -> list[dict]:
        p = self.state / "log.jsonl"
        if not p.exists():
            return []
        return [json.loads(x) for x in p.read_text(encoding="utf-8").splitlines() if x.strip()]

    def cmd(self, cargo_args: list[str], wait_max: str | None = None) -> list[str]:
        args = [sys.executable, str(HEAVY)]
        if wait_max is not None:
            args += ["--wait-max", wait_max]
        return args + ["--", str(self.cargo), *cargo_args]

    def run_broker(self, cargo_args: list[str] | None = None, cwd: Path | None = None,
                   wait_max: str | None = "60", **env_kw) -> tuple[int, dict]:
        """Run the broker to completion; return (exit code, the log row it appended)."""
        before = len(self.log_rows())
        cp = subprocess.run(
            self.cmd(cargo_args or ["test", "-p", "nucleos-core"], wait_max),
            env=self.env(**env_kw), cwd=str(cwd or self.repo),
            capture_output=True, text=True, timeout=120,
        )
        rows = self.log_rows()
        self.assertEqual(len(rows), before + 1, f"expected one new log row; stderr={cp.stderr!r}")
        return cp.returncode, rows[-1]

    def popen(self, cargo_args: list[str], cwd: Path, wait_max: str | None = "60",
              **env_kw) -> subprocess.Popen:
        p = subprocess.Popen(
            self.cmd(cargo_args, wait_max), env=self.env(**env_kw), cwd=str(cwd),
            stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
        )
        self.procs.append(p)
        return p

    def is_hit(self, row: dict) -> bool:
        return bool(row.get("fp_hit")) and row.get("weight") == 0

    def td_files(self) -> list[Path]:
        d = self.state / "td"
        return sorted(d.glob("*.json")) if d.is_dir() else []

    def assert_mutation_misses(self, mutate, label: str, cargo_args=None, cwd=None,
                               env_before=None, env_after=None) -> None:
        """Register a baseline, prove it hits, mutate, prove the next run is a miss."""
        env_before = env_before or {}
        env_after = env_before if env_after is None else env_after
        self.run_broker(cargo_args, cwd, **env_before)
        rc, row = self.run_broker(cargo_args, cwd, **env_before)
        self.assertEqual(rc, 0)
        self.assertTrue(self.is_hit(row), f"{label}: unchanged tree did not hit: {row}")
        mutate()
        rc, row = self.run_broker(cargo_args, cwd, **env_after)
        self.assertEqual(rc, 0)
        self.assertFalse(self.is_hit(row), f"{label}: change was not noticed: {row}")
        self.assertEqual(row.get("weight"), 2, f"{label}: a miss must keep weight 2: {row}")

    def assert_stays_hit(self, mutate, label: str, cargo_args=None, cwd=None,
                         env_before=None, env_after=None) -> None:
        env_before = env_before or {}
        env_after = env_before if env_after is None else env_after
        self.run_broker(cargo_args, cwd, **env_before)
        mutate()
        rc, row = self.run_broker(cargo_args, cwd, **env_after)
        self.assertEqual(rc, 0)
        self.assertTrue(self.is_hit(row), f"{label}: should not invalidate the fingerprint: {row}")

    def write(self, rel: str, text: str) -> None:
        f = self.repo / rel
        f.parent.mkdir(parents=True, exist_ok=True)
        f.write_text(text, encoding="utf-8")

    # 34
    def test_fingerprint_inputs(self) -> None:
        # Inputs that must invalidate it.
        self.assert_mutation_misses(
            lambda: self.write("core/src/main.rs", "fn main() { /* edit */ }\n"), "tracked core file")
        self.assert_mutation_misses(
            lambda: self.write("core/src/extra.rs", "pub fn x() {}\n"), "untracked core file")
        self.assert_mutation_misses(
            lambda: self.write("Cargo.toml", '[workspace]\nmembers = ["core", "x"]\n'), "Cargo.toml")
        self.assert_mutation_misses(
            lambda: self.write("Cargo.lock", "# lock v2\n"), "Cargo.lock")
        self.assert_mutation_misses(
            lambda: self.write("rust-toolchain.toml", '[toolchain]\nchannel = "nightly"\n'),
            "rust-toolchain.toml")
        self.assert_mutation_misses(
            lambda: self.write(".cargo/config.toml", "[build]\njobs = 2\n"), ".cargo/")
        self.assert_mutation_misses(
            lambda: self.write("scripts/gates.sh", "#!/bin/sh\necho changed\n"),
            "scripts/ (compiled into the classifier test)")
        self.assert_mutation_misses(
            lambda: (git(self.repo, "commit", "-q", "--allow-empty", "-m", "next")), "HEAD")
        # Environment and argv inputs.
        self.assert_mutation_misses(
            lambda: None, "RUSTFLAGS", env_before={"RUSTFLAGS": "-Dwarnings"},
            env_after={"RUSTFLAGS": "-Cdebuginfo=0"})
        self.assert_mutation_misses(
            lambda: None, "CARGO_PROFILE_*", env_before={"CARGO_PROFILE_DEV_DEBUG": "0"},
            env_after={"CARGO_PROFILE_DEV_DEBUG": "2"})
        self.assert_mutation_misses(
            lambda: None, "CARGO_FEATURES*", env_before={"CARGO_FEATURES_X": "a"},
            env_after={"CARGO_FEATURES_X": "b"})
        # argv variants are different fingerprints: register one, then run another.
        for label, other in (
            ("profile", ["test", "-p", "nucleos-core", "--release"]),
            ("feature", ["test", "-p", "nucleos-core", "--features", "x"]),
            ("--test", ["test", "-p", "nucleos-core", "--test", "module_map"]),
            ("--lib", ["test", "-p", "nucleos-core", "--lib"]),
            ("subcommand", ["check", "-p", "nucleos-core"]),
        ):
            self.run_broker(["test", "-p", "nucleos-core"])
            rc, row = self.run_broker(other)
            self.assertFalse(self.is_hit(row), f"argv {label} must change the fingerprint: {row}")
        # The resolved target dir is an input: a fresh one has no registry entry.
        self.run_broker()
        _, row = self.run_broker(target_dir=self.tmp / "target-other")
        self.assertFalse(self.is_hit(row), "a different target dir must not hit")

        # Inputs that must NOT invalidate it.
        self.assert_stays_hit(lambda: self.write("shell/src/app.ts", "export const a = 1;\n"),
                              "file outside the crate paths")
        self.assert_stays_hit(lambda: self.write("shell/src-tauri/src/main.rs", "fn main() { 1; }\n"),
                              "another crate's source")
        self.assert_stays_hit(lambda: self.write("core/trace.log", "noise\n"),
                              "ignored file inside the crate")
        self.assert_stays_hit(lambda: self.write("notes.txt", "scratch\n"),
                              "untracked file outside the paths")
        self.assert_stays_hit(lambda: None, "unrelated env var", env_before={"FOO_UNRELATED": "1"},
                              env_after={"FOO_UNRELATED": "2"})
        # A positional filter and everything after `--` are dropped from the argv part.
        self.run_broker(["test", "-p", "nucleos-core"])
        for other in (["test", "-p", "nucleos-core", "some_filter"],
                      ["test", "-p", "nucleos-core", "other_filter"],
                      ["test", "-p", "nucleos-core", "--", "--nocapture"],
                      ["test", "-p", "nucleos-core", "f", "--", "--exact", "--test-threads=1"]):
            _, row = self.run_broker(other)
            self.assertTrue(self.is_hit(row), f"filter/after-`--` must not matter ({other}): {row}")

    # 35
    def test_crate_resolution(self) -> None:
        # root + -p nucleos-core, and root + no -p, both mean core/.
        for args in (["test", "-p", "nucleos-core"], ["test"]):
            self.assert_mutation_misses(
                lambda: self.write("core/src/main.rs", f"fn main() {{ /* {args} */ }}\n"),
                f"core via {args}", cargo_args=args)
            self.assert_stays_hit(
                lambda: self.write("shell/src-tauri/src/main.rs", f"fn main() {{ /* {args} */ }}\n"),
                f"src-tauri edit while building core via {args}", cargo_args=args)
        # cwd under shell/src-tauri means that crate.
        tauri = self.repo / "shell" / "src-tauri"
        self.assert_mutation_misses(
            lambda: self.write("shell/src-tauri/src/main.rs", "fn main() { /* t1 */ }\n"),
            "src-tauri via cwd", cargo_args=["test"], cwd=tauri)
        self.assert_stays_hit(
            lambda: self.write("core/src/main.rs", "fn main() { /* t2 */ }\n"),
            "core edit while building src-tauri via cwd", cargo_args=["test"], cwd=tauri)
        deep = tauri / "src"
        self.assert_mutation_misses(
            lambda: self.write("shell/src-tauri/src/main.rs", "fn main() { /* t3 */ }\n"),
            "src-tauri via a deeper cwd", cargo_args=["test"], cwd=deep)
        # --manifest-path wins: its directory is the crate.
        mp_tauri = ["test", "--manifest-path", str(tauri / "Cargo.toml")]
        self.assert_mutation_misses(
            lambda: self.write("shell/src-tauri/src/main.rs", "fn main() { /* m1 */ }\n"),
            "manifest-path to src-tauri", cargo_args=mp_tauri)
        self.assert_stays_hit(
            lambda: self.write("core/src/main.rs", "fn main() { /* m2 */ }\n"),
            "core edit under a src-tauri manifest-path", cargo_args=mp_tauri)
        mp_core = ["test", "-p", "whatever", "--manifest-path", str(self.repo / "core" / "Cargo.toml")]
        self.assert_mutation_misses(
            lambda: self.write("core/src/main.rs", "fn main() { /* m3 */ }\n"),
            "manifest-path to core", cargo_args=mp_core)
        # Anything else: no crate, no fingerprint, never a hit, weight 2, nothing registered.
        shutil.rmtree(self.state / "td", ignore_errors=True)
        for _ in range(2):
            rc, row = self.run_broker(["test", "-p", "some-other-crate"])
            self.assertEqual(rc, 0)
            self.assertFalse(self.is_hit(row), f"unknown crate must not hit: {row}")
            self.assertEqual(row.get("weight"), 2)
        self.assertEqual(self.td_files(), [], "an unresolved crate must not write a registry entry")

    # 36
    def test_hit_requires_last_writer(self) -> None:
        wt_b = self.tmp / "wt-b"
        subprocess.run(["git", "clone", "-q", str(self.repo), str(wt_b)], check=True,
                       capture_output=True)
        # Identical content, identical HEAD, one shared target dir.
        self.run_broker(cwd=self.repo)
        _, row = self.run_broker(cwd=self.repo)
        self.assertTrue(self.is_hit(row), "A's own repeat must hit")
        entries = self.td_files()
        self.assertEqual(len(entries), 1, "one target dir, one registry entry")
        rec = json.loads(entries[0].read_text(encoding="utf-8"))
        self.assertTrue(rec.get("fingerprint"))
        self.assertTrue(same_path(rec.get("worktree", ""), self.repo), rec)
        # B builds into the same target dir: not a hit for B, and it becomes the last writer.
        _, row = self.run_broker(cwd=wt_b)
        self.assertFalse(self.is_hit(row), f"B has not written this target dir yet: {row}")
        self.assertEqual(len(self.td_files()), 1)
        rec = json.loads(self.td_files()[0].read_text(encoding="utf-8"))
        self.assertTrue(same_path(rec.get("worktree", ""), wt_b), rec)
        # So A's artifacts are gone: A misses even though its tree never changed.
        _, row = self.run_broker(cwd=self.repo)
        self.assertFalse(self.is_hit(row), f"A must miss after B wrote the target dir: {row}")
        # ... and now A is the last writer again, so B misses.
        _, row = self.run_broker(cwd=wt_b)
        self.assertFalse(self.is_hit(row))
        _, row = self.run_broker(cwd=wt_b)
        self.assertTrue(self.is_hit(row), "B's repeat as last writer must hit")
        # A separate target dir per worktree keeps both warm.
        ta, tb = self.tmp / "ta", self.tmp / "tb"
        self.run_broker(cwd=self.repo, target_dir=ta)
        self.run_broker(cwd=wt_b, target_dir=tb)
        _, ra = self.run_broker(cwd=self.repo, target_dir=ta)
        _, rb = self.run_broker(cwd=wt_b, target_dir=tb)
        self.assertTrue(self.is_hit(ra) and self.is_hit(rb), f"{ra} / {rb}")

    # 37
    def test_hit_runs_weight0_and_recording_rules(self) -> None:
        # Register (compiling is fine: the first run is a miss by definition).
        rc, row = self.run_broker(FAKE_COMPILE="1")
        self.assertEqual(rc, 0)
        self.assertFalse(row.get("fp_hit"))
        self.assertTrue(row.get("compiled"), "a Compiling line sets compiled")
        self.assertFalse(row.get("fp_miss"), "fp_miss needs a granted hit first")
        self.assertEqual(row.get("weight"), 2)
        self.assertEqual(len(self.td_files()), 1, "exit 0 registers the fingerprint")

        # A hit takes no token even when the whole budget is held by someone else.
        blocker_repo = make_repo(self.tmp / "wt-block")
        started, stop = self.marks / "block.start", self.marks / "block.stop"
        blocker = self.popen(["test", "-p", "other"], blocker_repo, capacity=2,
                             target_dir=self.tmp / "target-block",
                             FAKE_START_FILE=str(started), FAKE_STOP_FILE=str(stop))
        self.assertTrue(wait_for(started.exists), "blocker never started")
        rc, row = self.run_broker(wait_max="5", capacity=2)
        self.assertEqual(rc, 0, "a weight-0 hit must run past a full budget")
        self.assertTrue(self.is_hit(row), row)
        self.assertEqual(row.get("weight"), 0)
        self.assertFalse(row.get("compiled"))
        self.assertFalse(row.get("fp_miss"))
        # A miss in the same situation does wait, and gives up with 75.
        self.write("core/src/main.rs", "fn main() { /* miss */ }\n")
        rc, row = self.run_broker(wait_max="2", capacity=2)
        self.assertEqual(rc, 75, f"a miss needs a token and the budget is spent: {row}")
        stop.write_text("x")
        self.assertEqual(blocker.wait(timeout=30), 0)

        # A hit still waits for the worktree lock held by another run in this worktree.
        self.run_broker()  # re-register after the edit
        wt_started, wt_stop = self.marks / "wt.start", self.marks / "wt.stop"
        holder = self.popen(["check", "-p", "nucleos-core"], self.repo,
                            target_dir=self.tmp / "target-holder",
                            FAKE_START_FILE=str(wt_started), FAKE_STOP_FILE=str(wt_stop))
        self.assertTrue(wait_for(wt_started.exists), "lock holder never started")
        waiter = self.popen(["test", "-p", "nucleos-core"], self.repo)
        time.sleep(1.5)
        self.assertIsNone(waiter.poll(), "a hit must still queue behind the worktree lock")
        wt_stop.write_text("x")
        self.assertEqual(holder.wait(timeout=30), 0)
        self.assertEqual(waiter.wait(timeout=30), 0)
        self.assertTrue(self.is_hit(self.log_rows()[-1]), self.log_rows()[-1])

        # hit + a Compiling line anyway = fp_miss (the registry lied), compiled too.
        self.run_broker(FAKE_COMPILE="1")  # the tree is registered, this is a hit
        rc, row = self.run_broker(FAKE_COMPILE="1")
        self.assertEqual(rc, 0)
        self.assertTrue(row.get("fp_hit"))
        self.assertTrue(row.get("compiled"))
        self.assertTrue(row.get("fp_miss"), f"hit + Compiling must be fp_miss: {row}")

        # `compiled` is only a line that STARTS with Compiling (leading blanks allowed).
        self.write("core/src/main.rs", "fn main() { /* c */ }\n")
        _, row = self.run_broker(FAKE_STDERR="not Compiling here\nFinished dev profile")
        self.assertFalse(row.get("compiled"), f"mid-line text is not a compile: {row}")
        self.write("core/src/main.rs", "fn main() { /* d */ }\n")
        _, row = self.run_broker(FAKE_STDERR="  Compiling serde v1.0\n")
        self.assertTrue(row.get("compiled"), row)
        self.assertFalse(row.get("fp_miss"), "a miss that compiles is expected, not an fp_miss")

        # Recording: a test-only failure registers, a compile failure does not.
        t_fail = self.tmp / "t-fail"
        rc, row = self.run_broker(target_dir=t_fail, FAKE_EXIT="101", FAKE_STDOUT=TEST_ONLY_FAILURE)
        self.assertEqual(rc, 101)
        _, row = self.run_broker(target_dir=t_fail)
        self.assertTrue(self.is_hit(row), f"test-only failure must register: {row}")

        t_cc = self.tmp / "t-compile-error"
        rc, _ = self.run_broker(
            target_dir=t_cc, FAKE_EXIT="101",
            FAKE_STDOUT=TEST_ONLY_FAILURE, FAKE_STDERR="error: could not compile `nucleos-core`")
        self.assertEqual(rc, 101)
        _, row = self.run_broker(target_dir=t_cc)
        self.assertFalse(self.is_hit(row), f"could not compile must not register: {row}")

        t_plain = self.tmp / "t-plain-fail"
        rc, _ = self.run_broker(target_dir=t_plain, FAKE_EXIT="1", FAKE_STDERR="error: boom")
        self.assertEqual(rc, 1)
        _, row = self.run_broker(target_dir=t_plain)
        self.assertFalse(self.is_hit(row), f"an unexplained failure must not register: {row}")

    # 38
    def test_report_aggregates(self) -> None:
        self.state.mkdir(parents=True, exist_ok=True)

        def row(ts: str, agent, kind: str, wt: str, **kw) -> dict:
            base = {"v": 1, "ts": ts, "worktree": wt, "agent": agent, "prio": 1, "eff_prio": 1,
                    "kind": kind, "weight": 2, "wait_lock_s": 1.0, "wait_token_s": 1.0,
                    "run_s": 5.0, "compiled": False, "fp_hit": False, "fp_miss": False,
                    "exit": 0, "argv0": "cargo"}
            base.update(kw)
            return base

        rows = [
            # Excluded by --since.
            row("2020-01-01T00:00:00+00:00", "main", "cargo", "C:/old", wait_lock_s=900.0,
                wait_token_s=900.0, run_s=900.0),
            row("2026-10-02T10:00:00+00:00", "main", "cargo", "C:/wtA", wait_lock_s=1.0,
                wait_token_s=21.0, run_s=31.0, compiled=True),
            row("2026-10-02T10:01:00+00:00", "main", "cargo", "C:/wtA", wait_lock_s=13.0,
                wait_token_s=99.0, run_s=37.0, compiled=True, fp_hit=True, fp_miss=True, weight=0),
            row("2026-10-02T10:02:00+00:00", "agent-7f3", "cargo", "C:/wtB", wait_lock_s=2.0,
                wait_token_s=23.0, run_s=41.0, compiled=True, fp_hit=True, weight=0),
            row("2026-10-02T10:03:00+00:00", "agent-9a1", "node", "C:/wtB", wait_lock_s=40.0,
                wait_token_s=25.0, run_s=500.0, exit=75, weight=1),
            row("2026-10-02T10:04:00+00:00", None, "go", "C:/wtC", wait_lock_s=17.0,
                wait_token_s=0.5, run_s=3.0, exit=75, broker_error="boom", weight=1),
        ]
        (self.state / "log.jsonl").write_text(
            "\n".join(json.dumps(r) for r in rows) + "\n", encoding="utf-8")

        cp = subprocess.run(
            [sys.executable, str(HEAVY), "report", "--since", "2026-10-01"],
            env=self.env(), cwd=str(self.repo), capture_output=True, text=True, timeout=60,
        )
        self.assertEqual(cp.returncode, 0, cp.stderr)
        out = cp.stdout

        def has(pattern: str) -> bool:
            return re.search(pattern, out, re.I) is not None

        # By agent class: main / subagent / none.
        self.assertTrue(has(r"main\D{0,15}\b2\b"), out)
        self.assertTrue(has(r"subagent\D{0,15}\b2\b"), out)
        self.assertTrue(has(r"none\D{0,15}\b1\b"), out)
        # By kind.
        self.assertTrue(has(r"cargo\D{0,15}\b3\b"), out)
        self.assertTrue(has(r"node\D{0,15}\b1\b"), out)
        self.assertTrue(has(r"go\D{0,15}\b1\b"), out)
        # Outcome counters.
        self.assertTrue(has(r"75\D{0,15}\b2\b"), out)
        self.assertTrue(has(r"broker_error\D{0,15}\b1\b"), out)
        self.assertTrue(has(r"fp_hit\D{0,15}\b2\b"), out)
        self.assertTrue(has(r"fp_miss\D{0,15}\b1\b"), out)
        # Compiled per worktree.
        self.assertTrue(has(r"wtA\D{0,25}\b2\b"), out)
        self.assertTrue(has(r"wtB\D{0,25}\b1\b"), out)
        # Medians of the five rows left after --since (the 2020 row is out).
        self.assertTrue(has(r"wait_lock_s\D{0,15}\b13(\.0+)?\b"), out)
        self.assertTrue(has(r"wait_token_s\D{0,15}\b23(\.0+)?\b"), out)
        self.assertTrue(has(r"run_s\D{0,15}\b37(\.0+)?\b"), out)
        self.assertNotIn("C:/old", out)

        # Without --since the old row counts: six runs, and its worktree shows up.
        cp = subprocess.run(
            [sys.executable, str(HEAVY), "report"],
            env=self.env(), cwd=str(self.repo), capture_output=True, text=True, timeout=60,
        )
        self.assertEqual(cp.returncode, 0, cp.stderr)
        self.assertTrue(re.search(r"main\D{0,15}\b3\b", cp.stdout, re.I), cp.stdout)


if __name__ == "__main__":
    unittest.main()
