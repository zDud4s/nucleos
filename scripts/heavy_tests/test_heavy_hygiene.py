#!/usr/bin/env python3
"""RED tests for broker state hygiene (`scripts/heavy.py`): `status` marking a worktree
lock live or dead, the Mutex-guarded `sweep()` / `maybe_sweep()` / `gc` that collect dead
locks, waiter dirs and progress files, and progress files that carry the writer's `ctime`.

None of this exists yet, so every test here fails for that reason. State lives in a temp
NUCLEOS_HEAVY_DIR; children are `sys.executable -c` one-liners.
"""

from __future__ import annotations

import importlib.util
import json
import os
import shutil
import subprocess
import sys
import tempfile
import threading
import time
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
HEAVY = ROOT / "scripts" / "heavy.py"

HASH = "0123456789abcdef"


def load_heavy():
    spec = importlib.util.spec_from_file_location("heavy_hygiene_under_test", HEAVY)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


def clean_env(state_dir: str, **extra: str) -> dict:
    env = dict(os.environ)
    for k in ("NUCLEOS_HEAVY", "NUCLEOS_HEAVY_TOKEN", "NUCLEOS_HEAVY_HELD",
              "NUCLEOS_HEAVY_PRIO", "NUCLEOS_HEAVY_MAIN"):
        env.pop(k, None)
    env["NUCLEOS_HEAVY_DIR"] = state_dir
    env.update(extra)
    return env


def dead_pid() -> int:
    p = subprocess.Popen([sys.executable, "-c", "pass"])
    p.wait()
    return p.pid


def age_file(path: Path, seconds: float) -> None:
    t = time.time() - seconds
    os.utime(path, (t, t))


class HygieneTests(unittest.TestCase):
    def setUp(self) -> None:
        self.heavy = load_heavy()
        self.tmp = tempfile.mkdtemp(prefix="heavy-hygiene-")
        self.state = Path(self.tmp) / "state"
        (self.state / "wt").mkdir(parents=True)
        (self.state / "progress").mkdir()
        self.cwd = Path(self.tmp) / "cwd"
        self.cwd.mkdir()
        self.addCleanup(shutil.rmtree, self.tmp, ignore_errors=True)

    # ------------------------------------------------------------------ helpers

    def plant_lock(self, name: str, pid: int, ctime) -> Path:
        p = self.state / "wt" / f"{name}.lock"
        p.write_text(json.dumps({"pid": pid, "ctime": ctime, "argv": ["x"],
                                 "worktree": "w", "acquired": time.time()}),
                     encoding="utf-8")
        return p

    def plant_waiter(self, wdir: Path, name: str, pid: int, ctime) -> Path:
        wdir.mkdir(parents=True, exist_ok=True)
        p = wdir / name
        p.write_text(json.dumps({"pid": pid, "ctime": ctime, "argv": ["x"]}),
                     encoding="utf-8")
        return p

    def plant_progress(self, name: str, rec) -> Path:
        p = self.state / "progress" / name
        p.write_text(rec if isinstance(rec, str) else json.dumps(rec), encoding="utf-8")
        return p

    def run_heavy(self, args: list[str], **env_extra: str) -> subprocess.CompletedProcess:
        return subprocess.run(
            [sys.executable, str(HEAVY), *args],
            env=clean_env(str(self.state), **env_extra),
            capture_output=True, text=True, timeout=60, cwd=str(self.cwd),
        )

    def status(self) -> str:
        cp = self.run_heavy(["status"])
        self.assertEqual(cp.returncode, 0, cp.stderr)
        return cp.stdout

    def my_ctime(self):
        return self.heavy.proc_identity(os.getpid())[1]

    # ------------------------------------------------------------------ B1: status

    # 1
    def test_status_marks_a_lock_whose_pid_is_gone_dead(self) -> None:
        lock = self.plant_lock(HASH, dead_pid(), None)
        out = self.status()
        self.assertIn("worktree locks: 1 (dead: 1)", out)
        self.assertIn(f"lock={HASH}", out)
        self.assertIn("state=dead", out)
        self.assertTrue(lock.exists(), "status must not delete anything")

    # 2
    def test_status_marks_a_reused_pid_dead(self) -> None:
        real = self.my_ctime()
        if real is None:
            self.skipTest("creation time unreadable here")
        lock = self.plant_lock(HASH, os.getpid(), real + 10_000_000)
        out = self.status()
        self.assertIn("worktree locks: 1 (dead: 1)", out)
        self.assertIn("state=dead", out)
        self.assertTrue(lock.exists(), "status must not delete anything")

    # 3
    def test_status_marks_a_live_holder_live(self) -> None:
        self.plant_lock(HASH, os.getpid(), self.my_ctime())
        out = self.status()
        self.assertIn("worktree locks: 1 (dead: 0)", out)
        self.assertIn("state=live", out)
        self.assertNotIn("state=dead", out)

    # ------------------------------------------------------------------ B2: sweep

    # 4
    def test_sweep_removes_a_dead_lock_and_keeps_a_live_one(self) -> None:
        dead = self.plant_lock(HASH, dead_pid(), None)
        live = self.plant_lock("fedcba9876543210", os.getpid(), self.my_ctime())
        counts = self.heavy.sweep(self.state)
        self.assertFalse(dead.exists())
        self.assertTrue(live.exists())
        self.assertEqual(counts["locks"], 1)

    # 5
    def test_sweep_collects_empty_and_dead_waiters_dirs_only(self) -> None:
        wt = self.state / "wt"
        empty = wt / "aaaaaaaaaaaaaaaa.waiters"
        empty.mkdir()
        dead_dir = wt / "bbbbbbbbbbbbbbbb.waiters"
        self.plant_waiter(dead_dir, "w-1", dead_pid(), None)
        live_dir = wt / "cccccccccccccccc.waiters"
        live_file = self.plant_waiter(live_dir, "w-2", os.getpid(), self.my_ctime())
        counts = self.heavy.sweep(self.state)
        self.assertFalse(empty.exists())
        self.assertFalse(dead_dir.exists())
        self.assertTrue(live_dir.is_dir())
        self.assertTrue(live_file.exists())
        self.assertEqual(counts["waiters"], 2)

    # 6
    def test_sweep_removes_dead_or_reused_progress_and_keeps_live(self) -> None:
        real = self.my_ctime()
        dead = self.plant_progress("111", {"pid": dead_pid(), "ctime": None, "step": "x"})
        live = self.plant_progress(str(os.getpid()),
                                   {"pid": os.getpid(), "ctime": real, "step": "x"})
        removed = [dead]
        reused = None
        if real is not None:
            reused = self.plant_progress("222", {"pid": os.getpid(),
                                                 "ctime": real + 10_000_000, "step": "x"})
            removed.append(reused)
        counts = self.heavy.sweep(self.state)
        self.assertFalse(dead.exists())
        self.assertTrue(live.exists())
        if reused is not None:
            self.assertFalse(reused.exists())
        self.assertEqual(counts["progress"], len(removed))

    # 7
    def test_sweep_ages_out_legacy_progress_and_tmp_files(self) -> None:
        h = self.heavy
        old_legacy = self.plant_progress("333", {"pid": os.getpid(), "step": "x"})
        age_file(old_legacy, h.PROGRESS_LEGACY_STALE_S + 60)
        new_legacy = self.plant_progress("444", {"pid": os.getpid(), "step": "x"})
        old_tmp = self.plant_progress("555.tmp", {"pid": os.getpid()})
        age_file(old_tmp, h.TMP_STALE_S + 60)
        new_tmp = self.plant_progress("666.tmp", {"pid": os.getpid()})
        old_junk = self.plant_progress("777", "not json")
        age_file(old_junk, h.TMP_STALE_S + 60)
        new_junk = self.plant_progress("888", "not json")
        h.sweep(self.state)
        self.assertFalse(old_legacy.exists())
        self.assertTrue(new_legacy.exists())
        self.assertFalse(old_tmp.exists())
        self.assertTrue(new_tmp.exists())
        self.assertFalse(old_junk.exists())
        self.assertTrue(new_junk.exists())

    # 8
    def test_progress_file_carries_the_writer_ctime(self) -> None:
        heavy = self.heavy
        path = self.state / "progress" / "w8"
        stats = heavy._new_stats()
        stats["progress"]["units_total"] = 3
        stop = threading.Event()
        th = threading.Thread(target=heavy._write_progress, args=(path, stats, stop, None))
        th.start()
        try:
            deadline = time.time() + 10
            while not path.exists() and time.time() < deadline:
                time.sleep(0.05)
            self.assertTrue(path.exists(), "no progress file was written")
            rec = json.loads(path.read_text(encoding="utf-8"))
        finally:
            stop.set()
            th.join(10)
        self.assertIn("ctime", rec)
        self.assertEqual(rec["ctime"], heavy.proc_identity(os.getpid())[1])

    # 9
    def test_gc_subcommand_sweeps_and_reports(self) -> None:
        dead = self.plant_lock(HASH, dead_pid(), None)
        empty = self.state / "wt" / "aaaaaaaaaaaaaaaa.waiters"
        empty.mkdir()
        prog = self.plant_progress("999", {"pid": dead_pid(), "ctime": None, "step": "x"})
        cp = self.run_heavy(["gc"])
        self.assertEqual(cp.returncode, 0, cp.stderr)
        self.assertIn("removed: locks=1 waiters=1 progress=1", cp.stdout)
        self.assertFalse(dead.exists())
        self.assertFalse(empty.exists())
        self.assertFalse(prog.exists())
        self.assertTrue((self.state / "sweep.stamp").exists())

    # 9b
    def test_gc_reports_a_busy_mutex_without_a_traceback(self) -> None:
        import contextlib
        import io
        from unittest import mock

        holder = subprocess.Popen([sys.executable, "-c", "import time; time.sleep(60)"])
        self.addCleanup(holder.wait)
        self.addCleanup(holder.kill)
        mutex = self.state / ".mutex"
        mutex.mkdir()
        (mutex / "owner").write_text(json.dumps(
            {"pid": holder.pid, "ctime": self.heavy.proc_identity(holder.pid)[1]}))
        heavy = self.heavy
        orig = heavy.sweep
        err = io.StringIO()
        with mock.patch.dict(os.environ, {"NUCLEOS_HEAVY_DIR": str(self.state)}), \
                mock.patch.object(heavy, "sweep", lambda d: orig(d, 0.3)), \
                contextlib.redirect_stderr(err):
            code = heavy.cmd_gc()
        self.assertEqual(code, heavy.EXIT_QUEUE_TIMEOUT)
        self.assertNotIn("Traceback", err.getvalue())
        self.assertIn("busy", err.getvalue())
        self.assertIn(str(holder.pid), err.getvalue())
        self.assertTrue(mutex.exists(), "a live holder's mutex must be left alone")

    # 10
    def test_a_broker_run_sweeps_once_per_stamp_period(self) -> None:
        def plant():
            lock = self.plant_lock(HASH, dead_pid(), None)
            wdir = self.state / "wt" / "aaaaaaaaaaaaaaaa.waiters"
            wdir.mkdir(exist_ok=True)
            return lock, wdir

        argv = ["--", sys.executable, "-c", "pass"]
        lock, wdir = plant()
        cp = self.run_heavy(argv, NUCLEOS_HEAVY_CAPACITY="8")
        self.assertEqual(cp.returncode, 0, cp.stderr)
        self.assertFalse(lock.exists(), "the first run should have swept the dead lock")
        self.assertFalse(wdir.exists(), "the first run should have swept the empty dir")
        lock, wdir = plant()
        cp = self.run_heavy(argv, NUCLEOS_HEAVY_CAPACITY="8")
        self.assertEqual(cp.returncode, 0, cp.stderr)
        self.assertTrue(lock.exists(), "a fresh stamp means no second sweep")
        self.assertTrue(wdir.exists(), "a fresh stamp means no second sweep")


if __name__ == "__main__":
    unittest.main()
