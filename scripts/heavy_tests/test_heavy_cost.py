#!/usr/bin/env python3
"""RED tests for the broker's cost model and shadow admission (`scripts/heavy_cost.py`):
class key, measurement helpers, per-class cost model, budget/fits and admission mode.

`heavy_cost.py` does not exist yet, so every test here fails for that reason.
"""

from __future__ import annotations

import importlib.util
import json
import os
import sys
import unittest
from datetime import datetime, timedelta
from pathlib import Path
from unittest import mock

ROOT = Path(__file__).resolve().parents[2]
COST = ROOT / "scripts" / "heavy_cost.py"

TRACE = [(0, 0), (1, 1), (2, 4), (3, 5), (4, 10), (5, 11), (6, 12)]

ENV_KEYS = (
    "NUCLEOS_HEAVY_CAPACITY", "NUCLEOS_HEAVY_ADMISSION", "NUCLEOS_HEAVY_RESERVE_CPU",
    "NUCLEOS_HEAVY_RESERVE_MEM_MB", "NUCLEOS_HEAVY_FAKE_PROBE",
)


def load_cost():
    """Import scripts/heavy_cost.py as a module; raises (red) while the file is missing."""
    spec = importlib.util.spec_from_file_location("heavy_cost_under_test", COST)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


def env_without(**extra: str):
    """A patch.dict context with every NUCLEOS_HEAVY_* key under test removed, then `extra`."""
    ctx = mock.patch.dict(os.environ, extra, clear=False)
    return ctx


def pop_keys():
    for k in ENV_KEYS:
        os.environ.pop(k, None)


def ts_ago(days: float) -> str:
    when = datetime.now().astimezone() - timedelta(days=days)
    return when.strftime("%Y-%m-%dT%H:%M:%S%z")


class ClassKey(unittest.TestCase):
    def test_class_key_names_tool_subcommand_scope_filter_and_compiled(self):
        c = load_cost()
        self.assertEqual(
            c.class_key(["cargo", "test", "-p", "nucleos-core", "foo"], filtered=True),
            "cargo|test|nucleos-core|f|c",
        )
        self.assertEqual(c.class_key(["npm", "--prefix", "shell", "test"]), "npm|test|shell|a|c")
        self.assertEqual(
            c.class_key(["cargo", "clippy", "--all-targets"], compiled=False),
            "cargo|clippy|-|a|n",
        )
        self.assertEqual(c.class_key(["go", "test", "./..."], compiled=False), "go|test|-|a|c")
        self.assertEqual(c.class_key(["C:/x/npm.cmd", "exec", "tsc"]), "npm|exec:tsc|-|a|c")
        self.assertEqual(c.class_key([sys.executable, "-c", "print(1)"]), "python|-|-|a|c")


class Measure(unittest.TestCase):
    def test_cores_p90_takes_the_p90_of_two_second_cpu_rates(self):
        c = load_cost()
        self.assertEqual(c.cores_p90(TRACE), 3.0)
        self.assertIsNone(c.cores_p90([]))
        self.assertIsNone(c.cores_p90([(0, 0)]))

    def test_measure_fields_fill_cores_avg_and_tolerate_missing_data(self):
        c = load_cost()
        got = c.measure_fields({"cpu_s": 60.0, "cpu_trace": TRACE}, 30.0, None)
        self.assertEqual(got["cores_avg"], 2.0)
        self.assertEqual(got["cores_p90"], 3.0)
        self.assertIsNone(got["peak_mem_mb"])
        empty = c.measure_fields({}, 0, None)
        self.assertEqual(
            empty, {"peak_mem_mb": None, "cores_avg": None, "cores_p90": None}
        )


class CostModel(unittest.TestCase):
    def test_build_costs_uses_the_last_twenty_long_recent_runs_and_skips_old_rows(self):
        c = load_cost()
        A, B = "cargo|test|nucleos-core|f|c", "npm|test|shell|a|c"
        rows = []
        for _ in range(3):  # too old, must be ignored whatever they say
            rows.append({"cls": A, "run_s": 30, "ts": ts_ago(40), "cores_p90": 99, "peak_mem_mb": 99999})
        rows.append({"cls": A, "run_s": 30, "ts": ts_ago(1)})  # eligible, carries no numbers
        for i in range(1, 26):
            rows.append(
                {"cls": A, "run_s": 30, "ts": ts_ago(1), "cores_p90": i, "peak_mem_mb": 100 * i}
            )
        for _ in range(5):  # too short
            rows.append({"cls": A, "run_s": 5, "ts": ts_ago(1), "cores_p90": 99, "peak_mem_mb": 99999})
        for _ in range(2):  # fewer than min_n eligible rows
            rows.append({"cls": B, "run_s": 30, "ts": ts_ago(1), "cores_p90": 4, "peak_mem_mb": 400})
        rows.append({"v": 0, "argv0": "cargo"})
        out = c.build_costs(rows)
        self.assertEqual(out["v"], 1)
        self.assertIn("built", out)
        self.assertNotIn(B, out["classes"])
        a = out["classes"][A]
        self.assertEqual(a["n"], 20)
        self.assertEqual(a["cores"], 23)
        self.assertEqual(a["mem_mb"], 2300)

    def test_predict_falls_back_to_static_defaults(self):
        c = load_cost()
        self.assertEqual(c.predict({}, "npm|test|shell|a|c"), (8.0, 2048.0))
        self.assertEqual(c.predict({}, "cargo|nextest|-|f|c"), (5.0, 3072.0))
        self.assertEqual(c.predict({}, "weird|x|-|a|c", 2), (4.0, 4096.0))
        costs = {"classes": {"k": {"cores": 3, "mem_mb": 900, "n": 5}}}
        self.assertEqual(c.predict(costs, "k"), (3, 900))


PROBE = {"logical": 12, "total_mb": 16000, "avail_mb": 12000, "busy_cores": 0}


def held():
    return [{"pred_cores": 4, "pred_mem_mb": 2000, "weight": 1}]


def head(cores, mem):
    return {"pred_cores": cores, "pred_mem_mb": mem, "weight": 1}


class Fits(unittest.TestCase):
    def setUp(self):
        patcher = mock.patch.dict(os.environ, {}, clear=False)
        patcher.start()
        self.addCleanup(patcher.stop)
        pop_keys()

    def test_fits_checks_cpu_and_memory_budgets(self):
        c = load_cost()
        self.assertTrue(c.fits(held(), head(4, 2000), dict(PROBE), {})[0])
        ok, d = c.fits(held(), head(7, 2000), dict(PROBE), {})
        self.assertFalse(ok)
        self.assertEqual(d["need_cpu"], 11.0)
        self.assertEqual(d["b_cpu"], 10.0)
        ok, d = c.fits(held(), head(1, 9000), dict(PROBE), {})
        self.assertFalse(ok)
        self.assertEqual(d["need_mem_mb"], 13000.0)
        self.assertEqual(d["b_mem_mb"], 12800.0)
        self.assertEqual(d["rule"], "budget")

    def test_alone_rule_and_memory_guard(self):
        c = load_cost()
        ok, d = c.fits([], head(50, 50000), dict(PROBE), {})
        self.assertTrue(ok)
        self.assertEqual(d["rule"], "alone")
        ok, d = c.fits(held(), head(1, 1), dict(PROBE, avail_mb=3000), {})
        self.assertFalse(ok)
        self.assertEqual(d["rule"], "mem_guard")

    def test_reserve_overrides_from_env(self):
        c = load_cost()
        self.assertFalse(c.fits(held(), head(7, 2000), dict(PROBE), {})[0])
        with mock.patch.dict(os.environ, {"NUCLEOS_HEAVY_RESERVE_CPU": "0.5"}):
            self.assertTrue(c.fits(held(), head(7, 2000), dict(PROBE), {})[0])
        self.assertFalse(c.fits(held(), head(1, 9000), dict(PROBE), {})[0])
        with mock.patch.dict(os.environ, {"NUCLEOS_HEAVY_RESERVE_MEM_MB": "1000"}):
            self.assertTrue(c.fits(held(), head(1, 9000), dict(PROBE), {})[0])


class Mode(unittest.TestCase):
    def setUp(self):
        patcher = mock.patch.dict(os.environ, {}, clear=False)
        patcher.start()
        self.addCleanup(patcher.stop)
        pop_keys()

    def test_admission_mode_defaults_to_shadow_and_capacity_forces_static(self):
        c = load_cost()
        self.assertEqual(c.admission_mode(), "shadow")
        with mock.patch.dict(os.environ, {"NUCLEOS_HEAVY_ADMISSION": "dynamic"}):
            self.assertEqual(c.admission_mode(), "shadow")
        with mock.patch.dict(os.environ, {"NUCLEOS_HEAVY_ADMISSION": "static"}):
            self.assertEqual(c.admission_mode(), "static")
        with mock.patch.dict(os.environ, {"NUCLEOS_HEAVY_CAPACITY": "4"}):
            self.assertEqual(c.admission_mode(), "static")


class Probe(unittest.TestCase):
    def setUp(self):
        patcher = mock.patch.dict(os.environ, {}, clear=False)
        patcher.start()
        self.addCleanup(patcher.stop)
        pop_keys()

    def test_fake_probe_env_and_real_probe_shape(self):
        c = load_cost()
        with mock.patch.dict(os.environ, {"NUCLEOS_HEAVY_FAKE_PROBE": json.dumps(PROBE)}):
            self.assertEqual(c.probe(), PROBE)
        real = c.probe()
        self.assertGreaterEqual(real["logical"], 1)
        self.assertGreater(real["total_mb"], 0)
        self.assertGreater(real["avail_mb"], 0)
        self.assertLessEqual(real["avail_mb"], real["total_mb"])
        self.assertIn("busy_cores", real)


if __name__ == "__main__":
    unittest.main()
