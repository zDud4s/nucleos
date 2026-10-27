#!/usr/bin/env python3
"""RED tests for the heavy broker's `dynamic` admission (the default mode): the head of the
queue is admitted while fewer than capacity() jobs are held AND (nothing is held, or the live
probe shows room: free RAM, idle cores, and a settle window since the last admission). An
unreadable probe falls back to the static weight sum.

Black-box: `scripts/heavy.py` runs as a subprocess against a throwaway NUCLEOS_HEAVY_DIR with
NO NUCLEOS_HEAVY_CAPACITY (that switch forces static mode); the live probe is injected with
NUCLEOS_HEAVY_FAKE_PROBE. The "heavy commands" are a fake `cargo` (weight 2 for `test`, 1 for
`check`) that runs a sleeper waiting on a stop file; nothing real is compiled. The pure
`dynamic_fit` is tested by importing scripts/heavy_cost.py.

    python -m pytest -q scripts/heavy_tests/test_heavy_dynamic_admission.py
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


def read_jsonl(path: Path) -> list[dict]:
    if not path.exists():
        return []
    return [json.loads(line) for line in path.read_text(encoding="utf-8").splitlines()
            if line.strip()]


class Broker(unittest.TestCase):
    def setUp(self) -> None:
        self.tmp = Path(tempfile.mkdtemp(prefix="heavy-dyn-"))
        self.state = self.tmp / "state"
        self.d = self.tmp / "marks"
        self.d.mkdir()
        self.procs: list[subprocess.Popen] = []
        self.holders: list[subprocess.Popen] = []
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
        for h in self.holders:
            h.kill()
            h.wait()
        for p in self.procs:
            try:
                p.wait(timeout=15)
            except Exception:
                p.kill()
        shutil.rmtree(self.tmp, True)

    # ---- helpers -------------------------------------------------------------------
    def env(self, probe: str | None = None, settle: str = "0.3", **extra: str) -> dict:
        env = dict(os.environ)
        for k in list(env):
            if k.startswith("NUCLEOS_HEAVY"):
                env.pop(k)
        env.update(
            NUCLEOS_HEAVY_DIR=str(self.state),
            NUCLEOS_HEAVY_POLL_S="0.1",
            # No pooled target dirs: their 3 slots per worktree would cap these tests at 3
            # jobs before the admission rule under test does.
            NUCLEOS_HEAVY_TARGET="0",
            NUCLEOS_HEAVY_SETTLE_S=settle,
            NUCLEOS_HEAVY_FAKE_PROBE=probe if probe is not None else probe_json(),
            SLEEPER_DIR=str(self.d),
        )
        env.update(extra)
        return env

    def broker(self, tag: str, sub: str = "test", wait_max: str | None = None,
               **env_kw) -> subprocess.Popen:
        args = [sys.executable, str(HEAVY)]
        if wait_max is not None:
            args += ["--wait-max", wait_max]
        args += ["--", str(self.cargo), sub, tag]
        p = subprocess.Popen(args, env=self.env(**env_kw), cwd=str(ROOT),
                             stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        self.procs.append(p)
        return p

    def started(self, tag: str) -> bool:
        return (self.d / f"{tag}.start").exists()

    def stop(self, tag: str) -> None:
        (self.d / f"{tag}.stop").write_text("x")

    def held_files(self) -> list[Path]:
        h = self.state / "held"
        return sorted(h.iterdir()) if h.exists() else []

    def assert_started(self, tag: str) -> None:
        self.assertTrue(wait_for(lambda: self.started(tag)), f"{tag} never started")

    def assert_stays_queued(self, tag: str, secs: float = 1.2) -> None:
        time.sleep(secs)
        self.assertFalse(self.started(tag), f"{tag} was admitted without room")

    def finish(self, p: subprocess.Popen) -> None:
        out, err = p.communicate(timeout=30)
        self.assertEqual(p.returncode, 0, out + err)

    def hold_foreign(self, weight: int) -> None:
        """A held record owned by a live process outside the broker (its weight is the only
        thing the static fallback can see)."""
        holder = subprocess.Popen([sys.executable, "-c", "import time; time.sleep(120)"])
        self.holders.append(holder)
        held = self.state / "held"
        held.mkdir(parents=True, exist_ok=True)
        (held / str(holder.pid)).write_text(
            json.dumps({"pid": holder.pid, "weight": weight}), encoding="utf-8")

    # ---- acceptance ----------------------------------------------------------------
    # 1
    def test_idle_machine_admits_three_weight_2_jobs(self) -> None:
        # Static would stop at two (2 + 2 = 4 of 4); an idle machine takes a third.
        ps = [self.broker(t, "test") for t in ("a", "b", "c")]
        for t in ("a", "b", "c"):
            self.assert_started(t)
        self.assertEqual(len(self.held_files()), 3)
        for t in ("a", "b", "c"):
            self.stop(t)
        for p in ps:
            self.finish(p)

    # 2
    def test_low_avail_second_job_waits_until_release(self) -> None:
        low = probe_json(avail_mb=500)  # below the 1536 MB default floor
        a = self.broker("a", "check", probe=low)
        self.assert_started("a")
        b = self.broker("b", "check", probe=low)
        self.assert_stays_queued("b")
        self.stop("a")
        self.finish(a)
        # Alone again: admitted whatever the probe says.
        self.assert_started("b")
        self.stop("b")
        self.finish(b)

    # 3
    def test_settle_window_delays_second_admission(self) -> None:
        # A broker's own start-up takes up to ~1 s under load, so the window must be wide
        # enough that "b is still waiting at 2 s" cannot be explained by start-up alone.
        a = self.broker("a", "check", settle="4")
        self.assert_started("a")
        t_a = time.time()
        b = self.broker("b", "check", settle="4")
        time.sleep(2.0)
        self.assertFalse(self.started("b"), "b admitted inside the settle window")
        self.assert_started("b")
        # Generous bound: a's admission preceded its marker by a moment, so the window is
        # measured from just before t_a.
        self.assertGreaterEqual(time.time() - t_a, 3.0)
        self.stop("a")
        self.stop("b")
        self.finish(a)
        self.finish(b)

    # 4
    def test_never_more_than_capacity_held(self) -> None:
        tags = ["a", "b", "c", "d", "e"]
        ps = [self.broker(t, "check") for t in tags]
        # The five brokers race to the queue, so which one waits is not known in advance.
        def running() -> list[str]:
            return [t for t in tags if self.started(t)]
        self.assertTrue(wait_for(lambda: len(running()) >= 4), f"only {running()} started")
        time.sleep(1.2)
        self.assertEqual(len(running()), 4, "a fifth job was admitted past the ceiling")
        self.assertLessEqual(len(self.held_files()), 4)
        (waiting,) = [t for t in tags if not self.started(t)]
        self.stop(running()[0])
        self.assert_started(waiting)
        for t in tags:
            self.stop(t)
        for p in ps:
            self.finish(p)

    # 5
    def test_unreadable_probe_falls_back_to_static_sum(self) -> None:
        broken = "not json{"
        # Weight 4 held fills the static budget: a weight-1 job waits.
        self.hold_foreign(4)
        b = self.broker("b", "check", probe=broken)
        self.assert_stays_queued("b")
        for f in self.held_files():
            f.unlink()
        for h in self.holders:
            h.kill()
            h.wait()
        self.holders.clear()
        self.assert_started("b")
        self.stop("b")
        self.finish(b)
        # Weight 2 held leaves room for a weight-2 job (2 + 2 <= 4), settle window or not.
        self.hold_foreign(2)
        c = self.broker("c", "test", probe=broken, settle="60")
        self.assert_started("c")
        self.stop("c")
        self.finish(c)

    # 6
    def test_static_mode_keeps_weight_sum(self) -> None:
        extra = {"NUCLEOS_HEAVY_ADMISSION": "static"}
        a = self.broker("a", "test", **extra)
        b = self.broker("b", "test", **extra)
        self.assert_started("a")
        self.assert_started("b")
        c = self.broker("c", "test", **extra)
        self.assert_stays_queued("c")
        self.stop("a")
        self.assert_started("c")
        for t in ("a", "b", "c"):
            self.stop(t)
        for p in (a, b, c):
            self.finish(p)

    # 7
    def test_adm_recorded_in_held_record_and_log_row(self) -> None:
        a = self.broker("a", "check")
        self.assert_started("a")
        b = self.broker("b", "check")
        self.assert_started("b")
        recs = [json.loads(f.read_text(encoding="utf-8")) for f in self.held_files()]
        adms = [r.get("adm") for r in recs]
        for adm in adms:
            self.assertIsInstance(adm, dict, recs)
            self.assertEqual(set(adm), ADM_KEYS)
            self.assertEqual(adm["mode"], "dynamic")
        self.assertEqual(sorted(a_["rule"] for a_ in adms), ["alone", "room"])
        alone = next(a_ for a_ in adms if a_["rule"] == "alone")
        room = next(a_ for a_ in adms if a_["rule"] == "room")
        self.assertEqual(alone["held_n"], 0)
        self.assertEqual(room["held_n"], 1)
        self.assertEqual(room["avail_mb"], 20000)
        self.assertEqual(room["idle_cores"], 10.0)
        self.stop("a")
        self.stop("b")
        self.finish(a)
        self.finish(b)
        rows = [r for r in read_jsonl(self.state / "log.jsonl") if r.get("adm")]
        self.assertEqual(len(rows), 2, rows)
        for r in rows:
            self.assertEqual(set(r["adm"]), ADM_KEYS)
            self.assertIn(r["adm"]["rule"], ("alone", "room", "static_fallback"))
        self.assertEqual(sorted(r["adm"]["rule"] for r in rows), ["alone", "room"])


class Pure(unittest.TestCase):
    def load(self):
        keys = ("NUCLEOS_HEAVY_CAPACITY", "NUCLEOS_HEAVY_ADMISSION",
                "NUCLEOS_HEAVY_MIN_AVAIL_MB", "NUCLEOS_HEAVY_MIN_IDLE_CORES",
                "NUCLEOS_HEAVY_SETTLE_S", "NUCLEOS_HEAVY_FAKE_PROBE")
        patcher = mock.patch.dict(os.environ, {}, clear=False)
        patcher.start()
        self.addCleanup(patcher.stop)
        for k in keys:
            os.environ.pop(k, None)
        spec = importlib.util.spec_from_file_location("heavy_cost_dyn_under_test", COST)
        mod = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(mod)
        return mod

    @staticmethod
    def probe(busy, avail=20000):
        return {"logical": 12, "total_mb": 32000, "avail_mb": avail, "busy_cores": busy}

    # 9
    def test_dynamic_fit_idle_cores_and_none_busy(self) -> None:
        c = self.load()
        # 11.5 of 12 cores busy: 0.5 idle, under the 1.0 default.
        ok, adm = c.dynamic_fit(1, self.probe(11.5), 100.0, 4, True)
        self.assertFalse(ok)
        self.assertEqual(adm["rule"], "room")
        self.assertEqual(adm["idle_cores"], 0.5)
        # 2 busy: 10 idle.
        ok, adm = c.dynamic_fit(1, self.probe(2), 100.0, 4, True)
        self.assertTrue(ok)
        self.assertEqual(adm["rule"], "room")
        self.assertEqual(adm["idle_cores"], 10.0)
        self.assertEqual(adm["held_n"], 1)
        self.assertEqual(adm["mode"], "dynamic")
        self.assertEqual(set(adm), ADM_KEYS)
        # busy_cores unknown: the CPU condition is skipped, RAM and settle still decide.
        ok, adm = c.dynamic_fit(1, self.probe(None), 100.0, 4, True)
        self.assertTrue(ok)
        self.assertIsNone(adm["idle_cores"])
        ok, _ = c.dynamic_fit(1, self.probe(None, avail=500), 100.0, 4, True)
        self.assertFalse(ok)
        ok, _ = c.dynamic_fit(1, self.probe(None), 0.0, 4, True)
        self.assertFalse(ok)
        # Nothing held: alone, whatever the probe says.
        ok, adm = c.dynamic_fit(0, self.probe(11.9, avail=10), 0.0, 4, False)
        self.assertTrue(ok)
        self.assertEqual(adm["rule"], "alone")
        # At the ceiling: never, however idle.
        ok, _ = c.dynamic_fit(4, self.probe(0), 100.0, 4, True)
        self.assertFalse(ok)
        # No probe: the static decision stands.
        ok, adm = c.dynamic_fit(1, None, 0.0, 4, True)
        self.assertTrue(ok)
        self.assertEqual(adm["rule"], "static_fallback")
        ok, adm = c.dynamic_fit(1, None, 100.0, 4, False)
        self.assertFalse(ok)
        self.assertEqual(adm["rule"], "static_fallback")


if __name__ == "__main__":
    unittest.main()
