#!/usr/bin/env python3
"""RED broker-level tests for the test-phase weight drop (plan 2026-10-21-broker-followups, AC1-AC5).

A token-holding `cargo test` is admitted at weight 2 (it may have to compile). Once its output
shows a `Running ...` header the compile is over and only the test binary runs, so the broker
lowers `held/<pid>` to weight 1 while the command is still alive and logs `weight_drop_s`.

Black-box: `scripts/heavy.py` runs as a subprocess against a throwaway NUCLEOS_HEAVY_DIR; the
"cargo" is a shim (cargo.cmd on Windows, an sh script elsewhere) that runs a fake_cargo.py
written into the temp dir. Its behaviour comes from the environment:

    FAKECARGO_PRE      seconds to sleep before anything is printed (the "compile")
    FAKECARGO_RUNNING  the `Running ...` line to print (empty: print none)
    FAKECARGO_POST     seconds to sleep afterwards (the "tests")
    FAKECARGO_STOP     optional path: the post-sleep ends early once that file exists

    python scripts/heavy_tests/test_heavy_phase_drop.py -v
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

PROBE = {"logical": 12, "total_mb": 16000, "avail_mb": 8000, "busy_cores": 2}
# Matches heavy._RUNNING_BIN: `Running ... (<path>/<name>-<16 hex>[.exe])`.
RUNNING_LINE = ("     Running unittests src/main.rs "
                "(target/debug/deps/fakecrate-0123456789abcdef.exe)")

FAKE_CARGO = '''\
import os
import sys
import time

time.sleep(float(os.environ.get("FAKECARGO_PRE") or 0))
running = os.environ.get("FAKECARGO_RUNNING") or ""
if running:
    print(running, flush=True)
    print("running 1 test", flush=True)
stop = os.environ.get("FAKECARGO_STOP") or ""
end = time.time() + float(os.environ.get("FAKECARGO_POST") or 0)
while time.time() < end and not (stop and os.path.exists(stop)):
    time.sleep(0.05)
print("test result: ok. 1 passed; 0 failed", flush=True)
sys.exit(0)
'''


def clean_env(state_dir: str, **extra: str) -> dict:
    env = dict(os.environ)
    for k in list(env):
        if k.startswith("NUCLEOS_HEAVY") or k.startswith("FAKECARGO"):
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
        self.tmp = tempfile.mkdtemp(prefix="heavy-drop-")
        self.state = Path(self.tmp) / "state"
        self.state.mkdir(parents=True, exist_ok=True)
        self.work = Path(self.tmp) / "work"  # not a git repository: no crate, no worktree lock
        self.work.mkdir()
        self.bin = Path(self.tmp) / "bin"
        self.bin.mkdir()
        self.addCleanup(shutil.rmtree, self.tmp, True)
        script = self.bin / "fake_cargo.py"
        script.write_text(FAKE_CARGO, encoding="utf-8")
        if os.name == "nt":
            self.cargo = self.bin / "cargo.cmd"
            self.cargo.write_text(
                f'@echo off\r\n"{sys.executable}" "{script}" %*\r\n', encoding="utf-8")
        else:
            self.cargo = self.bin / "cargo"
            self.cargo.write_text(
                f'#!/bin/sh\nexec "{sys.executable}" "{script}" "$@"\n', encoding="utf-8")
            self.cargo.chmod(0o755)
        self.argv = [str(self.cargo), "test"]

    # -- helpers ----------------------------------------------------------------------------

    def env(self, capacity: int = 4, **extra: str) -> dict:
        return clean_env(
            str(self.state), NUCLEOS_HEAVY_CAPACITY=str(capacity), NUCLEOS_HEAVY_POLL_S="0.1",
            NUCLEOS_HEAVY_TARGET="0", NUCLEOS_HEAVY_FAKE_PROBE=json.dumps(PROBE), **extra)

    def start(self, env: dict, wait_max: str = "60") -> subprocess.Popen:
        p = subprocess.Popen(
            [sys.executable, str(HEAVY), "--wait-max", wait_max, "--", *self.argv], env=env,
            stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, cwd=str(self.work))
        self.addCleanup(p.wait)
        self.addCleanup(p.kill)
        return p

    def held_weight(self, pid: int) -> int | None:
        try:
            return int(json.loads(
                (self.state / "held" / str(pid)).read_text(encoding="utf-8"))["weight"])
        except (OSError, ValueError, KeyError, TypeError):
            return None

    def sample(self, p: subprocess.Popen, limit: float = 90) -> list[int]:
        """The held weight of `p`, read repeatedly for as long as it is alive."""
        seen: list[int] = []
        end = time.time() + limit
        while p.poll() is None and time.time() < end:
            w = self.held_weight(p.pid)
            if w is not None:
                seen.append(w)
            time.sleep(0.05)
        return seen

    def log_rows(self) -> list[dict]:
        return [r for r in read_jsonl(self.state / "log.jsonl") if r.get("cls")]

    def run_observed(self, running: str, pre: float = 1.0, post: float = 3.0,
                     capacity: int = 4, **extra: str) -> tuple[list[int], dict]:
        """Run the shim `cargo test` under the broker, sampling its held weight. Returns the
        weights seen while it was alive and its log row (checked to be an admission-weight-2
        cargo run, so the classification this relies on is confirmed)."""
        before = len(self.log_rows())
        env = self.env(capacity, FAKECARGO_PRE=str(pre), FAKECARGO_POST=str(post),
                       FAKECARGO_RUNNING=running, **extra)
        p = self.start(env)
        seen = self.sample(p)
        out, err = p.communicate(timeout=60)
        self.assertEqual(p.returncode, 0, out + err)
        rows = self.log_rows()
        self.assertEqual(len(rows), before + 1, rows)
        row = rows[-1]
        self.assertEqual(row["weight"], 2, f"the shim run did not classify as weight 2: {row}")
        self.assertTrue(seen, "held/<pid> was never readable while the command ran")
        return seen, row


class PhaseDrop(Base):
    def test_drop_on_running_line(self) -> None:
        seen, row = self.run_observed(RUNNING_LINE, pre=2.0, post=4.0)
        # Admitted at weight 2 and visibly so while the "compile" runs ...
        self.assertEqual(seen[0], 2, seen)
        # ... then lowered to 1 while the command is still alive (samples stop at its exit).
        self.assertIn(1, seen, f"held weight never dropped after the Running line: {seen}")
        self.assertGreater(seen.index(1), seen.index(2), seen)
        # The log keeps the admission weight and adds when the drop happened.
        self.assertEqual(row["weight"], 2)
        self.assertIn("weight_drop_s", row, row)
        self.assertIsInstance(row["weight_drop_s"], float)
        self.assertGreater(row["weight_drop_s"], 0)
        self.assertLess(row["weight_drop_s"], row["run_s"])

    def test_no_drop_without_running_line(self) -> None:
        with self.subTest("control: the same run WITH a Running line drops"):
            seen, row = self.run_observed(RUNNING_LINE)
            self.assertIn(1, seen, f"control run never dropped: {seen}")
            self.assertIn("weight_drop_s", row, row)
        with self.subTest("no Running / Doc-tests line: the weight stays 2"):
            seen, row = self.run_observed("")
            self.assertEqual(set(seen), {2}, seen)
            self.assertNotIn("weight_drop_s", row, row)
            self.assertEqual(row["weight"], 2)

    def test_disabled_by_env(self) -> None:
        with self.subTest("control: switch unset, the run drops"):
            seen, row = self.run_observed(RUNNING_LINE)
            self.assertIn(1, seen, f"control run never dropped: {seen}")
            self.assertIn("weight_drop_s", row, row)
        with self.subTest("NUCLEOS_HEAVY_PHASE_DROP=0: the weight stays 2"):
            seen, row = self.run_observed(RUNNING_LINE, NUCLEOS_HEAVY_PHASE_DROP="0")
            self.assertEqual(set(seen), {2}, seen)
            self.assertNotIn("weight_drop_s", row, row)
            self.assertEqual(row["weight"], 2)

    def test_queued_request_admitted_after_drop(self) -> None:
        # Capacity 3: one weight-2 run leaves no room for a second (2 + 2 > 3), but once the
        # first has dropped to 1 the second fits (1 + 2 <= 3) without waiting for it to end.
        stop = Path(self.tmp) / "stop-first"
        first = self.start(self.env(
            3, FAKECARGO_PRE="3", FAKECARGO_RUNNING=RUNNING_LINE, FAKECARGO_POST="60",
            FAKECARGO_STOP=str(stop)))
        deadline = time.time() + 30
        while time.time() < deadline and self.held_weight(first.pid) is None:
            self.assertIsNone(first.poll(), "the first run ended before it was admitted")
            time.sleep(0.05)
        self.assertEqual(self.held_weight(first.pid), 2)
        second = self.start(self.env(
            3, FAKECARGO_PRE="0", FAKECARGO_RUNNING="", FAKECARGO_POST="0"), wait_max="30")
        # It queues behind the first run (the first is still in its compile phase).
        queued = False
        while time.time() < deadline and second.poll() is None:
            if any(f.name.endswith(f"-{second.pid}") for f in (self.state / "queue").glob("*")):
                queued = True
                break
            time.sleep(0.05)
        self.assertTrue(queued, "the second request never queued")
        try:
            out, err = second.communicate(timeout=45)
            self.assertEqual(
                second.returncode, 0,
                f"the queued request was not admitted after the drop: {out}{err}")
            self.assertIsNone(first.poll(), "the first run ended before the second was admitted")
        finally:
            stop.write_text("go", encoding="utf-8")  # let the first run finish
        out1, err1 = first.communicate(timeout=60)
        self.assertEqual(first.returncode, 0, out1 + err1)
        rows = self.log_rows()
        self.assertEqual(len(rows), 2, rows)
        # The second one waited for the first one's drop (>= ~2 s of its 3 s compile phase);
        # the first was admitted at once.
        waited = [r for r in rows if r["wait_token_s"] > 0.5]
        self.assertEqual(len(waited), 1, rows)
        dropped = [r for r in rows if "weight_drop_s" in r]
        self.assertEqual(len(dropped), 1, rows)


if __name__ == "__main__":
    unittest.main()
