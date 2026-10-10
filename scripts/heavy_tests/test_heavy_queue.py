#!/usr/bin/env python3
"""RED tests for the heavy broker's machine token queue (`scripts/heavy.py`): weighted
admission, priority-then-FIFO order, head-of-line blocking, the wait cap (exit 75),
dead-entry reaping, nesting under a held token and `status`.

The broker does not exist yet (or lacks the queue), so every test here fails for that
reason. Hermetic: state lives in a temp NUCLEOS_HEAVY_DIR and the "heavy commands" are a
fake `cargo` launcher (weight 2 for `test`, 1 for `check`, as heavy_classify decides) that
runs a sleeper waiting on a stop file. Nothing real is compiled.
"""

from __future__ import annotations

import importlib.util
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

SLEEPER = r'''
import os, pathlib, subprocess, sys, time

d = pathlib.Path(os.environ["SLEEPER_DIR"])
tag = sys.argv[-1]
# order.txt first, the .start marker last: tests treat the marker as "started" and then read
# order.txt, so the marker must not be visible before the line it vouches for.
with open(d / "order.txt", "a") as f:
    f.write(tag + "\n")
(d / f"{tag}.start").write_text(str(os.getpid()))
(d / f"{tag}.token").write_text(os.environ.get("NUCLEOS_HEAVY_TOKEN", ""))
if os.environ.get("SLEEPER_NESTED"):
    # A nested broker call under the token this broker holds: it must take no second token.
    (d / "inner.stop").write_text("x")
    env = dict(os.environ)
    env["NUCLEOS_HEAVY_WAIT_MAX"] = "3"
    cp = subprocess.run(
        [sys.executable, os.environ["SLEEPER_HEAVY"], "--", os.environ["SLEEPER_CARGO"],
         "test", "inner"],
        env=env, capture_output=True, text=True, timeout=60,
    )
    (d / f"{tag}.nested").write_text(str(cp.returncode))
while not ((d / f"{tag}.stop").exists() or (d / "all.stop").exists()):
    time.sleep(0.05)
'''


def load_heavy():
    """Import scripts/heavy.py as a module; raises (red) while the file is missing."""
    spec = importlib.util.spec_from_file_location("heavy_under_test", HEAVY)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


def wait_for(pred, timeout: float = 30.0, step: float = 0.1) -> bool:
    deadline = time.time() + timeout
    while time.time() < deadline:
        if pred():
            return True
        time.sleep(step)
    return pred()


def read_log(state_dir: str) -> list[dict]:
    p = Path(state_dir) / "log.jsonl"
    if not p.exists():
        return []
    return [json.loads(line) for line in p.read_text(encoding="utf-8").splitlines() if line.strip()]


class QueueTests(unittest.TestCase):
    def setUp(self) -> None:
        self.tmp = Path(tempfile.mkdtemp(prefix="heavy-q-"))
        self.state = self.tmp / "state"
        self.d = self.tmp / "marks"
        self.d.mkdir()
        self.procs: list[subprocess.Popen] = []
        (self.tmp / "sleeper.py").write_text(SLEEPER, encoding="utf-8")
        # The fake cargo: classified by its basename, run as a program.
        if os.name == "nt":
            self.cargo = self.tmp / "cargo.cmd"
            self.cargo.write_text(
                f'@echo off\r\n"{sys.executable}" "{self.tmp / "sleeper.py"}" %*\r\n'
                "exit /b %ERRORLEVEL%\r\n",
                encoding="utf-8",
            )
        else:
            self.cargo = self.tmp / "cargo"
            self.cargo.write_text(
                f'#!/bin/sh\nexec "{sys.executable}" "{self.tmp / "sleeper.py"}" "$@"\n',
                encoding="utf-8",
            )
            self.cargo.chmod(0o755)
        self.addCleanup(self._teardown)

    def _teardown(self) -> None:
        (self.d / "all.stop").write_text("x")
        for p in self.procs:
            try:
                p.wait(timeout=15)
            except Exception:
                p.kill()
        shutil.rmtree(self.tmp, True)

    # ---- helpers -------------------------------------------------------------------
    def env(self, capacity: int = 4, **extra: str) -> dict:
        env = dict(os.environ)
        for k in ("NUCLEOS_HEAVY", "NUCLEOS_HEAVY_TOKEN", "NUCLEOS_HEAVY_HELD",
                  "NUCLEOS_HEAVY_PRIO", "NUCLEOS_HEAVY_WAIT_MAX", "NUCLEOS_HEAVY_MAIN"):
            env.pop(k, None)
        env.update(
            NUCLEOS_HEAVY_DIR=str(self.state),
            NUCLEOS_HEAVY_CAPACITY=str(capacity),
            NUCLEOS_HEAVY_POLL_S="0.1",
            SLEEPER_DIR=str(self.d),
            SLEEPER_HEAVY=str(HEAVY),
            SLEEPER_CARGO=str(self.cargo),
        )
        env.update(extra)
        return env

    def broker(self, tag: str, sub: str = "test", prio: int | None = 1, agent: str | None = None,
               capacity: int = 4, wait_max: str | None = None, **extra_env: str) -> subprocess.Popen:
        args = [sys.executable, str(HEAVY)]
        if prio is not None:
            args += ["--prio", str(prio)]
        if agent:
            args += ["--agent", agent]
        if wait_max is not None:
            args += ["--wait-max", wait_max]
        args += ["--", str(self.cargo), sub, tag]
        p = subprocess.Popen(
            args, env=self.env(capacity, **extra_env), cwd=str(ROOT),
            stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
        )
        self.procs.append(p)
        return p

    def started(self, tag: str) -> bool:
        return (self.d / f"{tag}.start").exists()

    def stop(self, tag: str) -> None:
        (self.d / f"{tag}.stop").write_text("x")

    def queue_files(self) -> list[Path]:
        q = self.state / "queue"
        return sorted(q.iterdir()) if q.exists() else []

    def held_files(self) -> list[Path]:
        h = self.state / "held"
        return sorted(h.iterdir()) if h.exists() else []

    def assert_started(self, tag: str) -> None:
        self.assertTrue(wait_for(lambda: self.started(tag)), f"{tag} never started")

    def assert_stays_queued(self, tag: str, secs: float = 1.2) -> None:
        time.sleep(secs)
        self.assertFalse(self.started(tag), f"{tag} was admitted past a full budget")

    def queued_count(self, n: int) -> None:
        self.assertTrue(
            wait_for(lambda: len(self.queue_files()) >= n), f"queue never reached {n} entries"
        )

    def finish(self, p: subprocess.Popen) -> tuple[int, str, str]:
        out, err = p.communicate(timeout=30)
        return p.returncode, out, err

    # 22
    def test_admission_by_weight_capacity(self) -> None:
        a = self.broker("a", "test")   # weight 2
        b = self.broker("b", "test")   # weight 2
        self.assert_started("a")
        self.assert_started("b")
        self.assertTrue(wait_for(lambda: len(self.held_files()) == 2))
        self.assertEqual(sorted(f.name for f in self.held_files()), sorted([str(a.pid), str(b.pid)]))
        # Budget 4 is spent: a weight-1 request waits, and is visible in the queue.
        c = self.broker("c", "check")  # weight 1
        self.queued_count(1)
        self.assert_stays_queued("c")
        self.assertIsNone(c.poll())
        # Freeing a weight-2 slot admits it.
        self.stop("a")
        self.assert_started("c")
        self.stop("b")
        self.stop("c")
        for p in (a, b, c):
            self.assertEqual(self.finish(p)[0], 0)
        self.assertTrue(wait_for(lambda: not self.held_files()), "held files were left behind")
        self.assertEqual(self.queue_files(), [])

        # A weight above the capacity is clamped to it, so a lone request still runs.
        d = self.broker("d", "test", capacity=1)
        self.assert_started("d")
        self.stop("d")
        self.assertEqual(self.finish(d)[0], 0)

    # 23
    def test_priority_then_fifo(self) -> None:
        h = self.broker("h", "test", capacity=2)
        self.assert_started("h")
        order = [("x", 1), ("y", 2), ("z", 0), ("w", 1)]
        procs = []
        for i, (tag, prio) in enumerate(order, start=1):
            procs.append(self.broker(tag, "test", prio=prio, capacity=2))
            self.queued_count(i)       # distinct arrival times: one request at a time
            time.sleep(0.05)
        # Capacity 2 admits one weight-2 request at a time: z(0), then x and w (1, FIFO), then y(2).
        self.stop("h")
        seen = []
        for expect in ("z", "x", "w", "y"):
            self.assertTrue(wait_for(lambda e=expect: self.started(e)), f"{expect} did not start")
            seen = (self.d / "order.txt").read_text().split()
            self.assertEqual(seen[-1], expect, f"admission order was {seen}")
            self.stop(expect)
        self.assertEqual(seen, ["h", "z", "x", "w", "y"])
        for p in procs:
            self.assertEqual(self.finish(p)[0], 0)
        self.assertEqual(self.finish(h)[0], 0)

    # 24
    def test_heavy_head_blocks_smaller_behind(self) -> None:
        h1 = self.broker("h1", "test")      # weight 2
        h2 = self.broker("h2", "check")     # weight 1 -> 3 of 4 held
        self.assert_started("h1")
        self.assert_started("h2")
        self.assertTrue(wait_for(lambda: len(self.held_files()) == 2))
        big = self.broker("big", "test")    # weight 2: does not fit (3 + 2 > 4), older
        self.queued_count(1)
        time.sleep(0.05)
        small = self.broker("small", "check")  # weight 1: would fit, but the head is heavier
        self.queued_count(2)
        self.assert_stays_queued("small", 1.5)
        self.assertFalse(self.started("big"))
        # Releasing the weight-1 holder lets the head in; the small one still waits behind it.
        self.stop("h2")
        self.assert_started("big")
        self.assert_stays_queued("small", 1.0)
        # Only when the head is gone does the small request run.
        self.stop("big")
        self.assert_started("small")
        order = (self.d / "order.txt").read_text().split()
        self.assertEqual(order[-2:], ["big", "small"], order)
        for t in ("h1", "small"):
            self.stop(t)
        for p in (h1, h2, big, small):
            self.assertEqual(self.finish(p)[0], 0)

    # 25
    def test_agent_wait_cap_exit_75(self) -> None:
        h = self.broker("h", "test", capacity=2)
        self.assert_started("h")

        def waiter(tag, *, agent=None, wait_max=None, env_wait=None):
            extra = {} if env_wait is None else {"NUCLEOS_HEAVY_WAIT_MAX": env_wait}
            return self.broker(tag, "test", agent=agent, capacity=2, wait_max=wait_max, **extra)

        # NUCLEOS_HEAVY_WAIT_MAX overrides the 3600 s default of a non-agent caller.
        p = waiter("w1", env_wait="1")
        rc, out, err = self.finish(p)
        self.assertEqual(rc, 75, err)
        self.assertIn("in queue for", err)
        self.assertIn("nothing was compiled", err)
        self.assertIn("try again", err)
        self.assertIn("600000", err, "the message must say how to re-run with a longer Bash timeout")
        self.assertFalse(self.started("w1"))
        # The abandoned request leaves no queue entry behind.
        self.assertEqual(self.queue_files(), [])

        # --wait-max beats the environment (which here would wait a minute and a half).
        t0 = time.time()
        p = waiter("w2", agent="agent-w", wait_max="1", env_wait="90")
        rc, out, err = self.finish(p)
        self.assertEqual(rc, 75, err)
        self.assertLess(time.time() - t0, 30, "--wait-max did not take precedence")
        self.assertFalse(self.started("w2"))
        self.assertEqual(self.queue_files(), [])

        # The cap never touches the holder.
        self.assertIsNone(h.poll())
        self.stop("h")
        self.assertEqual(self.finish(h)[0], 0)

    # 26
    def test_dead_holder_reaped_including_reused_pid(self) -> None:
        heavy = load_heavy()
        dead = subprocess.Popen([sys.executable, "-c", "pass"])
        dead.wait()
        _, my_ctime = heavy.proc_identity(os.getpid())

        def plant(kind: str, name: str, pid: int, ctime, weight: int = 2, prio: int = 0) -> Path:
            d = self.state / kind
            d.mkdir(parents=True, exist_ok=True)
            f = d / name
            f.write_text(json.dumps({
                "pid": pid, "ctime": ctime, "weight": weight, "prio": prio,
                "agent": None, "worktree": "planted", "argv": ["planted"],
            }))
            return f

        # A live holder (right pid AND right creation time) really holds the budget.
        live = plant("held", str(os.getpid()), os.getpid(), my_ctime)
        p = self.broker("live", "test", capacity=2, wait_max="1")
        rc, _, err = self.finish(p)
        self.assertEqual(rc, 75, err)
        self.assertFalse(self.started("live"))
        self.assertTrue(live.exists(), "a live holder must not be reaped")
        live.unlink()

        # Same pid, other creation time: the pid was reused, the holder is gone.
        if my_ctime is not None:
            reused = plant("held", str(os.getpid()), os.getpid(), my_ctime + 10_000_000)
            p = self.broker("reused", "test", capacity=2, wait_max="20")
            self.assert_started("reused")
            self.assertFalse(reused.exists(), "a reused pid must be reaped")
            self.stop("reused")
            self.assertEqual(self.finish(p)[0], 0)

        # A dead holder and a dead, older, better-priority queue head do not block anyone.
        dead_held = plant("held", str(dead.pid), dead.pid, None)
        dead_req = plant("queue", f"0-1-{dead.pid}", dead.pid, None, prio=0)
        p = self.broker("after-dead", "test", capacity=2, wait_max="20")
        self.assert_started("after-dead")
        self.assertFalse(dead_held.exists())
        self.assertFalse(dead_req.exists())
        self.stop("after-dead")
        self.assertEqual(self.finish(p)[0], 0)

    # 27
    def test_nested_token_and_env_prio(self) -> None:
        # (a) The outer broker holds the whole budget; a heavy call made by its child
        # takes no second token (it would deadlock waiting on its own parent).
        outer = self.broker("outer", "test", capacity=2, SLEEPER_NESTED="1")
        self.assert_started("outer")
        self.assertTrue(
            wait_for(lambda: (self.d / "outer.nested").exists(), 40), "the nested call hung"
        )
        self.assertEqual((self.d / "outer.nested").read_text(), "0")
        self.assertTrue(self.started("inner"))
        self.assertEqual((self.d / "outer.token").read_text(), str(outer.pid))
        self.assertEqual([f.name for f in self.held_files()], [str(outer.pid)])
        self.stop("outer")
        self.assertEqual(self.finish(outer)[0], 0)

        # (b) Priority: --prio > NUCLEOS_HEAVY_PRIO > 1.
        for tag, prio_flag, env_prio, want in (
            ("p-env", None, "0", 0),
            ("p-flag", 2, "0", 2),
            ("p-default", None, None, 1),
        ):
            self.stop(tag)   # the sleeper returns at once
            extra = {} if env_prio is None else {"NUCLEOS_HEAVY_PRIO": env_prio}
            p = self.broker(tag, "check", prio=prio_flag, **extra)
            rc, _, err = self.finish(p)
            self.assertEqual(rc, 0, err)
        rows = [r for r in read_log(str(self.state)) if r.get("argv0")]
        prios = [r["prio"] for r in rows[-3:]]
        self.assertEqual(prios, [0, 2, 1], rows[-3:])

    # 28
    def test_status_lists_queue_and_holders(self) -> None:
        h = self.broker("hold1", "test", prio=1, agent="agent-h", capacity=2)
        self.assert_started("hold1")
        q = self.broker("queued1", "check", prio=0, agent="agent-q", capacity=2)
        self.queued_count(1)

        # The request file is queue/<prio>-<epoch_ns>-<pid>, holders are held/<pid>.
        qf = self.queue_files()[0]
        self.assertRegex(qf.name, rf"^0-\d+-{q.pid}$")
        rec = json.loads(qf.read_text())
        for key in ("pid", "ctime", "weight", "prio", "agent", "worktree", "argv"):
            self.assertIn(key, rec)
        self.assertEqual((rec["pid"], rec["weight"], rec["prio"], rec["agent"]),
                         (q.pid, 1, 0, "agent-q"))
        hf = self.held_files()
        self.assertEqual([f.name for f in hf], [str(h.pid)])
        hrec = json.loads(hf[0].read_text())
        self.assertEqual((hrec["pid"], hrec["weight"], hrec["agent"]), (h.pid, 2, "agent-h"))

        # `status` takes no token: it answers while the budget is full.
        cp = subprocess.run(
            [sys.executable, str(HEAVY), "status"], env=self.env(2), cwd=str(ROOT),
            capture_output=True, text=True, timeout=30,
        )
        self.assertEqual(cp.returncode, 0, cp.stderr)
        out = cp.stdout
        for needle in ("agent-h", "agent-q", "queued1", "hold1", str(h.pid), str(q.pid)):
            self.assertIn(needle, out)
        # Queue first, then holders.
        self.assertLess(out.index("queued1"), out.index("hold1"), out)

        self.stop("hold1")
        self.assert_started("queued1")
        self.stop("queued1")
        self.assertEqual(self.finish(h)[0], 0)
        self.assertEqual(self.finish(q)[0], 0)

        # An empty state prints without failing.
        shutil.rmtree(self.state, True)
        cp = subprocess.run(
            [sys.executable, str(HEAVY), "status"], env=self.env(2), cwd=str(ROOT),
            capture_output=True, text=True, timeout=30,
        )
        self.assertEqual(cp.returncode, 0, cp.stderr)
        self.assertIsNone(re.search(r"Traceback", cp.stderr))


class MutexTraceTests(unittest.TestCase):
    """The broker mutex records, in `mutex.jsonl`, every timeout (with the holder it waited
    on) and every acquisition that waited or held longer than MUTEX_TRACE_S, naming the
    function that took it. Fast acquisitions write nothing."""

    def setUp(self) -> None:
        self.heavy = load_heavy()
        self.heavy.MUTEX_TRACE_S = 0.1
        self.tmp = Path(tempfile.mkdtemp(prefix="heavy-mutex-"))

    def tearDown(self) -> None:
        shutil.rmtree(self.tmp, True)

    def rows(self) -> list[dict]:
        path = self.tmp / "mutex.jsonl"
        if not path.exists():
            return []
        return [json.loads(l) for l in path.read_text(encoding="utf-8").splitlines() if l]

    def test_fast_acquisition_writes_nothing(self) -> None:
        with self.heavy.Mutex(self.tmp):
            pass
        self.assertEqual(self.rows(), [])

    def test_slow_hold_is_recorded_with_its_site(self) -> None:
        with self.heavy.Mutex(self.tmp):
            time.sleep(0.25)
        rows = self.rows()
        self.assertEqual(len(rows), 1, rows)
        row = rows[0]
        self.assertEqual(row["event"], "slow")
        self.assertEqual(row["site"], "test_slow_hold_is_recorded_with_its_site")
        self.assertEqual(row["pid"], os.getpid())
        self.assertGreaterEqual(row["held_s"], 0.2)

    def test_timeout_names_the_holder(self) -> None:
        # A live holder other than this process: one in this process's own name that it
        # does not hold is a leftover, and the mutex reclaims it instead of timing out.
        holder = subprocess.Popen([sys.executable, "-c", "import time; time.sleep(30)"])
        self.addCleanup(holder.kill)
        m = self.tmp / ".mutex"
        m.mkdir()
        _, ctime = self.heavy.proc_identity(holder.pid)
        (m / "owner").write_text(json.dumps(
            {"pid": holder.pid, "ctime": ctime, "site": "holder_site", "t": time.time()}))
        with self.assertRaises(TimeoutError):
            self.heavy.Mutex(self.tmp, timeout=0.3).acquire()
        rows = self.rows()
        self.assertEqual(len(rows), 1, rows)
        row = rows[0]
        self.assertEqual(row["event"], "timeout")
        self.assertEqual(row["site"], "test_timeout_names_the_holder")
        self.assertGreaterEqual(row["waited_s"], 0.3)
        self.assertGreater(row["n_exists"] + row["n_perm"], 0)
        self.assertEqual(row["owner"]["site"], "holder_site")
        self.assertEqual(row["owner"]["pid"], holder.pid)
        self.assertTrue(row["owner_alive"])
        self.assertIn("owner_held_s", row)


if __name__ == "__main__":
    unittest.main()
