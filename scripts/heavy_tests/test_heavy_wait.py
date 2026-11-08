#!/usr/bin/env python3
"""Lock-free waiting in the heavy broker (scripts/heavy_wait.py and its use in heavy.py):
waiters read queue and holder state without the Mutex, take it only to write, and the Mutex
holder runs boosted and reports per-step timings in its slow trace row."""

from __future__ import annotations

import importlib.util
import json
import os
import subprocess
import sys
import tempfile
import time
import unittest
from pathlib import Path
from unittest import mock

ROOT = Path(__file__).resolve().parents[2]
SCRIPTS = ROOT / "scripts"
if str(SCRIPTS) not in sys.path:
    sys.path.insert(0, str(SCRIPTS))
spec = importlib.util.spec_from_file_location("heavy", SCRIPTS / "heavy.py")
heavy = importlib.util.module_from_spec(spec)
spec.loader.exec_module(heavy)
import heavy_wait  # noqa: E402

CHILD_PRELUDE = f"""
import importlib.util, json, os, sys, time
from pathlib import Path
spec = importlib.util.spec_from_file_location("heavy", {str(SCRIPTS / "heavy.py")!r})
heavy = importlib.util.module_from_spec(spec); spec.loader.exec_module(heavy)
state = Path(os.environ["NUCLEOS_HEAVY_DIR"])
me = os.getpid()
ctime = heavy.proc_identity(me)[1]
"""

TOKEN_WAITER = CHILD_PRELUDE + """
cap = float(sys.argv[1])
rec = {"pid": me, "ctime": ctime, "weight": 1, "prio": 1, "argv": ["x"]}
t0 = time.time()
ok, _ = heavy.acquire_token(state, rec, cap)
print(json.dumps({"ok": ok, "acq": heavy.MUTEX_ACQUIRES, "dt": time.time() - t0}))
"""

TOKEN_USER = CHILD_PRELUDE + """
rec = {"pid": me, "ctime": ctime, "weight": 1, "prio": 1, "argv": ["x"]}
ok, _ = heavy.acquire_token(state, rec, 60.0)
n = len(list((state / "held").iterdir()))
time.sleep(0.25)
heavy.release_token(state)
print(json.dumps({"ok": ok, "held_seen": n}))
"""

LOCK_WAITER = CHILD_PRELUDE + """
cap = float(sys.argv[1])
rec = {"pid": me, "ctime": ctime, "argv": ["x"], "worktree": "w", "prio": 1}
t0 = time.time()
ok, _, _ = heavy.acquire_lock(state, "abc", rec, cap)
print(json.dumps({"ok": ok, "acq": heavy.MUTEX_ACQUIRES, "dt": time.time() - t0}))
"""

LOCK_USER = CHILD_PRELUDE + """
rec = {"pid": me, "ctime": ctime, "argv": ["x"], "worktree": "w", "prio": 1}
ok, _, _ = heavy.acquire_lock(state, "abc", rec, 60.0)
inside = state / "inside"
clash = False
try:
    inside.mkdir()
except OSError:
    clash = True
time.sleep(0.15)
if not clash:
    inside.rmdir()
heavy.release_lock(state, "abc")
print(json.dumps({"ok": ok, "clash": clash}))
"""


def spawn(script: str, state: Path, *args, poll="0.5") -> subprocess.Popen:
    env = dict(os.environ, NUCLEOS_HEAVY_DIR=str(state), NUCLEOS_HEAVY_POLL_S=poll,
               NUCLEOS_HEAVY_CAPACITY="2", NUCLEOS_HEAVY_LOCAL="1", NUCLEOS_HEAVY_BOOST="0")
    return subprocess.Popen([sys.executable, "-c", script, *map(str, args)], env=env,
                            stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)


def results(procs) -> list[dict]:
    out = []
    for p in procs:
        so, se = p.communicate(timeout=120)
        assert p.returncode == 0, se
        out.append(json.loads(so.strip().splitlines()[-1]))
    return out


class _State(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.state = Path(self.tmp.name)
        self.addCleanup(self.tmp.cleanup)

    def plant_holder(self, weight=2):
        (self.state / "held").mkdir(parents=True, exist_ok=True)
        rec = {"pid": os.getpid(), "ctime": heavy.proc_identity(os.getpid())[1],
               "weight": weight, "prio": 1}
        (self.state / "held" / str(os.getpid())).write_text(json.dumps(rec))


class Contention(_State):
    def test_waiters_barely_touch_the_mutex(self):
        self.plant_holder(weight=2)  # capacity 2: everyone waits behind it
        res = results([spawn(TOKEN_WAITER, self.state, 5.0) for _ in range(8)])
        per_s = sum(r["acq"] for r in res) / sum(r["dt"] for r in res)
        print(f"\nmutex acquisitions per waiter-second: {per_s:.3f} (was ~2.0)")
        self.assertTrue(all(not r["ok"] for r in res))
        self.assertLess(per_s, 0.6)

    def test_lock_waiters_barely_touch_the_mutex(self):
        (self.state / "wt").mkdir()
        (self.state / "wt" / "abc.lock").write_text(json.dumps(
            {"pid": os.getpid(), "ctime": heavy.proc_identity(os.getpid())[1],
             "argv": ["x"], "acquired": time.time()}))
        res = results([spawn(LOCK_WAITER, self.state, 5.0) for _ in range(8)])
        per_s = sum(r["acq"] for r in res) / sum(r["dt"] for r in res)
        print(f"\nlock mutex acquisitions per waiter-second: {per_s:.3f} (was ~2.0)")
        self.assertLess(per_s, 0.6)


class NoDoubleGrant(_State):
    def test_held_weight_never_exceeds_capacity(self):
        res = results([spawn(TOKEN_USER, self.state, poll="0.1") for _ in range(8)])
        self.assertTrue(all(r["ok"] for r in res))
        self.assertLessEqual(max(r["held_seen"] for r in res), 2)

    def test_two_lock_holders_never_coexist(self):
        res = results([spawn(LOCK_USER, self.state, poll="0.1") for _ in range(6)])
        self.assertTrue(all(r["ok"] for r in res))
        self.assertFalse(any(r["clash"] for r in res))


class OpenReaderBlocksUnlink(_State):
    """Windows refuses to unlink a file another handle holds open; lock-free readers can hold
    the queue record open exactly when the granter removes it."""

    def _rec(self):
        return {"pid": os.getpid(), "ctime": heavy.proc_identity(os.getpid())[1],
                "weight": 1, "prio": 1, "argv": ["x"]}

    def _grant_with_reader(self, hold_s, cap=2.0):
        import threading
        handles = []
        real = heavy._queue_order

        def spy(*a, **k):
            if not handles:  # first Mutex-held look: the queue record exists, open it
                f = open(next((self.state / "queue").iterdir()), "rb")
                handles.append(f)
                threading.Timer(hold_s, f.close).start()
            return real(*a, **k)

        with mock.patch.object(heavy, "_queue_order", spy),                 mock.patch.dict(os.environ, {"NUCLEOS_HEAVY_CAPACITY": "2",
                                             "NUCLEOS_HEAVY_POLL_S": "0.05"}):
            ok, _ = heavy.acquire_token(self.state, self._rec(), cap)
        for f in handles:
            f.close()
        return ok, handles

    @unittest.skipUnless(os.name == "nt", "sharing violations are Windows-only")
    def test_a_briefly_open_queue_record_still_grants_once(self):
        ok, handles = self._grant_with_reader(0.05)
        self.assertTrue(ok)
        self.assertEqual(len(list((self.state / "held").iterdir())), 1)
        self.assertEqual(list((self.state / "queue").iterdir()), [])

    @unittest.skipUnless(os.name == "nt", "sharing violations are Windows-only")
    def test_a_stuck_reader_never_leaves_held_and_queue_both(self):
        ok, _ = self._grant_with_reader(30.0, cap=1.5)
        # The grant is refused while the reader stays open; a held record next to a live
        # queue entry would be the double claim.
        self.assertFalse(ok)
        self.assertEqual(list((self.state / "held").iterdir()), [])

    def test_retry_io_retries_sharing_violations_then_gives_up(self):
        calls = []

        def flaky():
            calls.append(1)
            if len(calls) < 3:
                e = PermissionError(13, "in use")
                e.winerror = 32
                raise e
            return "done"

        self.assertEqual(heavy_wait.retry_io(flaky, sleep=lambda _s: None), "done")
        def stuck():
            e = PermissionError(13, "x")
            e.winerror = 32
            raise e

        with self.assertRaises(PermissionError):
            heavy_wait.retry_io(stuck, attempts=3, sleep=lambda _s: None)
        with self.assertRaises(FileNotFoundError):
            heavy_wait.retry_io(lambda: (_ for _ in ()).throw(FileNotFoundError()),
                                sleep=lambda _s: None)

    def test_retry_io_retries_only_windows_sharing_violations(self):
        # A plain permission error (a read-only file, a foreign owner) never clears by waiting.
        for exc in (PermissionError(13, "denied"), PermissionError(1, "not permitted")):
            calls = []

            def denied(exc=exc, calls=calls):
                calls.append(1)
                raise exc

            with self.assertRaises(PermissionError):
                heavy_wait.retry_io(denied, sleep=lambda _s: None)
            if os.name != "nt":
                self.assertEqual(len(calls), 1, "no retry without a winerror off Windows")
        for winerror in (32, 5):
            calls = []

            def shared(winerror=winerror, calls=calls):
                calls.append(1)
                e = PermissionError(13, "in use")
                e.winerror = winerror
                if len(calls) < 3:
                    raise e
                return "ok"

            self.assertEqual(heavy_wait.retry_io(shared, sleep=lambda _s: None), "ok")
        calls = []

        def other_winerror():
            calls.append(1)
            e = PermissionError(13, "x")
            e.winerror = 1314
            raise e

        with self.assertRaises(PermissionError):
            heavy_wait.retry_io(other_winerror, sleep=lambda _s: None)
        self.assertEqual(len(calls), 1)


class Torn(_State):
    def test_torn_lock_is_not_granted_on_a_lock_free_poll(self):
        holder = subprocess.Popen([sys.executable, "-c", "import time; time.sleep(60)"])
        self.addCleanup(holder.kill)
        lock = self.state / "wt" / "abc.lock"
        lock.parent.mkdir()
        lock.write_text(json.dumps({"pid": holder.pid,
                                    "ctime": heavy.proc_identity(holder.pid)[1],
                                    "argv": ["x"], "acquired": time.time()}))
        rec = {"pid": os.getpid(), "ctime": heavy.proc_identity(os.getpid())[1],
               "argv": ["x"], "worktree": "w", "prio": 1}

        class Stop(Exception):
            pass

        sleeps = []

        def fake_sleep(_s):
            sleeps.append(1)
            if len(sleeps) == 1:
                lock.write_text("{")  # a writer caught mid-write
            else:
                raise Stop

        with mock.patch.object(heavy.time, "sleep", fake_sleep), \
                mock.patch.dict(os.environ, {"NUCLEOS_HEAVY_POLL_S": "0.1"}):
            with self.assertRaises(Stop):
                heavy.acquire_lock(self.state, "abc", rec, 30.0)
        self.assertEqual(lock.read_text(), "{", "a torn lock must not be taken over")

    def test_torn_queue_record_does_not_grant_or_crash(self):
        self.plant_holder(weight=2)
        (self.state / "queue").mkdir()
        (self.state / "queue" / "1-1-99999999").write_text("{")
        rec = {"pid": os.getpid(), "ctime": heavy.proc_identity(os.getpid())[1],
               "weight": 1, "prio": 1, "argv": ["x"]}
        with mock.patch.dict(os.environ, {"NUCLEOS_HEAVY_CAPACITY": "2",
                                          "NUCLEOS_HEAVY_POLL_S": "0.05"}):
            ok, _ = heavy.acquire_token(self.state, rec, 0.4)
        self.assertFalse(ok)


class AliveCacheTest(unittest.TestCase):
    def test_answers_expire(self):
        now = [0.0]
        calls = []
        cache = heavy_wait.AliveCache(lambda p, c: calls.append(p) or True, ttl=1.5,
                                      clock=lambda: now[0])
        self.assertTrue(cache.check(7, 1))
        now[0] = 1.0
        cache.check(7, 1)
        self.assertEqual(len(calls), 1, "within the TTL the probe must not run again")
        now[0] = 1.6
        cache.check(7, 1)
        self.assertEqual(len(calls), 2, "past the TTL the probe must run again")
        cache.check(7, 2)  # another creation time is another process
        self.assertEqual(len(calls), 3)


class BackoffTest(unittest.TestCase):
    def test_grows_jittered_and_resets(self):
        b = heavy_wait.Backoff(0.2, 1.0, rng=lambda: 0.5)  # jitter factor 1.0
        seq = [b.next() for _ in range(6)]
        self.assertAlmostEqual(seq[0], 0.2)
        self.assertAlmostEqual(seq[-1], 1.0)
        self.assertAlmostEqual(b.next(head=True), 0.2)
        b.reset()
        self.assertAlmostEqual(b.next(), 0.2)
        lo = heavy_wait.Backoff(1.0, 1.0, rng=lambda: 0.0).next()
        hi = heavy_wait.Backoff(1.0, 1.0, rng=lambda: 1.0).next()
        self.assertAlmostEqual(lo, 0.8)
        self.assertAlmostEqual(hi, 1.2)


class Boost(_State):
    def test_boost_is_applied_and_restored(self):
        calls = []
        with mock.patch.object(heavy_wait, "_IS_WIN", True), \
                mock.patch.object(heavy_wait, "_get_priority", lambda: heavy_wait.NORMAL), \
                mock.patch.object(heavy_wait, "_set_priority",
                                  lambda c: calls.append(c) or True), \
                mock.patch.dict(os.environ, {"NUCLEOS_HEAVY_BOOST": "1"}), \
                mock.patch.object(heavy, "MUTEX_TRACE_S", 0.0):
            with heavy.Mutex(self.state) as mx:
                self.assertEqual(calls, [heavy_wait.ABOVE_NORMAL])
                self.assertTrue(mx.boosted)
        self.assertEqual(calls, [heavy_wait.ABOVE_NORMAL, heavy_wait.NORMAL])
        row = json.loads((self.state / "mutex.jsonl").read_text().splitlines()[-1])
        self.assertTrue(row["boosted"])

    def test_failure_never_raises_and_non_windows_is_a_no_op(self):
        def boom(*_):
            raise OSError("no")

        with mock.patch.object(heavy_wait, "_IS_WIN", True), \
                mock.patch.object(heavy_wait, "_get_priority", boom), \
                mock.patch.dict(os.environ, {"NUCLEOS_HEAVY_BOOST": "1"}):
            self.assertIsNone(heavy_wait.boost())
        with mock.patch.object(heavy_wait, "_IS_WIN", False):
            self.assertIsNone(heavy_wait.boost())
        heavy_wait.restore(None)


class SlowRowSteps(_State):
    def test_slow_row_carries_steps(self):
        rec = {"pid": os.getpid(), "ctime": heavy.proc_identity(os.getpid())[1],
               "weight": 1, "prio": 1, "argv": ["x"]}
        with mock.patch.object(heavy, "MUTEX_TRACE_S", 0.0), \
                mock.patch.dict(os.environ, {"NUCLEOS_HEAVY_CAPACITY": "2",
                                             "NUCLEOS_HEAVY_POLL_S": "0.05"}):
            ok, _ = heavy.acquire_token(self.state, rec, 5.0)
            heavy.release_token(self.state)
        self.assertTrue(ok)
        rows = [json.loads(ln)
                for ln in (self.state / "mutex.jsonl").read_text().splitlines()]
        steps = [r["steps"] for r in rows if r.get("event") == "slow" and "steps" in r]
        self.assertTrue(steps)
        self.assertTrue(any({"entries", "order", "decision", "write"} <= set(s)
                            for s in steps), steps)
        self.assertTrue(all("boosted" in r for r in rows if r.get("event") == "slow"))


class WaiterEdges(_State):
    def _rec(self):
        return {"pid": os.getpid(), "ctime": heavy.proc_identity(os.getpid())[1],
                "weight": 1, "prio": 1, "argv": ["x"]}

    def plant_other_holder(self, weight=2):
        p = subprocess.Popen([sys.executable, "-c", "import time; time.sleep(60)"])
        self.addCleanup(p.kill)
        (self.state / "held").mkdir(parents=True, exist_ok=True)
        (self.state / "held" / str(p.pid)).write_text(json.dumps(
            {"pid": p.pid, "ctime": heavy.proc_identity(p.pid)[1], "weight": weight, "prio": 1}))

    def test_corrupt_own_queue_file_is_rewritten_after_three_polls(self):
        self.plant_other_holder(weight=2)  # capacity 2: the waiter stays queued
        qfile = self.state / "queue" / f"1-5-{os.getpid()}"
        import threading

        def corrupt():
            end = time.time() + 5
            while time.time() < end and not qfile.exists():
                time.sleep(0.01)
            qfile.write_text("{not json", encoding="utf-8")

        def valid():
            try:
                return json.loads(qfile.read_text(encoding="utf-8")).get("pid") == os.getpid()
            except (OSError, ValueError):
                return False

        seen = []

        def watch():
            end = time.time() + 7
            while time.time() < end:
                if valid() and seen:
                    seen.append("rewritten")
                    return
                if not valid() and qfile.exists() and not seen:
                    seen.append("corrupt")
                time.sleep(0.02)

        for fn in (corrupt, watch):
            threading.Thread(target=fn, daemon=True).start()
        with mock.patch.dict(os.environ, {"NUCLEOS_HEAVY_CAPACITY": "2",
                                          "NUCLEOS_HEAVY_POLL_S": "0.05"}):
            ok, _ = heavy.acquire_token(self.state, self._rec(), 6.0, 5)
        self.assertFalse(ok)
        self.assertIn("rewritten", seen, "the corrupt queue file was never put back")

    def test_grant_landing_as_the_waiter_leaves_is_not_lost(self):
        self.plant_other_holder(weight=2)
        qname = f"1-5-{os.getpid()}"
        real = heavy.Mutex
        t_start = time.time()
        rec = self._rec()

        class Late:
            """After the cap, the leave block finds the Mutex busy, and the holder's batch
            grant has just landed: my held record exists, my queue entry is gone."""

            def __init__(self, d):
                self.real, self.d = real(d), d

            def __enter__(self):
                if time.time() - t_start >= 0.5:
                    q = self.d / "queue" / qname
                    (self.d / "held" / str(os.getpid())).write_text(
                        json.dumps({**rec, "grant": qname}), encoding="utf-8")
                    q.unlink()
                    raise heavy.MutexBusy("busy")
                return self.real.__enter__()

            def __exit__(self, *a):
                return self.real.__exit__(*a)

            def __getattr__(self, n):
                return getattr(self.real, n)

        with mock.patch.object(heavy, "Mutex", Late), mock.patch.dict(
                os.environ, {"NUCLEOS_HEAVY_CAPACITY": "2", "NUCLEOS_HEAVY_POLL_S": "0.05"}):
            ok, _ = heavy.acquire_token(self.state, rec, 0.5, 5)
        self.assertTrue(ok, "a grant that landed as the waiter left makes it admitted")
        self.assertEqual(list((self.state / "queue").iterdir()), [])
        self.assertTrue((self.state / "held" / str(os.getpid())).exists())
        heavy.release_token(self.state)  # the caller's normal release frees that record
        self.assertFalse((self.state / "held" / str(os.getpid())).exists())


if __name__ == "__main__":
    unittest.main()
