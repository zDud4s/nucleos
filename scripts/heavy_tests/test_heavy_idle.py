#!/usr/bin/env python3
"""Tests for `heavy.py idle` (2026-10-08): the answer select_tests' `auto` mode reads before it
runs a gate's groups in parallel. Idle means nothing but the caller's own session and
idle-priority warms uses the broker.

Black-box, on a throwaway state dir (NUCLEOS_HEAVY_DIR); a live `sleep` child stands in for
another session.

    python scripts/heavy_tests/test_heavy_idle.py -v
"""

from __future__ import annotations

import json
import os
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
HEAVY = ROOT / "scripts" / "heavy.py"


class Idle(unittest.TestCase):
    def setUp(self) -> None:
        self.dir = Path(tempfile.mkdtemp(prefix="heavy-idle-"))
        self.addCleanup(shutil.rmtree, self.dir, ignore_errors=True)
        self.other = subprocess.Popen([sys.executable, "-c", "import time; time.sleep(120)"])
        self.addCleanup(self.other.wait)
        self.addCleanup(self.other.kill)

    def idle(self, **extra: str) -> subprocess.CompletedProcess:
        env = {k: v for k, v in os.environ.items()
               if k not in ("NUCLEOS_HEAVY_HELD", "NUCLEOS_HEAVY_TOKEN", "NUCLEOS_HEAVY_PRIO")}
        env.update(NUCLEOS_HEAVY_DIR=str(self.dir / "state"),
                   NUCLEOS_HEAVY_TD_ROOT=str(self.dir / "td"), **extra)
        return subprocess.run([sys.executable, str(HEAVY), "idle"], env=env,
                              capture_output=True, text=True, timeout=60)

    def put(self, rel: str, rec: dict) -> None:
        path = self.dir / "state" / rel
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(json.dumps(rec), encoding="utf-8")

    def test_an_empty_broker_is_idle(self) -> None:
        cp = self.idle()
        self.assertEqual(cp.returncode, 0, cp.stdout + cp.stderr)

    def test_another_sessions_lock_is_busy(self) -> None:
        self.put("wt/abc.lock", {"pid": self.other.pid, "worktree": "C:/elsewhere"})
        cp = self.idle()
        self.assertEqual(cp.returncode, 1, cp.stdout + cp.stderr)
        self.assertIn("C:/elsewhere", cp.stdout)

    def test_the_callers_own_lock_is_idle(self) -> None:
        self.put("wt/abc.lock", {"pid": self.other.pid, "worktree": "C:/here"})
        cp = self.idle(NUCLEOS_HEAVY_HELD=str(self.other.pid))
        self.assertEqual(cp.returncode, 0, cp.stdout + cp.stderr)

    def test_a_held_run_is_busy_and_a_warm_is_not(self) -> None:
        self.put("held/w.json", {"pid": self.other.pid, "prio": 3})
        self.assertEqual(self.idle().returncode, 0)
        self.put("held/r.json", {"pid": self.other.pid, "prio": 1})
        self.assertEqual(self.idle().returncode, 1)


if __name__ == "__main__":
    unittest.main()
