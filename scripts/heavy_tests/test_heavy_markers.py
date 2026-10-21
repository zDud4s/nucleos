#!/usr/bin/env python3
"""RED broker-level tests for log markers, `status` and the nested-token guard (plan
2026-10-21-broker-followups, AC6-AC8).

  AC6  every run row says how it was admitted: `token` (it held a held/<pid> token), `nested`
       (it ran under a token holder's NUCLEOS_HEAVY_TOKEN), `in_held` (under hold-worktree), and
       carries `end`, the wall-clock time it finished (`ts` is its start).
  AC7  `heavy.py status` lists the light lane: `light: <n>` and one line per live light/<pid>.
  AC8  a light-lane run holds no token, so its child must NOT be handed NUCLEOS_HEAVY_TOKEN: a
       heavy.py call made inside it has to queue for a token like any other. A token holder's
       child still gets NUCLEOS_HEAVY_TOKEN=<broker pid>.

Black-box: `scripts/heavy.py` runs as a subprocess against a throwaway NUCLEOS_HEAVY_DIR.
"Machine full" is a live sleeper with a `held/<pid>` record of weight 4 under capacity 4.

    python scripts/heavy_tests/test_heavy_markers.py -v
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
from datetime import datetime
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
HEAVY = ROOT / "scripts" / "heavy.py"
sys.path.insert(0, str(ROOT / "scripts"))

import heavy_cost  # noqa: E402

PASS_ARGV = [sys.executable, "-c", "pass"]
SLEEPER = [sys.executable, "-c", "import time; time.sleep(120)"]
GOOD = {"n": 10, "mem_mb": 500.0, "run_s": 20.0, "cores": 1.0, "light": True}
PROBE = {"logical": 12, "total_mb": 16000, "avail_mb": 8000, "busy_cores": 2}
TS_FORMAT = "%Y-%m-%dT%H:%M:%S%z"

# The command a light-lane run executes: it calls the broker once more, for a DIFFERENT class
# (INNER_PY, a script), and exits with that call's code.
OUTER_CODE = (
    "import os, subprocess, sys; "
    "r = subprocess.run([sys.executable, os.environ['T_HEAVY'], '--wait-max', '2', '--', "
    "sys.executable, os.environ['T_INNER']], env=os.environ.copy()); "
    "sys.exit(r.returncode)")
OUTER_ARGV = [sys.executable, "-c", OUTER_CODE]
TOKEN_ARGV = [sys.executable, "-c",
              "import os; print('TOK=' + os.environ.get('NUCLEOS_HEAVY_TOKEN', ''))"]


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
        self.tmp = tempfile.mkdtemp(prefix="heavy-markers-")
        self.state = Path(self.tmp) / "state"
        self.state.mkdir(parents=True, exist_ok=True)
        self.addCleanup(shutil.rmtree, self.tmp, True)

    # -- fixtures ---------------------------------------------------------------------------

    def sleeper(self) -> subprocess.Popen:
        p = subprocess.Popen(SLEEPER)
        self.addCleanup(p.wait)
        self.addCleanup(p.kill)
        return p

    def hold_full(self) -> subprocess.Popen:
        """The whole capacity (4) held by a live process."""
        p = self.sleeper()
        held = self.state / "held"
        held.mkdir(parents=True, exist_ok=True)
        (held / str(p.pid)).write_text(
            json.dumps({"pid": p.pid, "weight": 4, "cls": "other|-|-|a|c"}), encoding="utf-8")
        return p

    def light_holder(self, pid: int, cls: str) -> None:
        d = self.state / "light"
        d.mkdir(parents=True, exist_ok=True)
        (d / str(pid)).write_text(json.dumps({"pid": pid, "cls": cls}), encoding="utf-8")

    def write_light(self, argv: list[str] = PASS_ARGV, **stats) -> None:
        entry = dict(GOOD)
        entry.update(stats)
        (self.state / "light.json").write_text(json.dumps({
            "v": 1, "built": time.strftime(TS_FORMAT),
            "classes": {cls_of(argv): entry}}), encoding="utf-8")

    def env(self, **extra: str) -> dict:
        return clean_env(
            str(self.state), NUCLEOS_HEAVY_CAPACITY="4", NUCLEOS_HEAVY_POLL_S="0.1",
            NUCLEOS_HEAVY_LIGHT_RETRY_S="0.2", NUCLEOS_HEAVY_FAKE_PROBE=json.dumps(PROBE),
            **extra)

    def run_broker(self, argv: list[str], env: dict, wait_max: str = "10",
                   timeout: float = 90) -> subprocess.CompletedProcess:
        return subprocess.run(
            [sys.executable, str(HEAVY), "--wait-max", wait_max, "--", *argv], env=env,
            capture_output=True, text=True, timeout=timeout, cwd=str(ROOT))

    def all_rows(self) -> list[dict]:
        return read_jsonl(self.state / "log.jsonl")

    def log_rows(self) -> list[dict]:
        return [r for r in self.all_rows() if r.get("cls")]

    def status(self) -> str:
        cp = subprocess.run([sys.executable, str(HEAVY), "status"], env=self.env(),
                            capture_output=True, text=True, timeout=60, cwd=str(ROOT))
        self.assertEqual(cp.returncode, 0, cp.stdout + cp.stderr)
        return cp.stdout


class Markers(Base):
    def last_row(self, argv: list[str], **env_extra: str) -> dict:
        before = len(self.log_rows())
        cp = self.run_broker(argv, self.env(**env_extra))
        self.assertEqual(cp.returncode, 0, cp.stdout + cp.stderr)
        rows = self.log_rows()
        self.assertEqual(len(rows), before + 1, rows)
        return rows[-1]

    def assertMarkers(self, row: dict, token: bool, nested: bool, in_held: bool) -> None:
        for key in ("token", "nested", "in_held", "end"):
            self.assertIn(key, row, f"row lacks {key!r}: {row}")
        self.assertIs(row["token"], token, row)
        self.assertIs(row["nested"], nested, row)
        self.assertIs(row["in_held"], in_held, row)
        self.assertIsInstance(row["end"], str, row)
        self.assertGreaterEqual(
            datetime.strptime(row["end"], TS_FORMAT), datetime.strptime(row["ts"], TS_FORMAT), row)

    def test_markers_on_token_nested_in_held_and_light_rows(self) -> None:
        with self.subTest("a plain run holds a token"):
            self.assertMarkers(self.last_row(PASS_ARGV), token=True, nested=False, in_held=False)
        with self.subTest("a run under a token holder is nested and holds none"):
            row = self.last_row(PASS_ARGV, NUCLEOS_HEAVY_TOKEN="1")
            self.assertMarkers(row, token=False, nested=True, in_held=False)
        with self.subTest("a run inside hold-worktree is marked in_held (and still takes a token)"):
            row = self.last_row(PASS_ARGV, NUCLEOS_HEAVY_HELD=str(os.getpid()))
            self.assertMarkers(row, token=True, nested=False, in_held=True)
        with self.subTest("a light-lane run holds no token"):
            self.hold_full()
            self.write_light()
            row = self.last_row(PASS_ARGV)
            self.assertEqual(row.get("lane"), "light", row)
            self.assertMarkers(row, token=False, nested=False, in_held=False)


class Status(Base):
    def test_status_lists_light_entries(self) -> None:
        holder = self.hold_full()
        with self.subTest("no light runs: the section is there and empty"):
            out = self.status()
            self.assertRegex(out, r"(?m)^light: 0\b", out)
        with self.subTest("two live light runs are listed with pid, class and age"):
            a, b = self.sleeper(), self.sleeper()
            cls_a, cls_b = "node|-|-|a|c", "python|x.py|-|a|c"
            self.light_holder(a.pid, cls_a)
            self.light_holder(b.pid, cls_b)
            out = self.status()
            self.assertRegex(out, r"(?m)^light: 2\b", out)
            for pid, cls in ((a.pid, cls_a), (b.pid, cls_b)):
                self.assertRegex(out, rf"(?m)^\s+pid={pid} cls={re.escape(cls)} age=\d+s\s*$", out)
            # After the held section and before the worktree locks.
            self.assertGreater(out.index("light:"), out.index(f"pid={holder.pid}"), out)
            self.assertLess(out.index("light:"), out.index("worktree locks:"), out)


class NestedGuard(Base):
    def test_nested_call_in_light_run_takes_a_token(self) -> None:
        inner = Path(self.tmp) / "inner.py"
        inner.write_text("pass\n", encoding="utf-8")
        inner_argv = [sys.executable, str(inner)]
        self.assertNotEqual(cls_of(inner_argv), cls_of(OUTER_ARGV))
        with self.subTest("a heavy.py call inside a light-lane run queues for a token"):
            self.hold_full()  # the machine is full: only the light lane admits the outer run
            self.write_light(argv=OUTER_ARGV)  # ... and only the outer class is light
            cp = self.run_broker(
                OUTER_ARGV, self.env(T_HEAVY=str(HEAVY), T_INNER=str(inner)), wait_max="20")
            outer = [r for r in self.log_rows() if r.get("lane") == "light"]
            self.assertEqual(len(outer), 1, self.all_rows())
            self.assertEqual(
                cp.returncode, 75,
                "the nested call ran under a token the light run never held: "
                f"{cp.stdout}{cp.stderr}")
            timeouts = [r for r in self.all_rows() if r.get("waited_on") == "token"]
            self.assertEqual(len(timeouts), 1, self.all_rows())
        with self.subTest("a token holder's child still gets NUCLEOS_HEAVY_TOKEN=<broker pid>"):
            shutil.rmtree(self.state, True)
            self.state.mkdir(parents=True)
            p = subprocess.Popen(
                [sys.executable, str(HEAVY), "--wait-max", "10", "--", *TOKEN_ARGV],
                env=self.env(), stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
                cwd=str(ROOT))
            self.addCleanup(p.wait)
            self.addCleanup(p.kill)
            out, err = p.communicate(timeout=60)
            self.assertEqual(p.returncode, 0, out + err)
            self.assertIn(f"TOK={p.pid}", out)


if __name__ == "__main__":
    unittest.main()
