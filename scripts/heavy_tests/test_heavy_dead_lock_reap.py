#!/usr/bin/env python3
"""Dead worktree locks are reclaimed promptly, not on the hourly sweep (`scripts/heavy.py`).

A verify unit that hits its timeout has its whole process tree hard-killed, so the
`hold-worktree` holder never runs its release path and its `wt/<hash>.lock` stays behind.
`status` and any lock acquire must remove locks whose holder is dead, whatever worktree
they belong to, and `status` must say what it removed.
"""

from __future__ import annotations

import importlib.util
import json
import os
import shutil
import subprocess
import sys
import tempfile
import time
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
HEAVY = ROOT / "scripts" / "heavy.py"

DEAD_HASH = "0123456789abcdef"
OTHER_HASH = "fedcba9876543210"


def load_heavy():
    spec = importlib.util.spec_from_file_location("heavy_dead_lock_under_test", HEAVY)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


def dead_pid() -> int:
    p = subprocess.Popen([sys.executable, "-c", "pass"])
    p.wait()
    return p.pid


class DeadLockReapTests(unittest.TestCase):
    def setUp(self) -> None:
        self.heavy = load_heavy()
        self.tmp = tempfile.mkdtemp(prefix="heavy-deadlock-")
        self.state = Path(self.tmp) / "state"
        (self.state / "wt").mkdir(parents=True)
        self.cwd = Path(self.tmp) / "cwd"
        self.cwd.mkdir()
        self.addCleanup(shutil.rmtree, self.tmp, ignore_errors=True)

    def plant_lock(self, name: str, pid: int, ctime) -> Path:
        p = self.state / "wt" / f"{name}.lock"
        p.write_text(json.dumps({"pid": pid, "ctime": ctime, "argv": ["x"],
                                 "worktree": "w", "acquired": time.time()}),
                     encoding="utf-8")
        return p

    def status(self) -> str:
        env = dict(os.environ)
        env.pop("NUCLEOS_HEAVY", None)
        env["NUCLEOS_HEAVY_DIR"] = str(self.state)
        cp = subprocess.run([sys.executable, str(HEAVY), "status"], env=env,
                            capture_output=True, text=True, timeout=60, cwd=str(self.cwd))
        self.assertEqual(cp.returncode, 0, cp.stderr)
        return cp.stdout

    def test_status_removes_a_dead_lock_and_reports_it(self) -> None:
        lock = self.plant_lock(DEAD_HASH, dead_pid(), None)
        out = self.status()
        self.assertFalse(lock.exists(), "status must reclaim a dead holder's lock")
        self.assertIn("worktree locks: 0 (dead: 0)", out)
        self.assertIn("reaped dead locks: 1", out)
        self.assertIn(DEAD_HASH, out)

    def test_status_keeps_a_live_lock_and_reports_no_reap(self) -> None:
        ctime = self.heavy.proc_identity(os.getpid())[1]
        lock = self.plant_lock(DEAD_HASH, os.getpid(), ctime)
        out = self.status()
        self.assertTrue(lock.exists())
        self.assertIn("worktree locks: 1 (dead: 0)", out)
        self.assertNotIn("reaped dead locks", out)

    def test_status_reaps_a_reused_pid(self) -> None:
        real = self.heavy.proc_identity(os.getpid())[1]
        if real is None:
            self.skipTest("creation time unreadable here")
        lock = self.plant_lock(DEAD_HASH, os.getpid(), real + 10_000_000)
        out = self.status()
        self.assertFalse(lock.exists())
        self.assertIn("reaped dead locks: 1", out)

    def test_acquiring_one_worktree_reaps_dead_locks_of_others(self) -> None:
        stale = self.plant_lock(DEAD_HASH, dead_pid(), None)
        rec = {"pid": os.getpid(), "ctime": None, "argv": ["x"], "worktree": "w2",
               "prio": 0}
        ok, _, _ = self.heavy.acquire_lock(self.state, OTHER_HASH, rec, 5.0)
        self.assertTrue(ok)
        self.addCleanup(self.heavy.release_lock, self.state, OTHER_HASH)
        self.assertFalse(stale.exists(),
                         "a dead lock of another worktree must go on any acquire")

    def test_reap_dead_locks_returns_the_removed_hashes(self) -> None:
        self.plant_lock(DEAD_HASH, dead_pid(), None)
        ctime = self.heavy.proc_identity(os.getpid())[1]
        live = self.plant_lock(OTHER_HASH, os.getpid(), ctime)
        removed = self.heavy.reap_dead_locks(self.state)
        self.assertEqual(removed, [DEAD_HASH])
        self.assertTrue(live.exists())


if __name__ == "__main__":
    unittest.main()
