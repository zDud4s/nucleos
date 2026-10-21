"""status --json: the machine-readable broker state read by tree_resources.py (plan
2026-10-21-arvore-fase1 §3.4). Hermetic: state lives in a temp NUCLEOS_HEAVY_DIR."""

from __future__ import annotations

import contextlib
import importlib.util
import io
import json
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location("heavy", ROOT / "scripts" / "heavy.py")
heavy = importlib.util.module_from_spec(spec)
spec.loader.exec_module(heavy)


def dead_pid() -> int:
    """A pid whose process has exited and been reaped. On Windows the handle must be released
    before the pid can be judged dead, hence communicate/wait and `del`."""
    p = subprocess.Popen([sys.executable, "-c", "pass"])
    p.communicate()
    p.wait()
    pid = p.pid
    del p
    return pid


class StatusJsonTest(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.dir = Path(self.tmp.name)
        env = {"NUCLEOS_HEAVY_DIR": str(self.dir), "NUCLEOS_HEAVY_CAPACITY": "4",
               "NUCLEOS_HEAVY_TARGET_SLOTS": "3"}
        patcher = mock.patch.dict(os.environ, env)
        patcher.start()
        self.addCleanup(patcher.stop)
        _, self.ctime = heavy.proc_identity(os.getpid())

    def run_json(self) -> dict:
        buf = io.StringIO()
        with contextlib.redirect_stdout(buf):
            rc = heavy.main(["status", "--json"])
        self.assertEqual(rc, 0)
        lines = buf.getvalue().strip().splitlines()
        self.assertEqual(len(lines), 1, "exactly one JSON line")
        return json.loads(lines[0])

    def write(self, rel: str, rec: dict) -> Path:
        path = self.dir / rel
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(json.dumps(rec), encoding="utf-8")
        return path

    def test_empty_state(self):
        out = self.run_json()
        self.assertEqual(out, {"capacity": 4, "held_weight": 0, "queue": 0,
                               "slots": {"total": 3, "free": 3}, "locks": []})

    def test_held_weight_slots_and_locks(self):
        me = os.getpid()
        dead = dead_pid()
        if heavy.is_alive(dead, 1.0):
            self.skipTest(f"pid {dead} was reused by a live process; cannot build a dead lock")
        self.write("held/a.json", {"pid": me, "ctime": self.ctime, "weight": 2})
        self.write("held/b.json", {"pid": me, "ctime": self.ctime})  # weight defaults to 1
        self.write("target-slots/1.json", {"pid": me, "ctime": self.ctime})
        self.write("wt/1a2b3c.lock", {
            "pid": me, "ctime": self.ctime, "acquired": 1760000000.0,
            "worktree": "C:\\Projects\\nucleos\\.claude\\worktrees\\x"})
        self.write("wt/dead01.lock", {
            "pid": dead, "ctime": 1.0, "acquired": 1760000001.5,
            "worktree": "C:/Projects/nucleos"})
        (self.dir / "wt" / "1a2b3c.waiters").mkdir()  # not a lock file: ignored
        (self.dir / "wt" / "broken.lock").write_text("{not json", encoding="utf-8")

        out = self.run_json()

        self.assertEqual(out["capacity"], 4)
        self.assertEqual(out["held_weight"], 3)
        self.assertEqual(out["queue"], 0)
        self.assertEqual(out["slots"], {"total": 3, "free": 2})
        by_hash = {lk["hash"]: lk for lk in out["locks"]}
        self.assertEqual(sorted(by_hash), ["1a2b3c", "dead01"])
        live = by_hash["1a2b3c"]
        self.assertEqual(live["worktree"], "C:/Projects/nucleos/.claude/worktrees/x")
        self.assertEqual(live["pid"], me)
        self.assertIs(live["alive"], True)
        self.assertEqual(live["acquired"], 1760000000.0)
        self.assertIs(by_hash["dead01"]["alive"], False)
        self.assertEqual(set(live), {"hash", "worktree", "pid", "alive", "acquired"})

    def test_text_status_unchanged(self):
        buf = io.StringIO()
        with contextlib.redirect_stdout(buf):
            self.assertEqual(heavy.main(["status"]), 0)
        text = buf.getvalue()
        self.assertTrue(text.startswith("state: "), text)
        self.assertIn("worktree locks: 0", text)


if __name__ == "__main__":
    unittest.main()
