#!/usr/bin/env python3
"""Tests for the heavy broker's weight-drop internals (plan 2026-10-21-broker-followups2).

In-process: `drop_token_weight` keeps the record's mtime, retries its replace once and fails open;
`acquire_token` sweeps the stale `.held-<pid>.tmp` of a dead pid but keeps a live one's.
Broker-level: a `cargo nextest run` whose output shows `Starting N tests across M binaries`
drops from weight 2 to 1 like a `cargo test` does at its `Running` line.

    python scripts/heavy_tests/test_heavy_drop_weight.py -v
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
from unittest import mock

HERE = Path(__file__).resolve().parent
SCRIPTS = HERE.parent
for _p in (str(SCRIPTS), str(HERE)):
    if _p not in sys.path:
        sys.path.insert(0, _p)

from test_heavy_phase_drop import Base  # noqa: E402  (frozen module, reused as a fixture)

_spec = importlib.util.spec_from_file_location("heavy_under_test", SCRIPTS / "heavy.py")
heavy = importlib.util.module_from_spec(_spec)
sys.modules["heavy_under_test"] = heavy
_spec.loader.exec_module(heavy)

REAL_REPLACE = os.replace


class _StateDir(unittest.TestCase):
    def setUp(self) -> None:
        self.tmp = tempfile.mkdtemp(prefix="heavy-dw-")
        self.addCleanup(shutil.rmtree, self.tmp, True)
        self.state = Path(self.tmp)
        (self.state / "held").mkdir()


class DropWeight(_StateDir):
    def setUp(self) -> None:
        super().setUp()
        self.ctime = heavy.proc_identity(os.getpid())[1]
        self.path = self.state / "held" / str(os.getpid())
        self.path.write_text(json.dumps(
            {"pid": os.getpid(), "ctime": self.ctime, "weight": 2, "prio": 1}), encoding="utf-8")
        t = time.time_ns() - 3_600_000_000_000
        os.utime(self.path, ns=(t, t))
        self.mtime_ns = self.path.stat().st_mtime_ns
        self.tmpfile = self.state / f".held-{os.getpid()}.tmp"

    def weight(self) -> int:
        return int(json.loads(self.path.read_text(encoding="utf-8"))["weight"])

    def test_keeps_original_mtime(self) -> None:
        self.assertTrue(heavy.drop_token_weight(self.state, self.ctime))
        self.assertEqual(self.weight(), 1)
        self.assertEqual(self.path.stat().st_mtime_ns, self.mtime_ns)

    def test_retries_replace_once(self) -> None:
        calls = []

        def flaky(src, dst):
            calls.append(1)
            if len(calls) == 1:
                raise OSError("simulated sharing violation")
            return REAL_REPLACE(src, dst)

        with mock.patch.object(heavy.os, "replace", flaky), \
                mock.patch.object(heavy.time, "sleep", lambda *_: None):
            self.assertTrue(heavy.drop_token_weight(self.state, self.ctime))
        self.assertEqual(len(calls), 2)
        self.assertEqual(self.weight(), 1)
        self.assertFalse(self.tmpfile.exists())

    def test_fails_open_when_replace_always_fails(self) -> None:
        def broken(src, dst):
            raise OSError("simulated sharing violation")

        with mock.patch.object(heavy.os, "replace", broken), \
                mock.patch.object(heavy.time, "sleep", lambda *_: None):
            self.assertFalse(heavy.drop_token_weight(self.state, self.ctime))
        self.assertEqual(self.weight(), 2)
        self.assertFalse(self.tmpfile.exists())


class SweepTmp(_StateDir):
    def test_dead_pid_tmp_swept_live_pid_tmp_kept(self) -> None:
        p = subprocess.Popen([sys.executable, "-c", "pass"])
        p.wait()
        dead = self.state / f".held-{p.pid}.tmp"
        live = self.state / f".held-{os.getpid()}.tmp"
        dead.write_text("{}", encoding="utf-8")
        live.write_text("{}", encoding="utf-8")
        rec = {"pid": os.getpid(), "ctime": heavy.proc_identity(os.getpid())[1],
               "weight": 1, "prio": 1}
        env = {"NUCLEOS_HEAVY_CAPACITY": "4", "NUCLEOS_HEAVY_POLL_S": "0.1"}
        with mock.patch.dict(os.environ, env):
            admitted, _ = heavy.acquire_token(self.state, rec, 30.0)
            try:
                self.assertTrue(admitted)
            finally:
                heavy.release_token(self.state)
        self.assertFalse(dead.exists(), "the dead pid's .held tmp was not swept")
        self.assertTrue(live.exists(), "a live pid's .held tmp must be kept")


class NextestDrop(Base):
    def test_drop_on_nextest_starting_line(self) -> None:
        self.argv = [str(self.cargo), "nextest", "run"]
        seen, row = self.run_observed("Starting 1 test across 1 binary", pre=2.0, post=4.0)
        self.assertEqual(seen[0], 2, seen)
        self.assertIn(1, seen, f"held weight never dropped after the Starting line: {seen}")
        self.assertGreater(seen.index(1), seen.index(2), seen)
        self.assertIn("weight_drop_s", row, row)


if __name__ == "__main__":
    unittest.main()
