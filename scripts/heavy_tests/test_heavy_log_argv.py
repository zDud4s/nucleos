#!/usr/bin/env python3
"""The broker's log line names the command it ran, not only the program (spec 2026-10-05, F0)."""

from __future__ import annotations

import importlib.util
import json
import shutil
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from test_heavy_runtime import HEAVY, clean_env, read_log, run_broker  # noqa: E402


def load_heavy():
    spec = importlib.util.spec_from_file_location("heavy_under_test", HEAVY)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


class LogArgvTests(unittest.TestCase):
    def setUp(self) -> None:
        self.tmp = tempfile.mkdtemp(prefix="heavy-argv-")
        self.state = str(Path(self.tmp) / "state")
        self.addCleanup(shutil.rmtree, self.tmp, True)

    def test_the_log_line_carries_the_argv(self) -> None:
        cp = run_broker(
            ["--kind", "node", "--", sys.executable, "-c", "pass", "--marker"],
            clean_env(self.state),
        )
        self.assertEqual(cp.returncode, 0, cp.stderr)
        rows = read_log(self.state)
        self.assertEqual(len(rows), 1, rows)
        self.assertEqual(rows[0]["argv"], [sys.executable, "-c", "pass", "--marker"])

    def test_a_long_argv_is_clipped(self) -> None:
        heavy = load_heavy()
        argv = ["cargo", "test", "x" * 1000] + [f"a{i}" for i in range(100)]
        logged = heavy._log_argv(argv)
        self.assertLessEqual(len(logged), heavy.LOG_ARGV_WORDS + 1)
        self.assertTrue(all(len(w) <= heavy.LOG_ARGV_CHARS + 1 for w in logged))
        self.assertEqual(logged[:2], ["cargo", "test"])
        self.assertTrue(logged[-1].startswith("…"))

    def test_a_timeout_row_carries_the_argv(self) -> None:
        heavy = load_heavy()
        directory = Path(self.tmp) / "timeout-state"
        heavy._log_timeout(directory, "agent", 1, "node", 1, 0.5, ["npm", "test", "--x"])
        rows = [json.loads(l) for l in (directory / "log.jsonl").read_text(encoding="utf-8").splitlines()]
        self.assertEqual(len(rows), 1, rows)
        self.assertEqual(rows[0]["argv"], ["npm", "test", "--x"])


if __name__ == "__main__":
    unittest.main()
