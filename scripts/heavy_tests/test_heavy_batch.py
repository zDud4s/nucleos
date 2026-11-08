#!/usr/bin/env python3
"""Batch admission and the shared probe of `scripts/heavy.py`.

The Mutex holder that admits the head also admits the entries behind it, in queue order, while
they fit; each of them sees its own held record on its next lock-free poll. The dynamic probe
is shared through `probe.json`, so a waiter that has just become head does not sample again.

    python -m pytest -q scripts/heavy_tests/test_heavy_batch.py
"""

from __future__ import annotations

import importlib.util
import json
import os
import shutil
import subprocess
import sys
import tempfile
import threading
import time
import unittest
from pathlib import Path
from unittest import mock

ROOT = Path(__file__).resolve().parents[2]
HEAVY = ROOT / "scripts" / "heavy.py"

ENV_KEYS = ("NUCLEOS_HEAVY_CAPACITY", "NUCLEOS_HEAVY_ADMISSION", "NUCLEOS_HEAVY_FAKE_PROBE",
            "NUCLEOS_HEAVY_FAKE_DISK", "NUCLEOS_HEAVY_DYNAMIC_MAX_JOBS",
            "NUCLEOS_HEAVY_SETTLE_S", "NUCLEOS_HEAVY_BATCH_MAX", "NUCLEOS_HEAVY_PROBE_TTL_S",
            "NUCLEOS_HEAVY_POLL_S")

WAITER = r'''
import importlib.util, json, os, sys, time
from pathlib import Path
spec = importlib.util.spec_from_file_location("hv", sys.argv[1])
hv = importlib.util.module_from_spec(spec); spec.loader.exec_module(hv)
d = Path(os.environ["NUCLEOS_HEAVY_DIR"]); out = Path(sys.argv[2]); tag = sys.argv[3]
arrival = int(sys.argv[4]); hold = float(sys.argv[5])
_, ct = hv.proc_identity(os.getpid())
rec = {"pid": os.getpid(), "ctime": ct, "weight": 1, "prio": 1, "agent": tag,
       "worktree": "w", "argv": ["x"]}
ok, waited = hv.acquire_token(d, rec, 120, arrival)
used = sum(int(r.get("weight") or 1) for _, r in hv._entries(d / "held", reap=False))
(out / (tag + ".got")).write_text(json.dumps({"ok": ok, "used": used, "waited": waited,
                                              "mutex": hv.MUTEX_ACQUIRES}))
time.sleep(hold)
hv.release_token(d)
'''


def load_heavy():
    spec = importlib.util.spec_from_file_location("heavy_batch_under_test", HEAVY)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


def wait_for(pred, timeout: float = 60.0, step: float = 0.05) -> bool:
    end = time.time() + timeout
    while time.time() < end:
        if pred():
            return True
        time.sleep(step)
    return pred()


class BatchTests(unittest.TestCase):
    def setUp(self) -> None:
        self.hv = load_heavy()
        self.tmp = Path(tempfile.mkdtemp(prefix="heavy-batch-"))
        self.state = self.tmp / "state"
        self.out = self.tmp / "out"
        self.out.mkdir()
        (self.tmp / "waiter.py").write_text(WAITER, encoding="utf-8")
        self.procs: list[subprocess.Popen] = []
        self.addCleanup(self._teardown)
        patcher = mock.patch.dict(os.environ)
        patcher.start()
        self.addCleanup(patcher.stop)
        for k in ENV_KEYS:
            os.environ.pop(k, None)
        os.environ["NUCLEOS_HEAVY_DIR"] = str(self.state)
        os.environ["NUCLEOS_HEAVY_POLL_S"] = "0.1"
        self.queue = self.state / "queue"
        self.held = self.state / "held"

    def _teardown(self) -> None:
        for p in self.procs:
            p.kill()
            p.wait()
        shutil.rmtree(self.tmp, True)

    # ---- helpers -------------------------------------------------------------------
    def sleeper(self) -> subprocess.Popen:
        p = subprocess.Popen([sys.executable, "-c", "import time; time.sleep(300)"])
        self.procs.append(p)
        return p

    def plant(self, p: subprocess.Popen, prio: int, arrival: int, weight: int = 1) -> Path:
        _, ct = self.hv.proc_identity(p.pid)
        self.queue.mkdir(parents=True, exist_ok=True)
        f = self.queue / f"{prio}-{arrival}-{p.pid}"
        f.write_text(json.dumps({"pid": p.pid, "ctime": ct, "weight": weight, "prio": prio,
                                 "agent": "f", "worktree": "w", "argv": ["x"]}),
                     encoding="utf-8")
        return f

    def head(self, prio: int = 0, weight: int = 1) -> dict:
        _, ct = self.hv.proc_identity(os.getpid())
        return {"pid": os.getpid(), "ctime": ct, "weight": weight, "prio": prio,
                "agent": "h", "worktree": "w", "argv": ["x"]}

    def held_pids(self) -> set[int]:
        return {int(p.name) for p in self.held.iterdir()} if self.held.exists() else set()

    def waiter(self, tag: str, arrival: int, hold: float = 0.2) -> subprocess.Popen:
        p = subprocess.Popen([sys.executable, str(self.tmp / "waiter.py"), str(HEAVY),
                              str(self.out), tag, str(arrival), str(hold)])
        self.procs.append(p)
        return p

    # ---- batch grant ---------------------------------------------------------------
    def test_six_waiters_with_room_are_all_admitted_in_one_pass(self) -> None:
        os.environ["NUCLEOS_HEAVY_CAPACITY"] = "8"
        fs = [self.sleeper() for _ in range(5)]
        for i, p in enumerate(fs):
            self.plant(p, 1, 10 + i)
        t = time.time()
        ok, _ = self.hv.acquire_token(self.state, self.head(prio=0), 30, 1)
        took = time.time() - t
        self.assertTrue(ok)
        self.assertEqual(self.held_pids(), {os.getpid(), *(p.pid for p in fs)})
        self.assertEqual(list(self.queue.iterdir()), [], "a queue entry survived its grant")
        self.assertLess(took, 5.0)
        for p in fs:
            rec = json.loads((self.held / str(p.pid)).read_text(encoding="utf-8"))
            self.assertEqual(rec["pid"], p.pid)
            self.assertEqual(rec["ctime"], self.hv.proc_identity(p.pid)[1])

    def test_room_for_three_admits_exactly_three_in_priority_order(self) -> None:
        os.environ["NUCLEOS_HEAVY_CAPACITY"] = "3"
        low, mid_a, mid_b, mid_c = (self.sleeper() for _ in range(4))
        self.plant(low, 2, 10)       # arrived first, lowest priority
        self.plant(mid_a, 1, 20)
        self.plant(mid_b, 1, 30)
        self.plant(mid_c, 1, 40)
        ok, _ = self.hv.acquire_token(self.state, self.head(prio=0), 30, 1)
        self.assertTrue(ok)
        self.assertEqual(self.held_pids(), {os.getpid(), mid_a.pid, mid_b.pid})
        self.assertEqual(sorted(p.name.split("-")[2] for p in self.queue.iterdir()),
                         sorted([str(low.pid), str(mid_c.pid)]))

    def test_per_pass_cap(self) -> None:
        os.environ["NUCLEOS_HEAVY_CAPACITY"] = "50"
        os.environ["NUCLEOS_HEAVY_BATCH_MAX"] = "2"
        fs = [self.sleeper() for _ in range(4)]
        for i, p in enumerate(fs):
            self.plant(p, 1, 10 + i)
        self.hv.acquire_token(self.state, self.head(prio=0), 30, 1)
        self.assertEqual(len(self.held_pids()), 3)

    def test_weights_are_summed(self) -> None:
        os.environ["NUCLEOS_HEAVY_CAPACITY"] = "4"
        a, b = self.sleeper(), self.sleeper()
        self.plant(a, 1, 10, weight=2)
        self.plant(b, 1, 20, weight=2)
        self.hv.acquire_token(self.state, self.head(prio=0, weight=2), 30, 1)
        self.assertEqual(self.held_pids(), {os.getpid(), a.pid})  # 2 + 2 = 4; b waits

    def test_granted_waiter_proceeds_and_its_release_frees_the_weight(self) -> None:
        os.environ["NUCLEOS_HEAVY_CAPACITY"] = "3"
        # A foreign holder takes the whole budget, so the waiter really queues.
        foreign = self.sleeper()
        _, fct = self.hv.proc_identity(foreign.pid)
        self.held.mkdir(parents=True, exist_ok=True)
        (self.held / str(foreign.pid)).write_text(
            json.dumps({"pid": foreign.pid, "ctime": fct, "weight": 3}), encoding="utf-8")
        w = self.waiter("w", 5, hold=1.0)
        self.assertTrue(wait_for(lambda: self.queue.exists() and any(self.queue.iterdir())))
        threading.Timer(0.5, foreign.kill).start()
        ok, _ = self.hv.acquire_token(self.state, self.head(prio=0), 60, 1)
        self.assertTrue(ok)
        got = self.out / "w.got"
        self.assertTrue(wait_for(got.exists), "the waiter never saw its grant")
        info = json.loads(got.read_text(encoding="utf-8"))
        self.assertTrue(info["ok"])
        self.assertLessEqual(info["mutex"], 2, "a granted waiter should not need the Mutex")
        self.assertEqual(w.wait(timeout=30), 0)
        self.assertNotIn(w.pid, self.held_pids(), "the release left the weight behind")
        self.assertEqual(list(self.queue.iterdir()), [])

    def test_waiter_that_dies_after_being_granted_is_reaped(self) -> None:
        os.environ["NUCLEOS_HEAVY_CAPACITY"] = "2"
        f = self.sleeper()
        self.plant(f, 1, 10)
        self.hv.acquire_token(self.state, self.head(prio=0), 30, 1)
        self.assertIn(f.pid, self.held_pids())
        f.kill()
        f.wait()
        live = {r["pid"] for _, r in self.hv._entries(self.held)}
        self.assertEqual(live, {os.getpid()})
        self.assertNotIn(f.pid, self.held_pids(), "the dead holder was not reaped")

    def test_many_concurrent_waiters_never_exceed_capacity(self) -> None:
        os.environ["NUCLEOS_HEAVY_CAPACITY"] = "4"
        base = time.time_ns()
        ws = [self.waiter(f"w{i}", base + i, hold=0.3) for i in range(10)]
        peak = 0
        stop = threading.Event()

        def sample() -> None:
            nonlocal peak
            while not stop.is_set():
                try:
                    n = sum(1 for _ in self.held.iterdir())
                    peak = max(peak, n)
                except OSError:
                    pass
                time.sleep(0.005)

        th = threading.Thread(target=sample)
        th.start()
        try:
            for w in ws:
                self.assertEqual(w.wait(timeout=240), 0)
        finally:
            stop.set()
            th.join()
        self.assertLessEqual(peak, 4, "held weight went past capacity")
        for i in range(10):
            info = json.loads((self.out / f"w{i}.got").read_text())
            self.assertTrue(info["ok"])
            self.assertLessEqual(info["used"], 4)
        self.assertEqual(list(self.queue.iterdir()), [])

    # ---- dynamic mode: the disk rule and the shared probe --------------------------
    def dynamic_env(self) -> None:
        os.environ["NUCLEOS_HEAVY_FAKE_PROBE"] = json.dumps(
            {"logical": 12, "total_mb": 64000, "avail_mb": 60000, "busy_cores": 1})
        os.environ["NUCLEOS_HEAVY_SETTLE_S"] = "0"

    def test_dynamic_batch_grants_while_the_machine_has_room(self) -> None:
        self.dynamic_env()
        os.environ["NUCLEOS_HEAVY_DYNAMIC_MAX_JOBS"] = "3"
        fs = [self.sleeper() for _ in range(4)]
        for i, p in enumerate(fs):
            self.plant(p, 1, 10 + i)
        self.hv.acquire_token(self.state, self.head(prio=0), 30, 1)
        self.assertEqual(len(self.held_pids()), 3)  # the job ceiling stops the batch

    def test_disk_rule_holds_the_batch(self) -> None:
        self.dynamic_env()
        os.environ["NUCLEOS_HEAVY_FAKE_DISK"] = json.dumps({"busy_pct": 99})
        fs = [self.sleeper() for _ in range(3)]
        for i, p in enumerate(fs):
            self.plant(p, 1, 10 + i)
        ok, _ = self.hv.acquire_token(self.state, self.head(prio=0), 30, 1)
        self.assertTrue(ok)  # nothing held: the head is admitted alone
        self.assertEqual(self.held_pids(), {os.getpid()})
        self.assertEqual(len(list(self.queue.iterdir())), 3)

    def test_shared_probe_fresh_is_reused_and_stale_is_not(self) -> None:
        hv = self.hv
        self.state.mkdir(parents=True, exist_ok=True)
        calls = []

        class HC:
            @staticmethod
            def dynamic_probe():
                calls.append(1)
                return {"avail_mb": 1.0}

        fresh = {"avail_mb": 5.0}
        (self.state / "probe.json").write_text(
            json.dumps({"ts": time.time(), "probe": fresh}), encoding="utf-8")
        self.assertEqual(hv._poll_probe(HC, self.state, False), (fresh, True))
        self.assertEqual(hv._poll_probe(HC, self.state, True), (fresh, True))
        self.assertEqual(calls, [])
        # Stale: a non-head does not sample; the head does and publishes the result.
        (self.state / "probe.json").write_text(
            json.dumps({"ts": time.time() - 30, "probe": fresh}), encoding="utf-8")
        self.assertEqual(hv._poll_probe(HC, self.state, False), (None, False))
        self.assertEqual(calls, [])
        self.assertEqual(hv._poll_probe(HC, self.state, True), ({"avail_mb": 1.0}, True))
        self.assertEqual(calls, [1])
        self.assertEqual(hv._poll_probe(HC, self.state, False), ({"avail_mb": 1.0}, True))
        self.assertEqual(calls, [1])


if __name__ == "__main__":
    unittest.main()
