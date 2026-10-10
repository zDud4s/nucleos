#!/usr/bin/env python3
"""RED tests for the heavy broker's per-worktree lock (`scripts/heavy.py`): `kind=cargo`
serialised per worktree under `wt/<hash>.lock`, waiters served by effective priority then
FIFO from `wt/<hash>.waiters/<prio>-<epoch_ns>-<pid>`, priority inheritance onto the lock
holder's machine-queue entry, lock-before-token ordering, and `hold-worktree` plus
`NUCLEOS_HEAVY_HELD` nesting.

The lock does not exist yet, so every test here fails for that reason. Hermetic: state lives
in a temp NUCLEOS_HEAVY_DIR, each "worktree" is a throwaway `git init` repo used as the cwd,
and the "heavy commands" are a fake `cargo` launcher that runs a sleeper waiting on a stop
file (same fixture style as test_heavy_queue.py). Nothing real is compiled.
"""

from __future__ import annotations

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

SLEEPER = r'''
import os, pathlib, sys, time

d = pathlib.Path(os.environ["SLEEPER_DIR"])
tag = sys.argv[-1]
# order.txt first, the .start marker last: tests treat the marker as "started" and then read
# order.txt, so the marker must not be visible before the line it vouches for.
with open(d / "order.txt", "a") as f:
    f.write(tag + "\n")
(d / f"{tag}.start").write_text(str(os.getpid()))
(d / f"{tag}.token").write_text(os.environ.get("NUCLEOS_HEAVY_TOKEN", ""))
while not ((d / f"{tag}.stop").exists() or (d / "all.stop").exists()):
    time.sleep(0.05)
'''

# Runs under `hold-worktree`: records what the held session sees, makes one nested broker
# call (which must not retake the worktree lock but must ask its own token), then waits.
HOLDER = r'''
import os, pathlib, subprocess, sys, time

d = pathlib.Path(os.environ["SLEEPER_DIR"])
state = pathlib.Path(os.environ["NUCLEOS_HEAVY_DIR"])
tag = sys.argv[-1]
(d / f"{tag}.start").write_text(str(os.getpid()))
(d / f"{tag}.held_env").write_text(os.environ.get("NUCLEOS_HEAVY_HELD", ""))
(d / f"{tag}.prio_env").write_text(os.environ.get("NUCLEOS_HEAVY_PRIO", ""))
(d / f"{tag}.token_env").write_text(os.environ.get("NUCLEOS_HEAVY_TOKEN", ""))
held = state / "held"
(d / f"{tag}.held_files").write_text(
    ",".join(sorted(p.name for p in held.iterdir())) if held.exists() else ""
)
if os.environ.get("HOLDER_NESTED"):
    env = dict(os.environ)
    env["NUCLEOS_HEAVY_WAIT_MAX"] = "20"
    p = subprocess.Popen(
        [sys.executable, os.environ["SLEEPER_HEAVY"], "--kind", "cargo", "--",
         os.environ["SLEEPER_CARGO"], "check", "inner"],
        env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
    )
    t0 = time.time()
    while not (d / "inner.start").exists() and time.time() - t0 < 20 and p.poll() is None:
        time.sleep(0.05)
    (d / f"{tag}.inner_started").write_text("1" if (d / "inner.start").exists() else "0")
    (d / f"{tag}.inner_held_files").write_text(
        ",".join(sorted(x.name for x in held.iterdir())) if held.exists() else ""
    )
    (d / "inner.stop").write_text("x")
    out, err = p.communicate(timeout=30)
    (d / f"{tag}.inner_rc").write_text(str(p.returncode))
while not ((d / f"{tag}.stop").exists() or (d / "all.stop").exists()):
    time.sleep(0.05)
sys.exit(int(os.environ.get("HOLDER_EXIT", "0")))
'''


def wait_for(pred, timeout: float = 30.0, step: float = 0.1) -> bool:
    deadline = time.time() + timeout
    while time.time() < deadline:
        if pred():
            return True
        time.sleep(step)
    return pred()


def git_init(path: Path) -> Path:
    path.mkdir(parents=True, exist_ok=True)
    subprocess.run(["git", "init", "-q", str(path)], check=True, capture_output=True)
    return path


class LockTests(unittest.TestCase):
    def setUp(self) -> None:
        self.tmp = Path(tempfile.mkdtemp(prefix="heavy-l-"))
        self.state = self.tmp / "state"
        self.d = self.tmp / "marks"
        self.d.mkdir()
        self.repo_a = git_init(self.tmp / "wt-a")
        self.repo_b = git_init(self.tmp / "wt-b")
        self.repo_c = git_init(self.tmp / "wt-c")
        self.procs: list[subprocess.Popen] = []
        (self.tmp / "sleeper.py").write_text(SLEEPER, encoding="utf-8")
        (self.tmp / "holder.py").write_text(HOLDER, encoding="utf-8")
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
    def env(self, capacity: int = 8, **extra: str) -> dict:
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

    def broker(self, tag: str, repo: Path, sub: str = "check", prio: int | None = 1,
               kind: str = "cargo", capacity: int = 8, wait_max: str | None = None,
               agent: str | None = None, **extra_env: str) -> subprocess.Popen:
        args = [sys.executable, str(HEAVY)]
        if prio is not None:
            args += ["--prio", str(prio)]
        if agent is not None:
            args += ["--agent", agent]
        if kind:
            args += ["--kind", kind]
        if wait_max is not None:
            args += ["--wait-max", wait_max]
        args += ["--", str(self.cargo), sub, tag]
        p = subprocess.Popen(
            args, env=self.env(capacity, **extra_env), cwd=str(repo),
            stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
        )
        self.procs.append(p)
        return p

    def hold(self, tag: str, repo: Path, prio: int | None = None, nested: bool = False,
             capacity: int = 8, **extra_env: str) -> subprocess.Popen:
        args = [sys.executable, str(HEAVY), "hold-worktree"]
        if prio is not None:
            args += ["--prio", str(prio)]
        args += ["--", sys.executable, str(self.tmp / "holder.py"), tag]
        if nested:
            extra_env["HOLDER_NESTED"] = "1"
        p = subprocess.Popen(
            args, env=self.env(capacity, **extra_env), cwd=str(repo),
            stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
        )
        self.procs.append(p)
        return p

    def started(self, tag: str) -> bool:
        return (self.d / f"{tag}.start").exists()

    def stop(self, tag: str) -> None:
        (self.d / f"{tag}.stop").write_text("x")

    def read(self, name: str) -> str:
        return (self.d / name).read_text()

    def lock_files(self) -> list[Path]:
        wt = self.state / "wt"
        return sorted(wt.glob("*.lock")) if wt.exists() else []

    def waiter_files(self) -> list[Path]:
        wt = self.state / "wt"
        out: list[Path] = []
        if wt.exists():
            for d in wt.glob("*.waiters"):
                out += [f for f in d.iterdir() if f.is_file()]
        return sorted(out)

    def queue_files(self) -> list[Path]:
        q = self.state / "queue"
        return sorted(q.iterdir()) if q.exists() else []

    def held_files(self) -> list[Path]:
        h = self.state / "held"
        return sorted(h.iterdir()) if h.exists() else []

    def assert_started(self, tag: str) -> None:
        self.assertTrue(wait_for(lambda: self.started(tag)), f"{tag} never started")

    def assert_stays_waiting(self, tag: str, secs: float = 1.2) -> None:
        time.sleep(secs)
        self.assertFalse(self.started(tag), f"{tag} ran while the worktree lock was held")

    def waiters(self, n: int) -> None:
        self.assertTrue(
            wait_for(lambda: len(self.waiter_files()) >= n), f"waiters never reached {n}"
        )

    def finish(self, p: subprocess.Popen) -> tuple[int, str, str]:
        out, err = p.communicate(timeout=40)
        return p.returncode, out, err

    # 29
    def test_cargo_serialised_per_worktree_node_not(self) -> None:
        a1 = self.broker("a1", self.repo_a)
        self.assert_started("a1")
        # The lock is a file under wt/, naming the holder.
        self.assertTrue(wait_for(lambda: len(self.lock_files()) == 1), "no wt/<hash>.lock")
        rec = json.loads(self.lock_files()[0].read_text())
        for key in ("pid", "ctime", "weight", "prio", "agent", "worktree", "argv", "start"):
            self.assertIn(key, rec)
        self.assertEqual(rec["pid"], a1.pid)

        # Same worktree, same kind: the second cargo waits for the lock, however big the budget.
        a2 = self.broker("a2", self.repo_a)
        self.waiters(1)
        self.assertRegex(self.waiter_files()[0].name, rf"^1-\d+-{a2.pid}$")
        self.assert_stays_waiting("a2")
        # Another worktree is independent.
        b1 = self.broker("b1", self.repo_b)
        self.assert_started("b1")
        self.assertEqual(len(self.lock_files()), 2)
        # A node command in the locked worktree takes no worktree lock.
        n1 = self.broker("n1", self.repo_a, kind="node")
        self.assert_started("n1")
        self.assertFalse(self.started("a2"))

        # Releasing the holder serves the waiter, and the lock file follows the holder.
        self.stop("a1")
        self.assert_started("a2")
        self.assertEqual(self.finish(a1)[0], 0)
        self.assertTrue(wait_for(lambda: self.waiter_files() == []), "waiter file left behind")
        locks = [json.loads(f.read_text())["pid"] for f in self.lock_files()]
        self.assertIn(a2.pid, locks)
        self.assertNotIn(a1.pid, locks)
        for t in ("a2", "b1", "n1"):
            self.stop(t)
        for p in (a2, b1, n1):
            self.assertEqual(self.finish(p)[0], 0)
        self.assertTrue(wait_for(lambda: not self.lock_files()), "lock files left behind")

    # 30
    def test_waiters_served_by_effective_priority(self) -> None:
        h = self.broker("h", self.repo_a)
        self.assert_started("h")
        procs = []
        for i, (tag, prio) in enumerate([("x", 1), ("y", 2), ("z", 0), ("w", 1)], start=1):
            procs.append(self.broker(tag, self.repo_a, prio=prio))
            self.waiters(i)            # one arrival at a time: distinct epoch_ns
            time.sleep(0.05)
        names = sorted(f.name for f in self.waiter_files())
        self.assertEqual([n.split("-")[0] for n in names], ["0", "1", "1", "2"], names)
        # Lock released: best priority first, FIFO inside one priority: z, x, w, y.
        self.stop("h")
        seen: list[str] = []
        for expect in ("z", "x", "w", "y"):
            self.assertTrue(wait_for(lambda e=expect: self.started(e)), f"{expect} did not start")
            seen = (self.d / "order.txt").read_text().split()
            self.assertEqual(seen[-1], expect, f"lock order was {seen}")
            self.stop(expect)
        self.assertEqual(seen, ["h", "z", "x", "w", "y"])
        for p in procs + [h]:
            self.assertEqual(self.finish(p)[0], 0)

    # 31
    def test_priority_inheritance_lifts_holder(self) -> None:
        cap = 2
        # B (prio 1) spends the whole machine budget (cargo test = weight 2).
        blocker = self.broker("blocker", self.repo_b, sub="test", capacity=cap)
        self.assert_started("blocker")
        # S (prio 2) takes the lock of worktree A, then waits in the machine queue for a token.
        s = self.broker("s", self.repo_a, sub="test", prio=2, capacity=cap)
        self.assertTrue(wait_for(lambda: len(self.queue_files()) == 1), "S never queued")
        self.assertTrue(wait_for(lambda: any(
            json.loads(f.read_text())["pid"] == s.pid for f in self.lock_files())),
            "S does not hold the lock while it queues")
        # Q (prio 1, worktree C) queues behind S in arrival order but ahead of it by priority.
        q = self.broker("q", self.repo_c, sub="test", prio=1, capacity=cap)
        self.assertTrue(wait_for(lambda: len(self.queue_files()) == 2), "Q never queued")
        # G (prio 0, worktree A) waits for S's lock: S inherits prio 0 and outranks Q.
        g = self.broker("g", self.repo_a, sub="test", prio=0, capacity=cap)
        self.waiters(1)
        time.sleep(0.5)               # let an admission cycle see the waiter
        self.assertFalse(self.started("s") or self.started("q") or self.started("g"))
        self.stop("blocker")
        self.assertEqual(self.finish(blocker)[0], 0)
        self.assert_started("s")
        self.assertFalse(self.started("q"), "Q went ahead of a lock holder lifted to prio 0")
        self.stop("s")
        self.assertEqual(self.finish(s)[0], 0)
        # G now owns the lock and its own prio 0 still beats Q.
        self.assert_started("g")
        self.assertFalse(self.started("q"))
        self.stop("g")
        self.assertEqual(self.finish(g)[0], 0)
        self.assert_started("q")
        self.stop("q")
        self.assertEqual(self.finish(q)[0], 0)
        order = (self.d / "order.txt").read_text().split()
        self.assertEqual(order, ["blocker", "s", "g", "q"])

    # 32
    def test_lock_before_token_no_deadlock(self) -> None:
        cap = 1
        h = self.broker("h", self.repo_a, capacity=cap)
        self.assert_started("h")
        # W waits for the worktree lock holding NO token and no machine-queue place.
        w = self.broker("w", self.repo_a, capacity=cap)
        self.waiters(1)
        time.sleep(0.5)
        self.assertEqual([f.name for f in self.held_files()], [str(h.pid)])
        self.assertEqual(self.queue_files(), [], "a lock waiter must not sit in the token queue")
        # R (another worktree) queues for the only token.
        r = self.broker("r", self.repo_b, capacity=cap)
        self.assertTrue(wait_for(lambda: len(self.queue_files()) == 1))
        self.assertFalse(self.started("w") or self.started("r"))
        # When H goes, both finish: no cycle, nobody holds one resource while wanting the other.
        self.stop("h")
        self.assertEqual(self.finish(h)[0], 0)
        for tag, p in (("w", w), ("r", r)):
            self.assert_started(tag)
            self.assertEqual(len(self.held_files()), 1)
            self.stop(tag)
            self.assertEqual(self.finish(p)[0], 0)
        self.assertTrue(wait_for(lambda: not self.held_files() and not self.lock_files()))

    # 33
    def test_hold_worktree_and_nesting(self) -> None:
        # No --prio: the held session runs at prio 0. It takes the lock and NO token.
        hw = self.hold("hw", self.repo_a, nested=True)
        self.assert_started("hw")
        self.assertEqual(self.read("hw.held_env"), str(hw.pid))
        self.assertEqual(self.read("hw.prio_env"), "0")
        self.assertEqual(self.read("hw.token_env"), "", "hold-worktree takes no token")
        self.assertEqual(self.read("hw.held_files"), "")
        locks = self.lock_files()
        self.assertEqual(len(locks), 1)
        self.assertEqual(json.loads(locks[0].read_text())["pid"], hw.pid)

        # A step inside the session does not retake the worktree lock (it would deadlock on
        # its own parent) but still asks its own token, step by step.
        self.assertTrue(wait_for(lambda: (self.d / "hw.inner_rc").exists(), 40), "nested call hung")
        self.assertEqual(self.read("hw.inner_started"), "1")
        self.assertEqual(self.read("hw.inner_rc"), "0")
        inner_token = self.read("inner.token")
        self.assertTrue(inner_token and inner_token != str(hw.pid), inner_token)
        self.assertEqual(self.read("hw.inner_held_files"), inner_token)

        # Outsiders in the same worktree stay out for the whole session; other trees do not.
        out_a = self.broker("out-a", self.repo_a)
        self.waiters(1)
        self.assert_stays_waiting("out-a")
        out_b = self.broker("out-b", self.repo_b)
        self.assert_started("out-b")
        self.stop("out-b")
        self.assertEqual(self.finish(out_b)[0], 0)

        # Release on exit.
        self.stop("hw")
        self.assertEqual(self.finish(hw)[0], 0)
        self.assert_started("out-a")
        self.stop("out-a")
        self.assertEqual(self.finish(out_a)[0], 0)
        self.assertTrue(wait_for(lambda: not self.lock_files()), "lock left behind")

        # --prio sets the inherited priority, and the child's exit code comes back.
        hp = self.hold("hp", self.repo_a, prio=2, HOLDER_EXIT="7")
        self.assert_started("hp")
        self.assertEqual(self.read("hp.prio_env"), "2")
        self.stop("hp")
        self.assertEqual(self.finish(hp)[0], 7)
        self.assertTrue(wait_for(lambda: not self.lock_files()), "lock left behind")

    def test_an_agent_behind_a_gate_hold_fails_fast(self) -> None:
        # A gate (`hold-worktree`) keeps its worktree for its whole run, 30-50 min on this
        # repo; an agent's call is capped at 540 s, so waiting behind one can only end in exit
        # 75 after nine idle minutes (52 times in four days, measured 2026-10-06). It is told
        # at once, and told who holds the tree, instead of being told to try again.
        hw = self.hold("hw", self.repo_a)
        self.assert_started("hw")
        t0 = time.time()
        ag = self.broker("ag", self.repo_a, agent="main", wait_max="30")
        rc, _, err = self.finish(ag)
        self.assertEqual(rc, 75, err)
        self.assertLess(time.time() - t0, 10, "the agent waited behind the gate")
        self.assertFalse(self.started("ag"))
        self.assertIn(f"pid {hw.pid}", err)
        self.assertIn("gate", err)
        self.assertNotIn("try again", err)

        # Without an agent (a person, the daemon) the old behaviour stays: it waits its turn.
        out = self.broker("out", self.repo_a)
        self.waiters(1)
        self.assert_stays_waiting("out")
        self.stop("hw")
        self.assertEqual(self.finish(hw)[0], 0)
        self.assert_started("out")
        self.stop("out")
        self.assertEqual(self.finish(out)[0], 0)

    def seed_log(self, argv: list[str], runs: int = 3, run_s: float = 600.0) -> None:
        """Previous successful runs of `argv`, as the broker would have logged them."""
        self.state.mkdir(parents=True, exist_ok=True)
        with open(self.state / "log.jsonl", "a", encoding="utf-8") as f:
            for _ in range(runs):
                f.write(json.dumps({"v": 1, "argv": argv, "run_s": run_s, "exit": 0}) + "\n")

    def test_an_agent_behind_a_long_holder_fails_fast_with_an_estimate(self) -> None:
        # The log says this command takes ten minutes; an agent that can wait 30 s behind it
        # is told at once (and told how long is left) rather than idling to a timeout.
        self.seed_log([str(self.cargo), "check", "h"])
        h = self.broker("h", self.repo_a, agent="other")
        self.assert_started("h")
        t0 = time.time()
        ag = self.broker("ag", self.repo_a, agent="main", wait_max="30")
        rc, _, err = self.finish(ag)
        self.assertEqual(rc, 75, err)
        self.assertLess(time.time() - t0, 10, "the agent waited behind a long holder")
        self.assertFalse(self.started("ag"))
        self.assertIn(f"pid {h.pid}", err)
        self.assertIn("agent other", err)
        self.assertIn("check h", err)
        self.assertIn("estimated", err)

        # Without an agent (a person, the daemon) the old behaviour stays: it waits its turn.
        out = self.broker("out", self.repo_a)
        self.waiters(1)
        self.assert_stays_waiting("out")
        self.stop("h")
        self.assertEqual(self.finish(h)[0], 0)
        self.assert_started("out")
        self.stop("out")
        self.assertEqual(self.finish(out)[0], 0)

    def test_a_lock_timeout_names_the_holder(self) -> None:
        # No history of the holder's command, so there is no estimate and no early exit: the
        # agent waits out its cap, and the timeout says who it was waiting for.
        h = self.broker("h", self.repo_a)
        self.assert_started("h")
        t0 = time.time()
        ag = self.broker("ag", self.repo_a, agent="main", wait_max="2")
        rc, _, err = self.finish(ag)
        self.assertEqual(rc, 75, err)
        self.assertGreaterEqual(time.time() - t0, 1.5, "gave up before its wait cap")
        self.assertFalse(self.started("ag"))
        self.assertIn(f"pid {h.pid}", err)
        self.assertIn("check h", err)

    def test_the_gate_message_names_the_holder(self) -> None:
        hw = self.hold("hw", self.repo_a)
        self.assert_started("hw")
        ag = self.broker("ag", self.repo_a, agent="main", wait_max="30")
        rc, _, err = self.finish(ag)
        self.assertEqual(rc, 75, err)
        self.assertFalse(self.started("ag"))
        self.assertIn("gate", err)
        # One holder line, the same shape everywhere: who, which agent, what, for how long.
        self.assertRegex(err, rf"pid {hw.pid}, agent \S+, `[^`]+`, running \d+s")

    def waiter_dirs(self) -> list[Path]:
        wt = self.state / "wt"
        return sorted(wt.glob("*.waiters")) if wt.exists() else []

    def test_last_waiter_leaving_removes_the_waiters_dir(self) -> None:
        # The broker used to leave an empty `wt/<hash>.waiters` behind when its last waiter
        # went: the dir has to follow the waiter out, not outlive the contention.
        a1 = self.broker("a1", self.repo_a)
        self.assert_started("a1")
        a2 = self.broker("a2", self.repo_a)
        self.waiters(1)
        self.stop("a1")
        self.assert_started("a2")
        self.stop("a2")
        self.assertEqual(self.finish(a1)[0], 0)
        self.assertEqual(self.finish(a2)[0], 0)
        self.assertTrue(
            wait_for(lambda: self.waiter_dirs() == [], timeout=10), "waiters dir left behind"
        )

    def test_an_uncontended_lock_leaves_no_waiters_dir(self) -> None:
        # Nobody ever waits, and the dir used to be made on every poll all the same.
        a1 = self.broker("a1", self.repo_a)
        self.assert_started("a1")
        self.stop("a1")
        self.assertEqual(self.finish(a1)[0], 0)
        self.assertEqual(self.waiter_dirs(), [])

    def test_entries_of_a_vanished_dir_is_empty(self) -> None:
        # The broker removes empty `.waiters` dirs; a reader outside the Mutex can see
        # `is_dir()` pass and then lose the dir before `iterdir()`.
        import importlib.util
        import pathlib
        from unittest import mock

        spec = importlib.util.spec_from_file_location("heavy_entries_under_test", HEAVY)
        heavy = importlib.util.module_from_spec(spec)
        sys.modules["heavy_entries_under_test"] = heavy
        self.addCleanup(sys.modules.pop, "heavy_entries_under_test", None)
        spec.loader.exec_module(heavy)

        gone = self.tmp / "gone.waiters"
        self.assertEqual(heavy._entries(gone), [])
        with mock.patch.object(pathlib.Path, "is_dir", return_value=True):
            self.assertEqual(heavy._entries(gone), [])


if __name__ == "__main__":
    unittest.main()
