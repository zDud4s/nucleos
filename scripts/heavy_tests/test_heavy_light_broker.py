#!/usr/bin/env python3
"""RED broker-level tests for the light lane (plan 2026-10-21-broker-light-lane, AC4-AC7).

Black-box: `scripts/heavy.py` runs as a subprocess against a throwaway NUCLEOS_HEAVY_DIR holding
a fixture `light.json`; the live probe is injected with NUCLEOS_HEAVY_FAKE_PROBE. "Machine full"
is a live sleeper with a `held/<pid>` record of weight 4 under NUCLEOS_HEAVY_CAPACITY=4, so a
weight-1 request can only get in through the light lane. rc 0 = admitted, rc 75 = it waited
until --wait-max. Until the lane exists, every admission test here ends in rc 75.

    python scripts/heavy_tests/test_heavy_light_broker.py -v
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
sys.path.insert(0, str(ROOT / "scripts"))

import heavy_cost  # noqa: E402

PASS_ARGV = [sys.executable, "-c", "pass"]
SLEEP_ARGV = [sys.executable, "-c", "import time; time.sleep(3)"]
SLEEPER = [sys.executable, "-c", "import time; time.sleep(120)"]
GOOD = {"n": 10, "mem_mb": 500.0, "run_s": 20.0, "cores": 1.0, "light": True}
PROBE = {"logical": 12, "total_mb": 16000, "avail_mb": 8000, "busy_cores": 2}


def cls_of(argv: list[str]) -> str:
    return heavy_cost.class_key(argv, False, True)


def clean_env(state_dir: str, **extra: str) -> dict:
    env = dict(os.environ)
    for k in list(env):
        if k.startswith("NUCLEOS_HEAVY"):
            env.pop(k)
    env["NUCLEOS_HEAVY_DIR"] = state_dir
    env.update(extra)
    return env


def read_jsonl(path: Path) -> list[dict]:
    if not path.exists():
        return []
    return [json.loads(line) for line in path.read_text(encoding="utf-8").splitlines()
            if line.strip()]


class Base(unittest.TestCase):
    def setUp(self) -> None:
        self.tmp = tempfile.mkdtemp(prefix="heavy-light-")
        self.state = Path(self.tmp) / "state"
        self.state.mkdir(parents=True, exist_ok=True)
        self.addCleanup(shutil.rmtree, self.tmp, True)

    # -- fixtures ---------------------------------------------------------------------------

    def sleeper(self) -> subprocess.Popen:
        p = subprocess.Popen(SLEEPER)
        self.addCleanup(p.wait)
        self.addCleanup(p.kill)
        return p

    def dead_pid(self) -> int:
        p = subprocess.Popen(PASS_ARGV)
        p.wait()
        return p.pid

    def hold_full(self) -> subprocess.Popen:
        """The whole capacity (4) held by a live process."""
        p = self.sleeper()
        held = self.state / "held"
        held.mkdir(parents=True, exist_ok=True)
        (held / str(p.pid)).write_text(
            json.dumps({"pid": p.pid, "weight": 4, "cls": "other|-|-|a|c"}), encoding="utf-8")
        return p

    def light_holder(self, pid: int, cls: str = "other|-|-|a|c") -> None:
        d = self.state / "light"
        d.mkdir(parents=True, exist_ok=True)
        (d / str(pid)).write_text(json.dumps({"pid": pid, "cls": cls}), encoding="utf-8")

    def clear_light_holders(self) -> None:
        for f in (self.state / "light").glob("*"):
            f.unlink()

    def write_light(self, argv: list[str] = PASS_ARGV, **stats) -> None:
        entry = dict(GOOD)
        entry.update(stats)
        (self.state / "light.json").write_text(json.dumps({
            "v": 1, "built": time.strftime("%Y-%m-%dT%H:%M:%S%z"),
            "classes": {cls_of(argv): entry}}), encoding="utf-8")

    def env(self, probe: dict | None = None, **extra: str) -> dict:
        return clean_env(
            str(self.state), NUCLEOS_HEAVY_CAPACITY="4", NUCLEOS_HEAVY_POLL_S="0.1",
            NUCLEOS_HEAVY_LIGHT_RETRY_S="0.2",
            NUCLEOS_HEAVY_FAKE_PROBE=json.dumps(PROBE if probe is None else probe), **extra)

    def run_broker(self, argv: list[str], env: dict, wait_max: str = "2",
                   timeout: float = 60) -> subprocess.CompletedProcess:
        return subprocess.run(
            [sys.executable, str(HEAVY), "--wait-max", wait_max, "--", *argv], env=env,
            capture_output=True, text=True, timeout=timeout, cwd=str(ROOT))

    def log_rows(self) -> list[dict]:
        return [r for r in read_jsonl(self.state / "log.jsonl") if r.get("cls")]

    def held_names(self) -> list[str]:
        return sorted(f.name for f in (self.state / "held").glob("*"))

    def assertWaits(self, cp: subprocess.CompletedProcess) -> None:
        self.assertEqual(cp.returncode, 75, cp.stdout + cp.stderr)
        self.assertEqual(self.log_rows(), [])  # nothing ran


class Admission(Base):
    def test_light_admitted_while_held_full_and_row_has_lane(self) -> None:
        holder = self.hold_full()
        self.write_light()
        cp = self.run_broker(PASS_ARGV, self.env())
        self.assertEqual(cp.returncode, 0, cp.stdout + cp.stderr)
        rows = self.log_rows()
        self.assertEqual(len(rows), 1, rows)
        self.assertEqual(rows[0]["lane"], "light")
        self.assertEqual(rows[0]["exit"], 0)
        # It never took a token: the held weight is still the sleeper's alone ...
        self.assertEqual(self.held_names(), [str(holder.pid)])
        # ... and its lane record is gone.
        self.assertEqual(list((self.state / "light").glob("*")), [])

    def test_normal_row_has_no_lane_key(self) -> None:
        self.write_light()
        # Capacity is free: the ordinary token path admits it, light.json notwithstanding.
        cp = self.run_broker(PASS_ARGV, self.env())
        self.assertEqual(cp.returncode, 0, cp.stdout + cp.stderr)
        rows = self.log_rows()
        self.assertEqual(len(rows), 1, rows)
        self.assertNotIn("lane", rows[0])
        # Same without any light.json at all.
        (self.state / "light.json").unlink()
        (self.state / "log.jsonl").unlink()
        cp = self.run_broker(PASS_ARGV, self.env())
        self.assertEqual(cp.returncode, 0, cp.stdout + cp.stderr)
        rows = self.log_rows()
        self.assertEqual(len(rows), 1, rows)
        self.assertNotIn("lane", rows[0])

    def test_not_light_class_or_missing_entry_waits(self) -> None:
        self.hold_full()
        with self.subTest("class absent from light.json"):
            (self.state / "light.json").write_text(json.dumps({
                "v": 1, "classes": {"other|-|-|a|c": dict(GOOD)}}), encoding="utf-8")
            self.assertWaits(self.run_broker(PASS_ARGV, self.env()))
        with self.subTest("light.json absent"):
            (self.state / "light.json").unlink()
            self.assertWaits(self.run_broker(PASS_ARGV, self.env()))
        with self.subTest("class present but flagged not light"):
            self.write_light(light=False, mem_mb=4000.0)
            self.assertWaits(self.run_broker(PASS_ARGV, self.env()))

    def test_unfiltered_heavy_class_waits(self) -> None:
        # The stored flag says light; the stats do not, and the stats win.
        self.hold_full()
        for name, stats in (("mem", dict(mem_mb=4000.0)), ("run", dict(run_s=600.0)),
                            ("cores", dict(cores=8.0)), ("n", dict(n=3))):
            with self.subTest(name):
                self.write_light(light=True, **stats)
                self.assertWaits(self.run_broker(PASS_ARGV, self.env()))

    def test_cargo_waits_even_if_class_marked_light(self) -> None:
        if os.name == "nt":
            cargo = Path(self.tmp) / "cargo.cmd"
            cargo.write_text("@echo off\r\nexit /b 0\r\n", encoding="utf-8")
        else:
            cargo = Path(self.tmp) / "cargo"
            cargo.write_text("#!/bin/sh\nexit 0\n", encoding="utf-8")
            cargo.chmod(0o755)
        argv = [str(cargo), "check"]
        self.assertEqual(cls_of(argv), "cargo|check|-|a|c")
        self.hold_full()
        self.write_light(argv=argv)
        self.assertWaits(self.run_broker(argv, self.env(NUCLEOS_HEAVY_TARGET="0")))

    def test_low_probe_memory_waits(self) -> None:
        self.hold_full()
        self.write_light()  # 500 MB + the 1024 MB margin needs 1524 MB available
        low = dict(PROBE, avail_mb=1000)
        self.assertWaits(self.run_broker(PASS_ARGV, self.env(probe=low)))

    def test_cap_reached_waits_dead_holder_reaped(self) -> None:
        self.hold_full()
        self.write_light()
        a, b = self.sleeper(), self.sleeper()
        with self.subTest("two live holders: the default cap of 2 is reached"):
            self.light_holder(a.pid)
            self.light_holder(b.pid)
            self.assertWaits(self.run_broker(PASS_ARGV, self.env()))
        with self.subTest("one live and one dead: the dead one does not count"):
            self.clear_light_holders()
            self.light_holder(a.pid)
            self.light_holder(self.dead_pid())
            cp = self.run_broker(PASS_ARGV, self.env())
            self.assertEqual(cp.returncode, 0, cp.stdout + cp.stderr)
            self.assertEqual(self.log_rows()[-1]["lane"], "light")
            # The dead record was reaped, the live one left alone.
            self.assertEqual([f.name for f in (self.state / "light").glob("*")], [str(a.pid)])
        with self.subTest("NUCLEOS_HEAVY_LIGHT_MAX=1 with one live holder waits"):
            (self.state / "log.jsonl").unlink()
            self.assertWaits(self.run_broker(PASS_ARGV, self.env(NUCLEOS_HEAVY_LIGHT_MAX="1")))

    def test_disabled_by_env(self) -> None:
        self.hold_full()
        self.write_light()
        self.assertWaits(self.run_broker(PASS_ARGV, self.env(NUCLEOS_HEAVY_LIGHT="0")))
        # Positive control: the very same state, switch unset, is admitted.
        cp = self.run_broker(PASS_ARGV, self.env())
        self.assertEqual(cp.returncode, 0, cp.stdout + cp.stderr)


class Lifecycle(Base):
    def test_light_registry_lifecycle(self) -> None:
        self.hold_full()
        self.write_light(argv=SLEEP_ARGV)
        p = subprocess.Popen(
            [sys.executable, str(HEAVY), "--wait-max", "20", "--", *SLEEP_ARGV],
            env=self.env(), stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
            cwd=str(ROOT))
        self.addCleanup(p.wait)
        self.addCleanup(p.kill)
        record = self.state / "light" / str(p.pid)
        seen = None
        deadline = time.time() + 15
        while time.time() < deadline and p.poll() is None:
            if record.exists():
                try:
                    seen = json.loads(record.read_text(encoding="utf-8"))
                except (OSError, ValueError):
                    pass
                else:
                    break
            time.sleep(0.05)
        out, err = p.communicate(timeout=30)
        self.assertEqual(p.returncode, 0, out + err)
        self.assertIsNotNone(seen, "light/<pid> never appeared while the command ran")
        self.assertEqual(seen["pid"], p.pid)
        self.assertEqual(seen["cls"], cls_of(SLEEP_ARGV))
        self.assertFalse(record.exists())
        self.assertEqual(list((self.state / "light").glob("*")), [])

    def test_corrupt_light_json_is_fail_open(self) -> None:
        (self.state / "light.json").write_text("not json{", encoding="utf-8")
        with self.subTest("capacity free: runs normally"):
            cp = self.run_broker(PASS_ARGV, self.env())
            self.assertEqual(cp.returncode, 0, cp.stdout + cp.stderr)
            self.assertNotIn("lane", self.log_rows()[-1])
        with self.subTest("machine full: waits, no crash"):
            (self.state / "light.json").write_text("not json{", encoding="utf-8")
            (self.state / "log.jsonl").unlink()
            self.hold_full()
            self.assertWaits(self.run_broker(PASS_ARGV, self.env()))
            self.assertNotIn("Traceback", self.run_broker(PASS_ARGV, self.env()).stderr)

    def test_light_json_rebuilt_after_a_run(self) -> None:
        ts = time.strftime("%Y-%m-%dT%H:%M:%S%z")
        rows = [{"v": 1, "ts": ts, "cls": "x|y|-|a|c", "run_s": 5, "cores_avg": 1.0,
                 "cores_p90": 1.0, "peak_mem_mb": 300.0} for _ in range(5)]
        (self.state / "log.jsonl").write_text(
            "".join(json.dumps(r) + "\n" for r in rows), encoding="utf-8")
        self.assertFalse((self.state / "light.json").exists())
        cp = self.run_broker(PASS_ARGV, self.env())
        self.assertEqual(cp.returncode, 0, cp.stdout + cp.stderr)
        light = json.loads((self.state / "light.json").read_text(encoding="utf-8"))
        entry = light["classes"]["x|y|-|a|c"]
        self.assertEqual(entry["n"], 5)
        self.assertIs(entry["light"], True)

    def test_light_retried_while_queued_when_cap_frees(self) -> None:
        holder = self.hold_full()
        self.write_light()
        a, b = self.sleeper(), self.sleeper()
        self.light_holder(a.pid)
        self.light_holder(b.pid)
        t0 = time.time()
        p = subprocess.Popen(
            [sys.executable, str(HEAVY), "--wait-max", "30", "--", *PASS_ARGV],
            env=self.env(), stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
            cwd=str(ROOT))
        self.addCleanup(p.wait)
        self.addCleanup(p.kill)
        time.sleep(1.5)
        self.assertIsNone(p.poll(), "admitted while the light cap was still reached")
        a.kill()
        a.wait()
        out, err = p.communicate(timeout=25)
        self.assertEqual(p.returncode, 0, out + err)
        self.assertLess(time.time() - t0, 20, "was not admitted soon after the cap freed")
        rows = self.log_rows()
        self.assertEqual(len(rows), 1, rows)
        self.assertEqual(rows[0]["lane"], "light")
        # Admitted through the lane: the held weight is still the full-capacity holder's.
        self.assertEqual(self.held_names(), [str(holder.pid)])


if __name__ == "__main__":
    unittest.main()
