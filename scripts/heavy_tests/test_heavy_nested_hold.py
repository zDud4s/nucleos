#!/usr/bin/env python3
"""A hold-worktree session started under a token holder must not make its steps queue for a
token the holder already owns (`heavy.py -- bash gates.sh core`: the outer wrapper holds a
token, gates.sh opens hold-worktree, and its clippy step used to wait for capacity the outer
call occupied -- a self-deadlock, 2026-10-22).

    python scripts/heavy_tests/test_heavy_nested_hold.py -v
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
PROBE = {"logical": 12, "total_mb": 16000, "avail_mb": 8000, "busy_cores": 2}


class NestedHold(unittest.TestCase):
    def setUp(self) -> None:
        self.tmp = tempfile.mkdtemp(prefix="heavy-nested-hold-")
        self.state = Path(self.tmp) / "state"
        (self.state / "held").mkdir(parents=True)
        self.addCleanup(shutil.rmtree, self.tmp, True)

    def env(self, token_pid: int | None) -> dict:
        env = {k: v for k, v in os.environ.items() if not k.startswith("NUCLEOS_HEAVY")}
        env.update(NUCLEOS_HEAVY_DIR=str(self.state), NUCLEOS_HEAVY_CAPACITY="4",
                   NUCLEOS_HEAVY_POLL_S="0.1", NUCLEOS_HEAVY_FAKE_PROBE=json.dumps(PROBE))
        if token_pid is not None:
            env["NUCLEOS_HEAVY_TOKEN"] = str(token_pid)
        return env

    def run_inner(self, token_pid: int | None) -> subprocess.CompletedProcess:
        # The whole capacity is held (by the "outer" wrapper). hold-worktree runs a child that
        # makes one broker call of its own.
        inner = [sys.executable, str(HEAVY), "--wait-max", "3", "--",
                 sys.executable, "-c", "pass"]
        cmd = [sys.executable, str(HEAVY), "hold-worktree", "--",
               sys.executable, "-c",
               "import subprocess, sys; sys.exit(subprocess.run(%r).returncode)" % (inner,)]
        sleeper = subprocess.Popen([sys.executable, "-c", "import time; time.sleep(120)"])
        self.addCleanup(sleeper.wait)
        self.addCleanup(sleeper.kill)
        (self.state / "held" / str(sleeper.pid)).write_text(
            json.dumps({"pid": sleeper.pid, "weight": 4, "cls": "other|-|-|a|c"}),
            encoding="utf-8")
        return subprocess.run(cmd, env=self.env(sleeper.pid if token_pid else None),
                              capture_output=True, text=True, timeout=90, cwd=str(ROOT))

    def test_step_under_a_token_holders_session_does_not_queue(self) -> None:
        cp = self.run_inner(token_pid=1)
        self.assertEqual(cp.returncode, 0, cp.stdout + cp.stderr)

    def test_without_a_token_holder_the_step_still_queues(self) -> None:
        cp = self.run_inner(token_pid=None)
        self.assertNotEqual(cp.returncode, 0, "control: a full machine must still queue")


if __name__ == "__main__":
    unittest.main()
