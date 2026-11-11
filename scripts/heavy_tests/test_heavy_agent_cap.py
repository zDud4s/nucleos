#!/usr/bin/env python3
"""Per-kind cap of the heavy broker (`scripts/heavy.py`): at most NUCLEOS_HEAVY_CAP_MAX brokers
whose `--agent` starts with NUCLEOS_HEAVY_CAP_PREFIX in flight at once. Hermetic: state lives in
a temp NUCLEOS_HEAVY_DIR and the "other brokers" are sleeping processes that own cap records."""

from __future__ import annotations

import importlib.util
import json
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
HEAVY = ROOT / "scripts" / "heavy.py"


def load_heavy():
    spec = importlib.util.spec_from_file_location("heavy_cap_under_test", HEAVY)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


class AgentCapTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.dir = Path(self.tmp.name)
        self.saved = {k: os.environ.get(k) for k in (
            "NUCLEOS_HEAVY_DIR", "NUCLEOS_HEAVY_CAP_PREFIX", "NUCLEOS_HEAVY_CAP_MAX",
            "NUCLEOS_HEAVY_POLL_S")}
        os.environ["NUCLEOS_HEAVY_DIR"] = str(self.dir)
        os.environ["NUCLEOS_HEAVY_POLL_S"] = "0.05"
        os.environ.pop("NUCLEOS_HEAVY_CAP_PREFIX", None)
        os.environ.pop("NUCLEOS_HEAVY_CAP_MAX", None)
        self.h = load_heavy()
        self.procs = []

    def tearDown(self):
        for p in self.procs:
            p.kill()
            p.wait()
        for k, v in self.saved.items():
            if v is None:
                os.environ.pop(k, None)
            else:
                os.environ[k] = v
        self.tmp.cleanup()

    def foreign(self, agent: str, alive: bool = True) -> None:
        """A cap record owned by another process: a live sleeper, or one already gone."""
        p = subprocess.Popen([sys.executable, "-c", "import time; time.sleep(120)"])
        self.procs.append(p)
        _, ctime = self.h.proc_identity(p.pid)
        if not alive:
            p.kill()
            p.wait()
        cap = self.dir / "cap"
        cap.mkdir(parents=True, exist_ok=True)
        (cap / str(p.pid)).write_text(
            json.dumps({"pid": p.pid, "ctime": ctime, "agent": agent}), encoding="utf-8")

    def test_defaults_are_two_leaf_checks(self):
        self.assertEqual(self.h.cap_prefix(), "leaf-")
        self.assertEqual(self.h.cap_max(), 2)

    def test_the_cap_applies_by_agent_prefix_and_is_configurable(self):
        self.assertTrue(self.h.cap_applies("leaf-n12"))
        self.assertFalse(self.h.cap_applies("gate"))
        self.assertFalse(self.h.cap_applies(None))
        os.environ["NUCLEOS_HEAVY_CAP_PREFIX"] = "gate"
        self.assertTrue(self.h.cap_applies("gate-1"))
        self.assertFalse(self.h.cap_applies("leaf-n12"))
        os.environ["NUCLEOS_HEAVY_CAP_PREFIX"] = ""
        self.assertFalse(self.h.cap_applies("gate-1"))
        os.environ["NUCLEOS_HEAVY_CAP_PREFIX"] = "leaf-"
        os.environ["NUCLEOS_HEAVY_CAP_MAX"] = "0"
        self.assertFalse(self.h.cap_applies("leaf-n12"))

    def test_a_third_in_flight_leaf_waits_and_times_out(self):
        self.foreign("leaf-a")
        self.foreign("leaf-b")
        took, waited = self.h.acquire_cap(self.dir, "leaf-c", 0.3)
        self.assertFalse(took)
        self.assertGreaterEqual(waited, 0.3)
        self.assertFalse((self.dir / "cap" / str(os.getpid())).exists())

    def test_a_place_is_taken_below_the_cap_and_given_back(self):
        self.foreign("leaf-a")
        took, _ = self.h.acquire_cap(self.dir, "leaf-c", 1.0)
        self.assertTrue(took)
        mine = self.dir / "cap" / str(os.getpid())
        self.assertTrue(mine.exists())
        self.h.release_cap(self.dir)
        self.assertFalse(mine.exists())

    def test_other_prefixes_and_dead_owners_do_not_count(self):
        self.foreign("gate-1")
        self.foreign("gate-2")
        self.foreign("leaf-dead", alive=False)
        self.foreign("leaf-a")
        took, _ = self.h.acquire_cap(self.dir, "leaf-c", 1.0)
        self.assertTrue(took)

    def test_a_cap_max_of_one_is_honoured(self):
        os.environ["NUCLEOS_HEAVY_CAP_MAX"] = "1"
        self.foreign("leaf-a")
        took, _ = self.h.acquire_cap(self.dir, "leaf-c", 0.2)
        self.assertFalse(took)

    def test_broker_run_exits_75_when_the_cap_is_full_and_runs_uncapped_agents(self):
        self.foreign("leaf-a")
        self.foreign("leaf-b")
        env = dict(os.environ, NUCLEOS_HEAVY_WAIT_MAX="0.3", NUCLEOS_HEAVY_LOCAL="1")
        for k in ("NUCLEOS_HEAVY_TOKEN", "NUCLEOS_HEAVY_HELD"):
            env.pop(k, None)
        capped = subprocess.run(
            [sys.executable, str(HEAVY), "--agent", "leaf-z", "--", sys.executable, "-c", "pass"],
            env=env, capture_output=True, text=True, timeout=60)
        self.assertEqual(capped.returncode, 75, capped.stderr)
        self.assertIn("in-flight places", capped.stderr)
        free = subprocess.run(
            [sys.executable, str(HEAVY), "--agent", "main", "--", sys.executable, "-c", "pass"],
            env=env, capture_output=True, text=True, timeout=60)
        self.assertEqual(free.returncode, 0, free.stderr)


if __name__ == "__main__":
    unittest.main()
