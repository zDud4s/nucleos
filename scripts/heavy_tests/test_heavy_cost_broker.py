#!/usr/bin/env python3
"""RED broker-level tests for the cost model and shadow admission (plan
2026-10-14-broker-gate-efficiency-c, packets P3 and P4).

Black-box: `scripts/heavy.py` runs as a subprocess against a throwaway
NUCLEOS_HEAVY_DIR; the live probe is injected with NUCLEOS_HEAVY_FAKE_PROBE. Until
`scripts/heavy_cost.py` exists and heavy.py calls it, every test here fails.

    python scripts/heavy_tests/test_heavy_cost_broker.py -v
"""

from __future__ import annotations

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

CLS_KEYS = {"v", "ts", "kind", "weight", "run_s", "exit", "cls", "cores_avg",
            "cores_p90", "peak_mem_mb", "pred_cores", "pred_mem_mb"}


def clean_env(state_dir: str, **extra: str) -> dict:
    env = dict(os.environ)
    for k in list(env):
        if k.startswith("NUCLEOS_HEAVY"):
            env.pop(k)
    env["NUCLEOS_HEAVY_DIR"] = state_dir
    env.update(extra)
    return env


def run_broker(args: list[str], env: dict, timeout: float = 90) -> subprocess.CompletedProcess:
    return subprocess.run([sys.executable, str(HEAVY), *args], env=env,
                          capture_output=True, text=True, timeout=timeout, cwd=str(ROOT))


def read_jsonl(path: Path) -> list[dict]:
    if not path.exists():
        return []
    return [json.loads(line) for line in path.read_text(encoding="utf-8").splitlines()
            if line.strip()]


class Base(unittest.TestCase):
    def setUp(self) -> None:
        self.tmp = tempfile.mkdtemp(prefix="heavy-cost-")
        self.state = Path(self.tmp) / "state"
        self.state.mkdir(parents=True, exist_ok=True)
        self.addCleanup(shutil.rmtree, self.tmp, True)

    def write_rows(self, name: str, rows: list[dict]) -> None:
        (self.state / name).write_text(
            "".join(json.dumps(r) + "\n" for r in rows), encoding="utf-8")


class Measured(Base):
    def test_log_row_carries_cost_fields(self) -> None:
        cp = run_broker(
            ["--", sys.executable, "-c", "import time\nt=time.time()\nwhile time.time()-t<3: pass"],
            clean_env(str(self.state)))
        self.assertEqual(cp.returncode, 0, cp.stdout + cp.stderr)
        rows = read_jsonl(self.state / "log.jsonl")
        self.assertEqual(len(rows), 1, rows)
        row = rows[0]
        self.assertTrue(CLS_KEYS <= set(row), f"missing: {CLS_KEYS - set(row)}")
        self.assertEqual(row["cls"], "python|-|-|a|c")
        if os.name == "nt":
            self.assertGreater(row["peak_mem_mb"], 0)
            self.assertGreater(row["cores_avg"], 0)

    def test_costs_json_is_rebuilt_after_a_run(self) -> None:
        ts = time.strftime("%Y-%m-%dT%H:%M:%S%z")
        self.write_rows("log.jsonl", [
            {"v": 1, "ts": ts, "cls": "x|y|-|a|c", "run_s": 30, "cores_p90": 3,
             "peak_mem_mb": 1000} for _ in range(3)])
        cp = run_broker(["--", sys.executable, "-c", "pass"], clean_env(str(self.state)))
        self.assertEqual(cp.returncode, 0, cp.stdout + cp.stderr)
        costs = json.loads((self.state / "costs.json").read_text(encoding="utf-8"))
        self.assertEqual(costs["classes"]["x|y|-|a|c"]["cores"], 3)


class Shadow(Base):
    def setUp(self) -> None:
        super().setUp()
        self.holder = subprocess.Popen([sys.executable, "-c", "import time; time.sleep(120)"])
        self.addCleanup(self.holder.wait)
        self.addCleanup(self.holder.kill)

    def hold(self, weight: int, pred_cores: float, pred_mem_mb: float) -> None:
        held = self.state / "held"
        held.mkdir(parents=True, exist_ok=True)
        (held / str(self.holder.pid)).write_text(json.dumps({
            "pid": self.holder.pid, "weight": weight,
            "pred_cores": pred_cores, "pred_mem_mb": pred_mem_mb}), encoding="utf-8")

    @staticmethod
    def probe(busy: float) -> str:
        return json.dumps({"logical": 12, "total_mb": 16000, "avail_mb": 12000,
                           "busy_cores": busy})

    def env(self, busy: float = 9, **extra: str) -> dict:
        return clean_env(str(self.state), NUCLEOS_HEAVY_POLL_S="0.1",
                         NUCLEOS_HEAVY_FAKE_PROBE=self.probe(busy), **extra)

    def test_disagreement_is_logged_and_static_still_admits(self) -> None:
        self.hold(3, 9, 1000)
        cp = run_broker(["--", sys.executable, "-c", "pass"], self.env(9))
        self.assertEqual(cp.returncode, 0, cp.stdout + cp.stderr)
        rows = read_jsonl(self.state / "admission.jsonl")
        self.assertEqual(len(rows), 1, rows)
        self.assertIs(rows[0]["admitted"], True)
        self.assertIs(rows[0]["agree"], False)
        self.assertIs(rows[0]["first_disagreement"]["shadow"], False)

    def test_static_wait_stands_when_shadow_would_admit(self) -> None:
        self.hold(4, 1, 500)
        cp = run_broker(["--wait-max", "2", "--", sys.executable, "-c", "pass"], self.env(1))
        self.assertEqual(cp.returncode, 75, cp.stdout + cp.stderr)
        rows = read_jsonl(self.state / "admission.jsonl")
        self.assertEqual(len(rows), 1, rows)
        self.assertIs(rows[0]["admitted"], False)
        self.assertIs(rows[0]["agree"], False)
        self.assertIsNone(rows[0]["static_wait_s"])
        self.assertIsNotNone(rows[0]["shadow_wait_s"])

    def test_capacity_or_static_mode_disables_shadow(self) -> None:
        # Positive control: the same setup with neither switch does write a shadow row.
        self.hold(3, 9, 1000)
        cp = run_broker(["--", sys.executable, "-c", "pass"], self.env(9))
        self.assertEqual(cp.returncode, 0, cp.stdout + cp.stderr)
        self.assertTrue((self.state / "admission.jsonl").exists())
        for extra in ({"NUCLEOS_HEAVY_CAPACITY": "4"}, {"NUCLEOS_HEAVY_ADMISSION": "static"}):
            with self.subTest(extra=extra):
                (self.state / "admission.jsonl").unlink(missing_ok=True)
                self.hold(3, 9, 1000)
                cp = run_broker(["--", sys.executable, "-c", "pass"], self.env(9, **extra))
                self.assertEqual(cp.returncode, 0, cp.stdout + cp.stderr)
                self.assertFalse((self.state / "admission.jsonl").exists())

    def test_broken_probe_is_fail_open(self) -> None:
        env = clean_env(str(self.state), NUCLEOS_HEAVY_POLL_S="0.1",
                        NUCLEOS_HEAVY_FAKE_PROBE="not json{")
        cp = run_broker(["--", sys.executable, "-c", "pass"], env)
        self.assertEqual(cp.returncode, 0, cp.stdout + cp.stderr)
        log = read_jsonl(self.state / "log.jsonl")
        self.assertEqual(len(log), 1)
        # The cost fields are independent of the probe, so they must still be logged.
        self.assertIn("cls", log[0])
        self.assertFalse((self.state / "admission.jsonl").exists())


class Report(Base):
    def test_report_shows_agreement_and_prediction_error_per_class(self) -> None:
        ts = time.strftime("%Y-%m-%dT%H:%M:%S%z")
        cls = "npm|test|shell|a|c"
        rows = [{"v": 0, "argv0": "cargo"}, {"v": 1, "ts": ts, "argv0": "cargo"}]
        for p90, peak in ((6, 1048), (10, 3048)):
            rows.append({"v": 1, "ts": ts, "argv0": "npm", "cls": cls, "pred_cores": 8,
                         "cores_p90": p90, "pred_mem_mb": 2048, "peak_mem_mb": peak})
        self.write_rows("log.jsonl", rows)
        self.write_rows("admission.jsonl", [
            {"v": 1, "ts": ts, "admitted": True, "agree": a} for a in (True, True, True, False)])
        cp = run_broker(["report"], clean_env(str(self.state)))
        self.assertEqual(cp.returncode, 0, cp.stdout + cp.stderr)
        self.assertRegex(cp.stdout, r"agreement\D{0,5}75%")
        self.assertIn(cls, cp.stdout)
        self.assertRegex(cp.stdout, r"err_cores=2\.0")


if __name__ == "__main__":
    unittest.main()
