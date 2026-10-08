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
