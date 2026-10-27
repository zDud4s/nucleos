#!/usr/bin/env python3
"""An exit-75 row says what the run waited on, and `report` counts them per cause."""

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
from unittest import mock
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from test_heavy_runtime import HEAVY, LOG_FIELDS, ROOT, clean_env, read_log, run_broker  # noqa: E402


def load_heavy():
    spec = importlib.util.spec_from_file_location("heavy_under_test", HEAVY)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


def _rows(directory: Path) -> list[dict]:
    text = (directory / "log.jsonl").read_text(encoding="utf-8")
    return [json.loads(l) for l in text.splitlines() if l.strip()]


class TimeoutLogTests(unittest.TestCase):
    def setUp(self) -> None:
        self.tmp = tempfile.mkdtemp(prefix="heavy-tolog-")
        self.state = str(Path(self.tmp) / "state")
        self.addCleanup(shutil.rmtree, self.tmp, True)

    def _plant_lock(self, heavy, directory: Path, h: str, hold: bool) -> None:
        p = subprocess.Popen([sys.executable, "-c", "import time; time.sleep(60)"])
        self.addCleanup(p.wait)
        self.addCleanup(p.kill)
        lock = heavy._lock_paths(directory, h)[0]
        lock.parent.mkdir(parents=True, exist_ok=True)
        lock.write_text(json.dumps({
            "pid": p.pid, "ctime": heavy.proc_identity(p.pid)[1], "prio": 1,
            "argv": ["x"], "worktree": "w", "acquired": time.time(), "hold": hold,
        }), encoding="utf-8")

    def _poll_env(self) -> None:
        old = os.environ.get("NUCLEOS_HEAVY_POLL_S")
        os.environ["NUCLEOS_HEAVY_POLL_S"] = "0.1"

        def restore() -> None:
            if old is None:
                os.environ.pop("NUCLEOS_HEAVY_POLL_S", None)
            else:
                os.environ["NUCLEOS_HEAVY_POLL_S"] = old

        self.addCleanup(restore)

    def _acquire(self, heavy, directory: Path, h: str, cap: float, **kw):
        rec = {"pid": os.getpid(), "prio": 1, "argv": ["y"]}
        return heavy.acquire_lock(directory, h, rec, cap, **kw)

    def test_log_timeout_records_waited_on_and_splits_the_waits(self) -> None:
        heavy = load_heavy()
        directory = Path(self.tmp) / "split-state"
        heavy._log_timeout(directory, "agent", 1, "node", 1, 2.0, ["npm", "test"],
                           waited_on="token", wait_token=7.5)
        rows = _rows(directory)
        self.assertEqual(len(rows), 1, rows)
        row = rows[0]
        self.assertTrue(LOG_FIELDS <= set(row), LOG_FIELDS - set(row))
        self.assertEqual(row["waited_on"], "token")
        self.assertEqual(row["wait_lock_s"], 2.0)
        self.assertEqual(row["wait_token_s"], 7.5)
        self.assertEqual(row["exit"], heavy.EXIT_QUEUE_TIMEOUT)

    def test_log_timeout_positional_call_defaults_to_a_lock_wait(self) -> None:
        heavy = load_heavy()
        directory = Path(self.tmp) / "legacy-state"
        heavy._log_timeout(directory, "agent", 1, "node", 1, 0.5, ["npm", "test"])
        row = _rows(directory)[0]
        self.assertEqual(row["waited_on"], "lock")
        self.assertEqual(row["wait_token_s"], 0.0)
        self.assertEqual(row["wait_lock_s"], 0.5)

    def test_acquire_lock_names_a_gate_refusal(self) -> None:
        heavy = load_heavy()
        self._poll_env()
        directory, h = Path(self.tmp) / "gate-state", "0" * 16
        self._plant_lock(heavy, directory, h, hold=True)
        why: dict = {}
        got, _, _ = self._acquire(heavy, directory, h, 30, fail_on_hold=True, why=why)
        self.assertFalse(got)
        self.assertEqual(why.get("waited_on"), "gate")

    def test_acquire_lock_names_an_estimate_refusal(self) -> None:
        heavy = load_heavy()
        self._poll_env()
        heavy._holder_estimate = lambda d, h: 1e6
        directory, h = Path(self.tmp) / "est-state", "0" * 16
        self._plant_lock(heavy, directory, h, hold=False)
        why: dict = {}
        got, _, _ = self._acquire(heavy, directory, h, 30, fail_on_hold=True, why=why)
        self.assertFalse(got)
        self.assertEqual(why.get("waited_on"), "estimate")

    def test_acquire_lock_names_a_lock_timeout(self) -> None:
        heavy = load_heavy()
        self._poll_env()
        heavy._holder_estimate = lambda d, h: None
        directory, h = Path(self.tmp) / "lock-state", "0" * 16
        self._plant_lock(heavy, directory, h, hold=False)
        why: dict = {}
        got, _, _ = self._acquire(heavy, directory, h, 0.5, fail_on_hold=False, why=why)
        self.assertFalse(got)
        self.assertEqual(why.get("waited_on"), "lock")

    def test_a_token_refusal_row_says_token(self) -> None:
        stop = Path(self.tmp) / "stop"
        marker = Path(self.tmp) / "marker"
        loop = (
            "import pathlib, time\n"
            f"pathlib.Path({str(marker)!r}).write_text('up')\n"
            f"s = pathlib.Path({str(stop)!r})\n"
            "while not s.exists():\n"
            "    time.sleep(0.1)\n"
        )
        env = clean_env(self.state, NUCLEOS_HEAVY_CAPACITY="1", NUCLEOS_HEAVY_POLL_S="0.1")
        holder = subprocess.Popen(
            [sys.executable, str(HEAVY), "--kind", "node", "--", sys.executable, "-c", loop],
            env=env, cwd=str(ROOT), stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
        )

        def finish() -> None:
            stop.write_text("x")
            try:
                holder.communicate(timeout=30)
            except subprocess.TimeoutExpired:
                holder.kill()
                holder.communicate()

        self.addCleanup(finish)
        deadline = time.time() + 30
        while not marker.exists() and time.time() < deadline:
            time.sleep(0.1)
        self.assertTrue(marker.exists(), "the holder never started")
        cp = run_broker(
            ["--kind", "node", "--wait-max", "1", "--", sys.executable, "-c", "pass"], env)
        self.assertEqual(cp.returncode, 75, cp.stderr)
        rows = [r for r in read_log(self.state) if r.get("exit") == 75]
        self.assertEqual(len(rows), 1, rows)
        self.assertEqual(rows[0].get("waited_on"), "token")
        self.assertEqual(rows[0]["wait_lock_s"], 0.0)
        self.assertGreaterEqual(rows[0]["wait_token_s"], 0.5)

    def test_an_agent_behind_a_gate_row_says_gate(self) -> None:
        heavy = load_heavy()
        old_cwd = os.getcwd()
        os.chdir(ROOT)
        try:
            h = heavy.worktree_hash(heavy.worktree_root())
        finally:
            os.chdir(old_cwd)
        self._plant_lock(heavy, Path(self.state), h, hold=True)
        env = clean_env(self.state, NUCLEOS_HEAVY_TARGET="0", NUCLEOS_HEAVY_POLL_S="0.1")
        cp = run_broker(
            ["--agent", "t", "--kind", "cargo", "--wait-max", "5", "--",
             sys.executable, "-c", "pass"], env)
        self.assertEqual(cp.returncode, 75, cp.stderr)
        rows = [r for r in read_log(self.state) if r.get("exit") == 75]
        self.assertEqual(len(rows), 1, rows)
        self.assertEqual(rows[0].get("waited_on"), "gate")
        self.assertEqual(rows[0]["wait_token_s"], 0.0)

    def _attempt(self, heavy, token_error: Exception):
        """Run one in-process broker attempt whose token acquisition raises `token_error`;
        returns (exit code, argvs the child was started with)."""
        started: list = []

        def fake_run_child(argv, env, job, **kw):
            started.append(list(argv))
            return 0, {"compiled": False, "test_failed": False, "compile_error": False}

        def boom(*a, **kw):
            raise token_error

        env = {"NUCLEOS_HEAVY_DIR": self.state}
        with mock.patch.dict(os.environ, env),                 mock.patch.object(heavy, "acquire_token", boom),                 mock.patch.object(heavy, "run_child", fake_run_child):
            for k in ("NUCLEOS_HEAVY", "NUCLEOS_HEAVY_TOKEN", "NUCLEOS_HEAVY_HELD"):
                os.environ.pop(k, None)
            code = heavy._broker_attempt(["--kind", "node", "--", "npm", "test"])
        return code, started

    def test_a_busy_mutex_in_admission_returns_75_and_never_runs_the_child(self) -> None:
        heavy = load_heavy()
        code, started = self._attempt(heavy, heavy.MutexBusy("broker mutex busy"))
        self.assertEqual(code, heavy.EXIT_QUEUE_TIMEOUT)
        self.assertEqual(started, [])
        rows = [r for r in _rows(Path(self.state)) if r.get("exit") == 75]
        self.assertEqual(len(rows), 1, rows)
        self.assertEqual(rows[0].get("waited_on"), "mutex")
        self.assertNotIn("broker_error", rows[0])

    def test_another_broker_exception_still_fails_open(self) -> None:
        heavy = load_heavy()
        code, started = self._attempt(heavy, RuntimeError("bug"))
        self.assertEqual(code, 0)
        self.assertEqual(started, [["npm", "test"]])
        rows = _rows(Path(self.state))
        self.assertTrue(any("RuntimeError" in str(r.get("broker_error")) for r in rows), rows)

    def test_report_splits_exit_75_by_waited_on(self) -> None:
        heavy = load_heavy()
        directory = Path(self.state)

        def row(exit_code: int, waited_on: str | None) -> dict:
            r = {k: 0 for k in LOG_FIELDS}
            r.update({
                "v": heavy.LOG_VERSION, "ts": time.strftime("%Y-%m-%dT%H:%M:%S%z"),
                "worktree": "w", "agent": "a", "kind": "node", "compiled": False,
                "fp_hit": False, "fp_miss": False, "exit": exit_code, "argv0": "x",
            })
            if waited_on is not None:
                r["waited_on"] = waited_on
            return r

        for r in (row(75, "token"), row(75, "token"), row(75, "lock"),
                  row(75, None), row(0, "gate")):
            heavy.append_log(r, directory)
        cp = run_broker(["report"], clean_env(self.state))
        self.assertEqual(cp.returncode, 0, cp.stderr)
        self.assertIn("waited_on token: 2", cp.stdout)
        self.assertIn("waited_on lock: 1", cp.stdout)
        self.assertIn("waited_on unknown: 1", cp.stdout)
        self.assertNotIn("waited_on gate", cp.stdout)


if __name__ == "__main__":
    unittest.main()
