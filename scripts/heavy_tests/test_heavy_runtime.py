#!/usr/bin/env python3
"""RED tests for the heavy broker runtime (`scripts/heavy.py`): liveness, mutex,
Job-object child execution, NUCLEOS_HEAVY=0, fail-open and the telemetry line.

The broker does not exist yet, so every test here fails for that reason. Children are
`sys.executable -c` one-liners; state lives in a temp NUCLEOS_HEAVY_DIR.
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

LOG_FIELDS = {
    "v", "ts", "worktree", "agent", "prio", "eff_prio", "kind", "weight",
    "wait_lock_s", "wait_token_s", "run_s", "compiled", "fp_hit", "fp_miss",
    "exit", "argv0",
}


def load_heavy():
    """Import scripts/heavy.py as a module; raises (red) while the file is missing."""
    spec = importlib.util.spec_from_file_location("heavy_under_test", HEAVY)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


def clean_env(state_dir: str | None, **extra: str) -> dict:
    env = dict(os.environ)
    for k in ("NUCLEOS_HEAVY", "NUCLEOS_HEAVY_TOKEN", "NUCLEOS_HEAVY_HELD",
              "NUCLEOS_HEAVY_PRIO", "NUCLEOS_HEAVY_DIR", "NUCLEOS_HEAVY_MAIN"):
        env.pop(k, None)
    if state_dir is not None:
        env["NUCLEOS_HEAVY_DIR"] = state_dir
    env.update(extra)
    return env


def run_broker(args: list[str], env: dict, timeout: float = 60) -> subprocess.CompletedProcess:
    return subprocess.run(
        [sys.executable, str(HEAVY), *args],
        env=env, capture_output=True, text=True, timeout=timeout, cwd=str(ROOT),
    )


def read_log(state_dir: str) -> list[dict]:
    p = Path(state_dir) / "log.jsonl"
    if not p.exists():
        return []
    return [json.loads(line) for line in p.read_text(encoding="utf-8").splitlines() if line.strip()]


def pid_listed(pid: int) -> bool:
    out = subprocess.run(
        ["tasklist", "/FI", f"PID eq {pid}", "/NH"], capture_output=True, text=True
    ).stdout
    return str(pid) in out


class RuntimeTests(unittest.TestCase):
    def setUp(self) -> None:
        self.tmp = tempfile.mkdtemp(prefix="heavy-rt-")
        self.state = str(Path(self.tmp) / "state")
        self.addCleanup(shutil.rmtree, self.tmp, True)

    # 16
    def test_liveness_pid_and_creation_time(self) -> None:
        heavy = load_heavy()
        alive, ctime = heavy.proc_identity(os.getpid())
        self.assertTrue(alive)
        if os.name == "nt":
            self.assertIsInstance(ctime, int)
            self.assertGreater(ctime, 0)
        self.assertTrue(heavy.is_alive(os.getpid(), ctime))
        if os.name == "nt":
            # Same pid, different creation time == a reused pid: not the same process.
            self.assertFalse(heavy.is_alive(os.getpid(), ctime + 10_000_000))
        # A pid that has exited is dead, even when no creation time is known.
        child = subprocess.Popen([sys.executable, "-c", "pass"])
        child.wait()
        dead_alive, _ = heavy.proc_identity(child.pid)
        self.assertFalse(dead_alive)
        self.assertFalse(heavy.is_alive(child.pid, None))

    # 17
    @unittest.skipUnless(os.name == "nt", "Job objects are Windows-only")
    def test_job_object_kills_child_when_broker_killed(self) -> None:
        pidfile = Path(self.tmp) / "child.pid"
        code = (
            "import os,time,pathlib;"
            f"pathlib.Path({str(pidfile)!r}).write_text(str(os.getpid()));"
            "time.sleep(120)"
        )
        broker = subprocess.Popen(
            [sys.executable, str(HEAVY), "--", sys.executable, "-c", code],
            env=clean_env(self.state), cwd=str(ROOT),
            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
        )
        child_pid = None
        try:
            deadline = time.time() + 30
            while time.time() < deadline:
                if pidfile.exists() and pidfile.read_text().strip():
                    child_pid = int(pidfile.read_text().strip())
                    break
                time.sleep(0.2)
            self.assertIsNotNone(child_pid, "the child never started under the broker")
            self.assertTrue(pid_listed(child_pid))
            # Hard kill: no chance for the broker to clean up after itself.
            subprocess.run(["taskkill", "/F", "/PID", str(broker.pid)], capture_output=True)
            broker.wait(timeout=15)
            deadline = time.time() + 15
            while time.time() < deadline and pid_listed(child_pid):
                time.sleep(0.2)
            self.assertFalse(pid_listed(child_pid), "the child outlived its broker")
        finally:
            if broker.poll() is None:
                broker.kill()
            if child_pid is not None and pid_listed(child_pid):
                subprocess.run(["taskkill", "/F", "/PID", str(child_pid)], capture_output=True)

    # 18
    def test_stale_mutex_reclaimed(self) -> None:
        heavy = load_heavy()
        dead = subprocess.Popen([sys.executable, "-c", "pass"])
        dead.wait()
        state = Path(self.state)

        def plant(owner_pid: int, age_s: float) -> None:
            mutex = state / ".mutex"
            if mutex.exists():
                shutil.rmtree(mutex)
            mutex.mkdir(parents=True)
            (mutex / "owner").write_text(json.dumps({"pid": owner_pid, "ctime": None}))
            old = time.time() - age_s
            os.utime(mutex, (old, old))

        # (a) fresh mutex whose owner is dead; (b) live owner but older than 60 s.
        for owner, age in ((dead.pid, 1), (os.getpid(), 120)):
            plant(owner, age)
            t0 = time.time()
            cp = run_broker(
                ["--", sys.executable, "-c", "print('after-stale')"], clean_env(self.state), 30
            )
            self.assertEqual(cp.returncode, 0, cp.stderr)
            self.assertIn("after-stale", cp.stdout)
            self.assertLess(time.time() - t0, 30)
            self.assertFalse((state / ".mutex").exists(), "the mutex was left behind")
        self.assertTrue(hasattr(heavy, "proc_identity"))

    # 19
    def test_bypass_and_fail_open(self) -> None:
        # NUCLEOS_HEAVY=0: run directly, touch no state, pass the exit code through.
        cp = run_broker(
            ["--", sys.executable, "-c", "import sys;print('direct');sys.exit(5)"],
            clean_env(self.state, NUCLEOS_HEAVY="0"),
        )
        self.assertEqual(cp.returncode, 5, cp.stderr)
        self.assertIn("direct", cp.stdout)
        self.assertFalse(Path(self.state).exists(), "NUCLEOS_HEAVY=0 must not create state")

        # Fail-open: a state dir that cannot exist (its parent is a file) must not block
        # the command.
        blocker = Path(self.tmp) / "not-a-dir"
        blocker.write_text("x")
        cp = run_broker(
            ["--", sys.executable, "-c", "print('ran-anyway')"],
            clean_env(str(blocker / "state")),
        )
        self.assertEqual(cp.returncode, 0, cp.stderr)
        self.assertIn("ran-anyway", cp.stdout)

    # 20
    def test_passthrough_and_log_line(self) -> None:
        cp = run_broker(
            ["--agent", "agent-x", "--prio", "2", "--kind", "cargo", "--",
             sys.executable, "-c",
             "import sys;print('out-line');print('   Compiling foo v0.1.0');"
             "print('err-line',file=sys.stderr);sys.exit(3)"],
            clean_env(self.state),
        )
        self.assertEqual(cp.returncode, 3, cp.stderr)
        self.assertIn("out-line", cp.stdout)
        self.assertIn("Compiling foo", cp.stdout)
        self.assertIn("err-line", cp.stderr)

        rows = read_log(self.state)
        self.assertEqual(len(rows), 1, rows)
        row = rows[0]
        self.assertTrue(LOG_FIELDS <= set(row), f"missing: {LOG_FIELDS - set(row)}")
        self.assertEqual(row["v"], 1)
        self.assertEqual(row["agent"], "agent-x")
        self.assertEqual(row["prio"], 2)
        self.assertEqual(row["kind"], "cargo")
        self.assertEqual(row["exit"], 3)
        self.assertIs(row["compiled"], True)
        self.assertNotIn("broker_error", row)
        self.assertIn("python", str(row["argv0"]).lower())
        self.assertGreaterEqual(row["run_s"], 0)

        # A run that printed no `Compiling` line records compiled=false, appended as a
        # second line rather than rewriting the first.
        cp = run_broker(
            ["--", sys.executable, "-c", "print('quiet')"], clean_env(self.state)
        )
        self.assertEqual(cp.returncode, 0, cp.stderr)
        rows = read_log(self.state)
        self.assertEqual(len(rows), 2)
        self.assertIs(rows[1]["compiled"], False)
        self.assertEqual(rows[1]["exit"], 0)

    # 21
    def test_missing_double_dash_accepted(self) -> None:
        cp = run_broker(
            ["--prio", "1", sys.executable, "-c", "print('no-dashes');import sys;sys.exit(4)"],
            clean_env(self.state),
        )
        self.assertEqual(cp.returncode, 4, cp.stderr)
        self.assertIn("no-dashes", cp.stdout)
        rows = read_log(self.state)
        self.assertEqual(len(rows), 1, rows)
        self.assertEqual(rows[0]["exit"], 4)
        self.assertEqual(rows[0]["prio"], 1)


if __name__ == "__main__":
    unittest.main()
