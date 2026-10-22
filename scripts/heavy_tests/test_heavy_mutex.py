#!/usr/bin/env python3
"""Tests for the broker's state mutex (`scripts/heavy.py` Mutex): a release that cannot
delete `owner` at first try still frees the mutex, and a mutex left behind in this process's
own name is reclaimed by it rather than waited on until the timeout (2026-10-07: six queued
commands fell out of the queue together that way)."""

from __future__ import annotations

import importlib.util
import json
import os
import tempfile
import threading
import time
import unittest
from unittest import mock
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location("heavy", ROOT / "scripts" / "heavy.py")
heavy = importlib.util.module_from_spec(spec)
spec.loader.exec_module(heavy)


class MutexTest(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.dir = Path(self.tmp.name)

    def tearDown(self):
        self.tmp.cleanup()

    def leave_own_mutex(self):
        (self.dir / ".mutex").mkdir()
        _, ctime = heavy.proc_identity(os.getpid())
        (self.dir / ".mutex" / "owner").write_text(json.dumps(
            {"pid": os.getpid(), "ctime": ctime, "site": "acquire_token", "t": time.time()}))

    def test_a_leftover_in_its_own_name_is_reclaimed(self):
        self.leave_own_mutex()
        t0 = time.time()
        with heavy.Mutex(self.dir, timeout=5):
            pass
        self.assertLess(time.time() - t0, 2)
        self.assertFalse((self.dir / ".mutex").exists())

    def test_one_it_holds_is_not_taken_from_itself(self):
        with heavy.Mutex(self.dir):
            with self.assertRaises(TimeoutError):
                heavy.Mutex(self.dir, timeout=0.3).acquire()
            self.assertTrue((self.dir / ".mutex").exists())

    def fake_clock(self, jump_at, jump_s, on_sleep=None):
        """Patch heavy.time so a poll sleep can jump the clock like a frozen machine does.
        Returns the sleep counter; no real sleeping happens."""
        real_time = time.time
        offset = [0.0]
        calls = [0]

        def fake_time():
            return real_time() + offset[0]

        def fake_sleep(_d):
            calls[0] += 1
            if calls[0] == jump_at:
                offset[0] += jump_s
            if on_sleep:
                on_sleep(calls[0])

        for name, fn in (("time", fake_time), ("sleep", fake_sleep)):
            p = mock.patch.object(heavy.time, name, fn)
            p.start()
            self.addCleanup(p.stop)
        return calls

    def test_a_stall_past_the_deadline_does_not_time_out_before_the_minimum_attempts(self):
        holder = heavy.Mutex(self.dir)
        holder.acquire()
        waiter = heavy.Mutex(self.dir, timeout=1.0)
        minimum = int(1.0 / heavy.MUTEX_POLL_S * heavy.MUTEX_MIN_ATTEMPTS_FRAC)
        released = []

        def release_late(n):
            if n == 3:  # shortly after the jump, well before `minimum` attempts
                holder.release()
                released.append(n)

        # 2.0 s jump: past the 1.0 s deadline, under the 4.0 s hard cap (1.0 x MUTEX_WAIT_CAP_X).
        self.fake_clock(jump_at=1, jump_s=2.0, on_sleep=release_late)
        waiter.acquire()  # must not raise although the deadline passed on the first poll
        self.addCleanup(waiter.release)
        self.assertTrue(waiter.held)
        self.assertEqual(released, [3])
        # The old wall-clock rule would have raised here: the deadline was long past.
        self.assertLess(waiter.n_exists + waiter.n_perm, minimum)

    def test_genuine_contention_still_raises_mutex_busy(self):
        minimum = int(0.4 / heavy.MUTEX_POLL_S * heavy.MUTEX_MIN_ATTEMPTS_FRAC)
        with heavy.Mutex(self.dir):
            waiter = heavy.Mutex(self.dir, timeout=0.4)
            with self.assertRaises(heavy.MutexBusy) as cm:
                waiter.acquire()
            self.assertIsInstance(cm.exception, TimeoutError)
            self.assertGreaterEqual(waiter.n_exists + waiter.n_perm, minimum)

    def test_the_hard_cap_raises_even_with_few_attempts(self):
        with heavy.Mutex(self.dir):
            waiter = heavy.Mutex(self.dir, timeout=10.0)  # 100 attempts minimum, cap 40 s
            self.fake_clock(jump_at=2, jump_s=45.0)
            with self.assertRaises(heavy.MutexBusy):
                waiter.acquire()
            self.assertLess(waiter.n_exists + waiter.n_perm, 100)

    @unittest.skipUnless(os.name == "nt", "only Windows refuses to delete an open file")
    def test_release_waits_out_a_reader_holding_owner(self):
        m = heavy.Mutex(self.dir)
        m.acquire()
        reader = open(self.dir / ".mutex" / "owner", encoding="utf-8")
        threading.Timer(0.2, reader.close).start()
        t0 = time.time()
        m.release()
        self.assertFalse((self.dir / ".mutex").exists())
        # Freed once the reader let go, not by the last-resort rmtree a second later: that
        # one is a single attempt, and another reader at that moment leaves the mutex behind.
        self.assertLess(time.time() - t0, 0.8)


if __name__ == "__main__":
    unittest.main()
