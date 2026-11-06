#!/usr/bin/env python3
"""RED tests for the heavy broker's disk-pressure admission gate: while something is held and
the disk is saturated (busy_pct >= NUCLEOS_HEAVY_DISK_BUSY_PCT, default 90; optionally also
queue >= NUCLEOS_HEAVY_DISK_QUEUE) the head of the queue waits, rule "disk". Nothing held is
still admitted ("alone"). `0` switches the gate off; an unreadable signal never gates.

`dynamic_fit`, `disk_threshold`, `disk_saturated`, `disk_probe` and `measure_fields` are tested
by importing scripts/heavy_cost.py; the broker is exercised black-box as a subprocess against a
throwaway NUCLEOS_HEAVY_DIR, with the disk signal injected through NUCLEOS_HEAVY_FAKE_DISK.

    python -m pytest -q scripts/heavy_tests/test_heavy_disk_admission.py
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
HEAVY = ROOT / "scripts" / "heavy.py"
COST = ROOT / "scripts" / "heavy_cost.py"

SLEEPER = r'''
import os, pathlib, sys, time

d = pathlib.Path(os.environ["SLEEPER_DIR"])
tag = sys.argv[-1]
(d / f"{tag}.start").write_text(str(os.getpid()))
while not ((d / f"{tag}.stop").exists() or (d / "all.stop").exists()):
    time.sleep(0.05)
'''

ADM_KEYS = {"mode", "rule", "avail_mb", "idle_cores", "held_n"}

# Every env var that changes what the code under test decides.
CLEARED = (
    "NUCLEOS_HEAVY_CAPACITY", "NUCLEOS_HEAVY_ADMISSION", "NUCLEOS_HEAVY_MIN_AVAIL_MB",
    "NUCLEOS_HEAVY_MIN_IDLE_CORES", "NUCLEOS_HEAVY_SETTLE_S", "NUCLEOS_HEAVY_SETTLE_FREE_PCT",
    "NUCLEOS_HEAVY_DYNAMIC_MAX_JOBS", "NUCLEOS_HEAVY_FAKE_PROBE", "NUCLEOS_HEAVY_FAKE_DISK",
    "NUCLEOS_HEAVY_DISK_BUSY_PCT", "NUCLEOS_HEAVY_DISK_QUEUE", "NUCLEOS_HEAVY_DISK_CACHE_S",
)


def wait_for(pred, timeout: float = 30.0, step: float = 0.1) -> bool:
    deadline = time.time() + timeout
    while time.time() < deadline:
        if pred():
            return True
        time.sleep(step)
    return pred()


def probe_json(avail_mb: float = 20000, busy: float | None = 2) -> str:
    return json.dumps({"logical": 12, "total_mb": 32000, "avail_mb": avail_mb,
                       "busy_cores": busy})


def room_probe() -> dict:
    return {"logical": 12, "total_mb": 32000, "avail_mb": 20000, "busy_cores": 2}


class _CostCase(unittest.TestCase):
    def load(self):
        patcher = mock.patch.dict(os.environ, {}, clear=False)
        patcher.start()
        self.addCleanup(patcher.stop)
        for k in CLEARED:
            os.environ.pop(k, None)
        spec = importlib.util.spec_from_file_location("heavy_cost_disk_under_test", COST)
        mod = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(mod)
        return mod

    def fit(self, c, n_held, disk, p="room"):
        probe = room_probe() if p == "room" else p
        return c.dynamic_fit(n_held, probe, 1000.0, 8, True, disk=disk)


class DiskFit(_CostCase):
    def test_saturated_with_held_is_held(self) -> None:
        c = self.load()
        ok, adm = self.fit(c, 1, {"busy_pct": 97})
        self.assertFalse(ok)
        self.assertEqual(adm["rule"], "disk")
        self.assertEqual(adm["disk_busy_pct"], 97)
        self.assertEqual(set(adm), ADM_KEYS | {"disk_busy_pct"})

    def test_saturated_none_held_admitted(self) -> None:
        c = self.load()
        ok, adm = self.fit(c, 0, {"busy_pct": 97})
        self.assertTrue(ok)
        self.assertEqual(adm["rule"], "alone")

    def test_threshold_zero_is_off(self) -> None:
        c = self.load()
        os.environ["NUCLEOS_HEAVY_DISK_BUSY_PCT"] = "0"
        ok, adm = self.fit(c, 1, {"busy_pct": 97})
        self.assertTrue(ok)
        self.assertEqual(adm["rule"], "room")

    def test_no_signal_not_gated(self) -> None:
        c = self.load()
        for disk in (None, {"busy_pct": "x"}, "junk"):
            ok, adm = self.fit(c, 1, disk)
            self.assertTrue(ok, disk)
            self.assertEqual(adm["rule"], "room", disk)
            self.assertEqual(set(adm), ADM_KEYS, disk)

    def test_below_threshold_normal_room(self) -> None:
        c = self.load()
        ok, adm = self.fit(c, 1, {"busy_pct": 50})
        self.assertTrue(ok)
        self.assertEqual(adm["rule"], "room")
        self.assertEqual(set(adm), ADM_KEYS)

    def test_queue_gate_needs_both(self) -> None:
        c = self.load()
        os.environ["NUCLEOS_HEAVY_DISK_QUEUE"] = "2"
        ok, adm = self.fit(c, 1, {"busy_pct": 95, "queue": 1})
        self.assertTrue(ok)
        self.assertEqual(adm["rule"], "room")
        ok, adm = self.fit(c, 1, {"busy_pct": 95, "queue": 3})
        self.assertFalse(ok)
        self.assertEqual(adm["rule"], "disk")

    def test_disk_hold_beats_static_fallback(self) -> None:
        c = self.load()
        ok, adm = self.fit(c, 1, {"busy_pct": 97}, p=None)
        self.assertFalse(ok)
        self.assertEqual(adm["rule"], "disk")

    def test_threshold_parse(self) -> None:
        c = self.load()
        self.assertEqual(c.disk_threshold(), 90.0)
        for raw, want in (("0", 0.0), ("abc", 90.0), ("-5", 90.0), ("75", 75.0)):
            with mock.patch.dict(os.environ, {"NUCLEOS_HEAVY_DISK_BUSY_PCT": raw}):
                self.assertEqual(c.disk_threshold(), want, raw)


class DiskProbe(_CostCase):
    def test_fake_disk_env(self) -> None:
        c = self.load()
        os.environ["NUCLEOS_HEAVY_FAKE_DISK"] = '{"busy_pct": 42}'
        got = c.disk_probe()
        self.assertEqual(got["busy_pct"], 42)
        self.assertIsInstance(got["ts"], (int, float))

    @unittest.skipUnless(os.name != "nt", "non-Windows returns None without any fake")
    def test_none_without_signal_non_windows(self) -> None:
        c = self.load()
        self.assertIsNone(c.disk_probe())

    def test_none_with_only_fake_probe(self) -> None:
        c = self.load()
        os.environ["NUCLEOS_HEAVY_FAKE_PROBE"] = probe_json()
        self.assertIsNone(c.disk_probe())

    @unittest.skipUnless(os.name == "nt", "the sampling path is Windows-only")
    def test_cache_reused_when_fresh(self) -> None:
        c = self.load()
        tmp = Path(tempfile.mkdtemp(prefix="heavy-disk-cache-"))
        self.addCleanup(shutil.rmtree, tmp, True)
        cache = tmp / "disk.json"
        cache.write_text(json.dumps({"busy_pct": 88, "ts": 1000.0}), encoding="utf-8")
        sampler = mock.Mock(return_value={"busy_pct": 10, "ts": 1010.0})
        with mock.patch.object(c, "_win_disk_sample", sampler):
            got = c.disk_probe(tmp, [], now=1001.0)
            self.assertEqual(got["busy_pct"], 88)
            sampler.assert_not_called()
            got = c.disk_probe(tmp, [], now=1010.0)
            self.assertEqual(got["busy_pct"], 10)
            sampler.assert_called_once()
            self.assertEqual(json.loads(cache.read_text(encoding="utf-8"))["busy_pct"], 10)
            cache.write_text("{not json", encoding="utf-8")
            sampler.reset_mock()
            got = c.disk_probe(tmp, [], now=1011.0)
            sampler.assert_called_once()
            self.assertEqual(got["busy_pct"], 10)


class MeasureFields(_CostCase):
    def test_measure_fields_has_write_mb(self) -> None:
        c = self.load()
        got = c.measure_fields({}, 0, None)
        self.assertIn("write_mb", got)
        self.assertIsNone(got["write_mb"])
        self.assertIsNone(c.job_write_mb(None))


class Broker(unittest.TestCase):
    def setUp(self) -> None:
        self.tmp = Path(tempfile.mkdtemp(prefix="heavy-disk-"))
        self.state = self.tmp / "state"
        self.d = self.tmp / "marks"
        self.d.mkdir()
        self.procs: list[subprocess.Popen] = []
        (self.tmp / "sleeper.py").write_text(SLEEPER, encoding="utf-8")
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
                p.wait()
        shutil.rmtree(self.tmp, True)

    def env(self, **extra: str) -> dict:
        env = dict(os.environ)
        for k in list(env):
            if k.startswith("NUCLEOS_HEAVY"):
                env.pop(k)
        env.update(
            NUCLEOS_HEAVY_DIR=str(self.state),
            NUCLEOS_HEAVY_POLL_S="0.1",
            NUCLEOS_HEAVY_TARGET="0",
            # A tiny settle window and a roomy probe: the only reason B can be held is the disk.
            NUCLEOS_HEAVY_SETTLE_S="0.1",
            NUCLEOS_HEAVY_FAKE_PROBE=probe_json(),
            SLEEPER_DIR=str(self.d),
        )
        env.update(extra)
        return env

    def broker(self, tag: str, **env_kw: str) -> subprocess.Popen:
        args = [sys.executable, str(HEAVY), "--", str(self.cargo), "check", tag]
        p = subprocess.Popen(args, env=self.env(**env_kw), cwd=str(ROOT),
                             stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        self.procs.append(p)
        return p

    def started(self, tag: str) -> bool:
        return (self.d / f"{tag}.start").exists()

    def queue_entries(self) -> list[Path]:
        q = self.state / "queue"
        return sorted(q.iterdir()) if q.exists() else []

    def test_disk_hold_then_off(self) -> None:
        a = self.broker("a")
        self.assertTrue(wait_for(lambda: self.started("a")), "a never started")

        # Round 1: saturated disk with A held -> B stays queued.
        disk = json.dumps({"busy_pct": 97})
        b = self.broker("b", NUCLEOS_HEAVY_FAKE_DISK=disk)
        self.assertTrue(wait_for(lambda: bool(self.queue_entries()), timeout=10),
                        "b never queued")
        time.sleep(3.0)
        self.assertFalse(self.started("b"), "b was admitted on a saturated disk")
        self.assertTrue(self.queue_entries())
        b.kill()
        b.wait()

        # Round 2: the gate switched off -> C is admitted while A still runs.
        c = self.broker("c", NUCLEOS_HEAVY_FAKE_DISK=disk, NUCLEOS_HEAVY_DISK_BUSY_PCT="0")
        self.assertTrue(wait_for(lambda: self.started("c")), "c held although gate is off")
        self.assertIsNone(a.poll(), "a ended early")

        (self.d / "all.stop").write_text("x")
        for p in (a, c):
            p.communicate(timeout=30)


def load_heavy():
    """scripts/heavy.py as a module (importing it has no side effects: constants and defs only)."""
    spec = importlib.util.spec_from_file_location("heavy_disk_under_test", HEAVY)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


class HeavyHelpers(unittest.TestCase):
    def setUp(self) -> None:
        patcher = mock.patch.dict(os.environ, {}, clear=False)
        patcher.start()
        self.addCleanup(patcher.stop)
        for k in CLEARED:
            os.environ.pop(k, None)
        self.h = load_heavy()

    def test_disk_sample_skipped_when_nothing_held(self) -> None:
        with tempfile.TemporaryDirectory() as t:
            tmp = Path(t)
            # No held/ at all: nothing held, no sample.
            hc = mock.Mock()
            self.assertIsNone(self.h._disk_sample(hc, tmp))
            hc.disk_probe.assert_not_called()
            # An empty held/: same.
            (tmp / "held").mkdir()
            self.assertIsNone(self.h._disk_sample(hc, tmp))
            hc.disk_probe.assert_not_called()
            # Something held: sampled once, and the sample comes back.
            (tmp / "held" / "123").write_text("{}", encoding="utf-8")
            sample = {"busy_pct": 97}
            hc = mock.Mock()
            hc.disk_probe.return_value = sample
            self.assertIs(self.h._disk_sample(hc, tmp), sample)
            hc.disk_probe.assert_called_once()
            # A failing probe is no signal.
            hc = mock.Mock()
            hc.disk_probe.side_effect = RuntimeError("boom")
            self.assertIsNone(self.h._disk_sample(hc, tmp))
            hc.disk_probe.assert_called_once()

    def test_wait_line_disk_suffix(self) -> None:
        self.assertEqual(self.h._wait_line(35.4, 1, 97.2),
                         "heavy: waiting 35s, position 1 in the queue; disk busy 97%\n")
        line = self.h._wait_line(35, 2, None)
        self.assertEqual(line, "heavy: waiting 35s, position 2 in the queue\n")
        self.assertNotIn("disk busy", line)


class StatusDiskLine(unittest.TestCase):
    def setUp(self) -> None:
        self.tmp = Path(tempfile.mkdtemp(prefix="heavy-disk-status-"))
        self.addCleanup(shutil.rmtree, self.tmp, True)

    def status(self, **extra: str) -> str:
        env = dict(os.environ)
        for k in list(env):
            if k.startswith("NUCLEOS_HEAVY") or k in CLEARED:
                env.pop(k)
        env["NUCLEOS_HEAVY_DIR"] = str(self.tmp)
        env.update(extra)
        p = subprocess.run([sys.executable, str(HEAVY), "status"], env=env, cwd=str(ROOT),
                           capture_output=True, text=True, timeout=30)
        self.assertEqual(p.returncode, 0, p.stderr)
        return p.stdout

    def disk_line(self, out: str) -> str:
        lines = [ln for ln in out.splitlines() if ln.startswith("disk:")]
        self.assertEqual(len(lines), 1, out)
        return lines[0]

    def write_sample(self) -> None:
        (self.tmp / "disk.json").write_text(
            json.dumps({"busy_pct": 97, "queue": 1.2, "ts": time.time()}), encoding="utf-8")

    def test_status_disk_line(self) -> None:
        # No cached sample.
        line = self.disk_line(self.status())
        self.assertTrue(line.startswith("disk: no sample"), line)

        # A cached sample, default threshold.
        self.write_sample()
        line = self.disk_line(self.status())
        self.assertTrue(line.startswith("disk: busy 97%"), line)
        self.assertIn("threshold 90%", line)
        self.assertIn("(gating on)", line)

        # The gate switched off.
        line = self.disk_line(self.status(NUCLEOS_HEAVY_DISK_BUSY_PCT="0"))
        self.assertTrue(line.startswith("disk: busy 97%"), line)
        self.assertIn("threshold 0% (gating off)", line)


if __name__ == "__main__":
    unittest.main()
