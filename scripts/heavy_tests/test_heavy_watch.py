#!/usr/bin/env python3
"""RED tests for the heavy broker's live view (`scripts/heavy_watch.py`): worktree-lock
holders and waiters, the wait reason of every queued job, WARM/prio badges, health counted
from the broker's logs, retries of one argv, stable pid labels, a frame that keeps what it
must when the queue is tall, an error footer instead of a crash, and above all a view that
never writes (a dead holder's lock stays where it is and is shown as an orphan).

Hermetic: the state lives in a temp NUCLEOS_HEAVY_DIR holding hand-written files, and the
module under test is loaded by path. The current process is the "alive" pid; a python child
that was spawned and reaped is the "dead" one. Nothing is compiled and no broker runs.
Run all, or one with `-k name`.
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

ROOT = Path(__file__).resolve().parents[2]
WATCH = ROOT / "scripts" / "heavy_watch.py"


def load_watch():
    # `heavy_watch` puts scripts/ on sys.path and imports `heavy` itself; the state dir is
    # read at call time, so loading under a scratch NUCLEOS_HEAVY_DIR is only hygiene.
    scratch = tempfile.mkdtemp(prefix="heavy-w-load-")
    try:
        with mock.patch.dict(os.environ, {"NUCLEOS_HEAVY_DIR": scratch}):
            spec = importlib.util.spec_from_file_location("heavy_watch_under_test", WATCH)
            mod = importlib.util.module_from_spec(spec)
            spec.loader.exec_module(mod)
            return mod
    finally:
        shutil.rmtree(scratch, True)


W = load_watch()
heavy = W.heavy


def stamp(epoch: float) -> str:
    """The broker's own timestamp format, in local time with the offset."""
    return time.strftime("%Y-%m-%dT%H:%M:%S%z", time.localtime(epoch))


def spawn_and_reap_pid() -> int:
    p = subprocess.Popen([sys.executable, "-c", "pass"])
    p.wait()
    return p.pid


class WatchTests(unittest.TestCase):
    def setUp(self) -> None:
        self.tmp = Path(tempfile.mkdtemp(prefix="heavy-w-"))
        self.addCleanup(shutil.rmtree, self.tmp, True)
        self.dir = self.tmp / "state"
        for sub in ("wt", "queue", "held"):
            (self.dir / sub).mkdir(parents=True)
        env = mock.patch.dict(os.environ, {
            "NUCLEOS_HEAVY_DIR": str(self.dir),
            "NUCLEOS_HEAVY_CAPACITY": "4",
            "NUCLEOS_HEAVY_TARGET_SLOTS": "3",
        })
        env.start()
        self.addCleanup(env.stop)
        self.dead = spawn_and_reap_pid()
        self.assertFalse(heavy.is_alive(self.dead), "the 'dead' pid is still alive")
        self.assertTrue(heavy.is_alive(os.getpid()))

    # ---- fixtures ------------------------------------------------------------------
    def live(self, **kw) -> dict:
        _, ctime = heavy.proc_identity(os.getpid())
        rec = {"pid": os.getpid(), "ctime": ctime, "weight": 1, "prio": 1,
               "worktree": str(self.tmp), "argv": ["cargo", "test", "-p", "x"]}
        rec.update(kw)
        return rec

    def corpse(self, **kw) -> dict:
        rec = {"pid": self.dead, "ctime": None, "weight": 1, "prio": 1,
               "worktree": str(self.tmp), "argv": ["cargo", "clippy"]}
        rec.update(kw)
        return rec

    def write_lock(self, name: str, rec: dict) -> Path:
        p = self.dir / "wt" / f"{name}.lock"
        p.write_text(json.dumps(rec), encoding="utf-8")
        return p

    def write_waiter(self, name: str, prio: int, rec: dict, age: float = 0.0,
                     ns: int | None = None) -> Path:
        wdir = self.dir / "wt" / f"{name}.waiters"
        wdir.mkdir(parents=True, exist_ok=True)
        p = wdir / f"{prio}-{ns if ns is not None else time.time_ns()}-{os.getpid()}"
        p.write_text(json.dumps(rec), encoding="utf-8")
        if age:
            t = time.time() - age
            os.utime(p, (t, t))
        return p

    # ---- locks ---------------------------------------------------------------------
    def test_worktree_locks_lists_holder_and_waiters(self) -> None:
        self.write_lock("h", self.live(acquired=time.time() - 100))
        base = time.time_ns()
        self.write_waiter("h", 2, self.live(prio=2), ns=base + 2)
        self.write_waiter("h", 0, self.live(prio=0), age=30, ns=base + 1)
        locks = W.worktree_locks(self.dir)
        self.assertEqual(len(locks), 1, locks)
        lock = locks[0]
        self.assertTrue(lock["alive"])
        self.assertAlmostEqual(lock["age"], 100, delta=10)
        self.assertEqual(lock["rec"]["pid"], os.getpid())
        self.assertEqual([w[0] for w in lock["waiters"]], [0, 2])
        self.assertAlmostEqual(lock["waiters"][0][1], 30, delta=10)

    def test_exit75_countdown_only_near_the_cap(self) -> None:
        self.assertIsNone(W.exit75_in(100, 540))
        self.assertEqual(W.exit75_in(500, 540), 40)
        self.assertEqual(W.exit75_in(600, 540), 0)
        # The cap is an assumption: agents wait up to 540 s, other callers up to 3600 s.
        self.assertEqual(W.waiter_cap({"agent": "main"}), heavy.AGENT_WAIT_MAX_S)
        self.assertEqual(W.waiter_cap({}), heavy.CALLER_WAIT_MAX_S)

    # ---- queue ---------------------------------------------------------------------
    def test_queue_reason_names_the_token_shortfall(self) -> None:
        head = W.queue_reason(1, {"weight": 2}, 3, 4)
        self.assertIn("1 weight must be freed", head)
        back = W.queue_reason(3, {"weight": 1}, 4, 4)
        self.assertIn("2 ahead", back)
        self.assertIn("free 0/4", back)
        self.assertIn("admitting", W.queue_reason(1, {"weight": 1}, 0, 4))

    def test_badges_mark_warm_and_prio(self) -> None:
        warm = W.badges({"agent": "warm", "prio": 3})
        self.assertIn("WARM", warm)
        self.assertIn("prio 3", warm)
        self.assertIn("WARM", W.badges({"warm": True, "prio": 1}))
        self.assertIn("GATE", W.badges({"hold": True, "prio": 0}))
        plain = W.badges({"prio": 1})
        self.assertIn("prio 1", plain)
        self.assertNotIn("WARM", plain)
        self.assertNotIn("GATE", plain)

    # ---- health --------------------------------------------------------------------
    def test_health_counts_exits_and_mutex_events(self) -> None:
        now = time.time()
        rows = [
            {"ts": stamp(now - 60), "exit": 75, "wait_token_s": 0.0},
            {"ts": stamp(now - 3600), "exit": 101},
            {"ts": stamp(now - 120), "exit": 0, "wait_token_s": 4},
            {"ts": stamp(now - 180), "exit": 0, "wait_token_s": 8},
            {"ts": stamp(now - 4 * 3600), "exit": 75},          # outside the 3 h window
        ]
        (self.dir / "mutex.jsonl").write_text(
            json.dumps({"ts": stamp(now - 30), "event": "slow", "waited_s": 2.5}) + "\n"
            + "this line is not json\n"
            + json.dumps({"ts": stamp(now - 10), "event": "timeout"}) + "\n",
            encoding="utf-8",
        )
        mutex_rows = W.read_mutex_tail(self.dir)
        self.assertEqual(len(mutex_rows), 2, mutex_rows)
        h = W.health(rows, mutex_rows, now)
        self.assertEqual(h["exit75"], 1)
        self.assertEqual(h["exit101"], 1)
        self.assertEqual(h["wait_token_med"], 6)
        self.assertEqual(h["mutex_slow"], 1)
        self.assertEqual(h["mutex_timeout"], 1)
        self.assertAlmostEqual(W.ts_epoch(stamp(now - 60)), now - 60, delta=2)
        self.assertIsNone(W.ts_epoch("not a timestamp"))

    def test_retries_counts_recent_exit75_of_same_argv(self) -> None:
        now = time.time()
        argv = ["cargo", "test", "-p", "x"]
        rec = {"argv": argv}
        rows = [
            {"ts": stamp(now - 60), "exit": 75, "argv": argv},
            {"ts": stamp(now - 600), "exit": 75, "argv": argv},
            {"ts": stamp(now - 120), "exit": 75, "argv": ["cargo", "clippy"]},   # other argv
            {"ts": stamp(now - 2 * 3600), "exit": 75, "argv": argv},            # too old
            {"ts": stamp(now - 90), "exit": 0, "argv": argv},                   # it ran
        ]
        self.assertEqual(W.retries(rec, rows, now), 2)

    def test_retries_matches_a_truncated_long_argv(self) -> None:
        now = time.time()
        argv = ["cargo", "test"] + [f"name{i}" for i in range(60)]
        rows = [{"ts": stamp(now - 60), "exit": 75, "argv": W.heavy._log_argv(argv)}]
        self.assertEqual(W.retries({"argv": argv}, rows, now), 1)

    # ---- labels and layout ---------------------------------------------------------
    def test_label_is_stable_per_pid(self) -> None:
        self.assertEqual(W.label(123456), "#3456")
        self.assertEqual(W.label(7), "#7")
        self.assertEqual(W.label(None), "#?")
        self.assertEqual(W.colour_of(123456), W.colour_of(123456))
        self.assertIn(W.colour_of(123456), W.JOB_COLOURS)
        self.assertIn("#3456", W.tag(123456))

    def test_short_argv_and_clip(self) -> None:
        out = W.short_argv({"argv": [r"C:\x\Python311\python.exe", "a.py"]})
        self.assertTrue(out.startswith("python.exe"), out)
        self.assertIn("a.py", out)
        self.assertEqual(W.visible_len(W.GREEN + "abc" + W.RESET), 3)
        clipped = W.clip(W.GREEN + "x" * 50 + W.RESET, 10)
        self.assertEqual(W.visible_len(clipped), 10)
        self.assertTrue(clipped.endswith(W.RESET), repr(clipped))
        self.assertEqual(W.clip("abc", 10), "abc")

    # ---- frame ---------------------------------------------------------------------
    def test_frame_keeps_a_dead_lock_and_shows_it_orphaned(self) -> None:
        lock = self.write_lock("dead", self.corpse())
        text = W.frame({})
        self.assertTrue(lock.exists(), "the view deleted a dead holder's lock")
        self.assertIn("ORPHAN LOCKS", text)

    def test_alerts_survive_a_tall_queue(self) -> None:
        base = time.time_ns()
        for i in range(40):
            f = self.dir / "queue" / f"1-{base + i}-{os.getpid()}"
            f.write_text(json.dumps(self.live(prio=1)), encoding="utf-8")
        self.write_lock("h", self.live(acquired=time.time() - 600))
        # An agent has waited 500 s of its assumed 540 s: it is about to give up with 75.
        self.write_waiter("h", 1, self.live(agent="main", prio=1), age=500)
        out = W.fit(W.frame({}), 40)
        text = "\n".join(out)
        self.assertIn("ALERTS", "\n".join(out[:6]))
        self.assertIn("exit 75", text)
        self.assertIn("TARGET SLOTS", text)
        self.assertIn("QUEUED (40)", text)

    def test_render_turns_a_frame_error_into_a_footer(self) -> None:
        with mock.patch.object(W, "frame", side_effect=RuntimeError("boom")):
            out = W.render({}, 80, 10)
        self.assertIn("frame error: RuntimeError: boom", out[-1])
        self.assertTrue(all(W.visible_len(line) <= 80 for line in out), out)


if __name__ == "__main__":
    unittest.main()
