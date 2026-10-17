#!/usr/bin/env python3
"""Regression tests for broker races and false denials: a stale mutex broken by two waiters,
prepare_slot wiping a slot over a transient failure, the subagent deny matching paths and
prose, and a backslash-newline continuation hiding a heavy command's arguments."""

from __future__ import annotations

import importlib.util
import json
import subprocess
import sys
import tempfile
import threading
import time
import unittest
from pathlib import Path
from unittest import mock

ROOT = Path(__file__).resolve().parents[2]


def _load(name, rel):
    spec = importlib.util.spec_from_file_location(name, ROOT / rel)
    mod = importlib.util.module_from_spec(spec)
    sys.modules.setdefault(name, mod)
    spec.loader.exec_module(mod)
    return mod


heavy = _load("heavy", "scripts/heavy.py")
guard = _load("heavy_guard", "scripts/heavy_guard.py")
classify = _load("heavy_classify", "scripts/heavy_classify.py")

DEAD = {"pid": 2 ** 22 + 1, "ctime": 1}


class StaleMutexBreakTest(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.dir = Path(self.tmp.name)

    def tearDown(self):
        self.tmp.cleanup()

    def leave_dead(self):
        (self.dir / ".mutex").mkdir()
        (self.dir / ".mutex" / "owner").write_text(json.dumps(DEAD))

    def test_a_second_breaker_does_not_delete_the_first_ones_fresh_mutex(self):
        # Two waiters both judged the mutex stale. The first breaks it and takes a fresh one;
        # the second, acting on its old verdict, must not delete THAT.
        self.leave_dead()
        first = heavy.Mutex(self.dir, timeout=5)
        first.acquire()
        second = heavy.Mutex(self.dir, timeout=5)
        second._break_stale()
        self.assertTrue((self.dir / ".mutex").exists(), "a live mutex was deleted")
        first.release()

    def test_concurrent_waiters_never_both_hold(self):
        self.leave_dead()
        lock = threading.Lock()
        inside = [0]
        overlap = []

        def worker():
            with heavy.Mutex(self.dir, timeout=30):
                with lock:
                    inside[0] += 1
                    if inside[0] > 1:
                        overlap.append(1)
                time.sleep(0.05)
                with lock:
                    inside[0] -= 1

        ts = [threading.Thread(target=worker) for _ in range(6)]
        for t in ts:
            t.start()
        for t in ts:
            t.join()
        self.assertEqual(overlap, [])


class PrepareSlotTest(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.dir = Path(self.tmp.name)
        self.slot = self.dir / "slot"
        (self.slot / "debug" / ".fingerprint").mkdir(parents=True)
        (self.slot / "debug" / "deps").mkdir()
        self.lease = (0, str(self.slot), str(self.dir / "other-wt"))

    def tearDown(self):
        self.tmp.cleanup()

    def run_prepare(self, fake):
        with mock.patch.object(heavy.subprocess, "run", side_effect=fake), \
                mock.patch.object(heavy, "registry_forget"), \
                mock.patch.object(heavy, "_progress_step"):
            heavy.prepare_slot(self.dir, self.lease, str(self.dir / "wt"), "prog", str(self.dir))

    @staticmethod
    def meta(cmd):
        data = {"workspace_members": ["a"], "packages": [{"id": "a", "name": "a"}]}
        return subprocess.CompletedProcess(cmd, 0, json.dumps(data).encode(), b"")

    def test_a_failing_metadata_call_does_not_remove_the_slot(self):
        def fake(cmd, **kw):
            raise subprocess.TimeoutExpired(cmd, 1)
        self.run_prepare(fake)
        self.assertTrue(self.slot.exists(), "slot wiped over a metadata failure")
        self.assertTrue((self.slot / "debug" / "deps").exists())
        self.assertFalse((self.slot / "debug" / ".fingerprint").exists())

    def test_a_transient_clean_failure_is_retried(self):
        calls = []

        def fake(cmd, **kw):
            if cmd[1] == "metadata":
                return self.meta(cmd)
            calls.append(cmd)
            return subprocess.CompletedProcess(cmd, 1 if len(calls) == 1 else 0, b"", b"")
        self.run_prepare(fake)
        self.assertEqual(len(calls), 2)
        self.assertTrue(self.slot.exists())

    def test_a_clean_that_keeps_failing_removes_the_slot(self):
        def fake(cmd, **kw):
            if cmd[1] == "metadata":
                return self.meta(cmd)
            return subprocess.CompletedProcess(cmd, 1, b"", b"")
        self.run_prepare(fake)
        self.assertFalse(self.slot.exists())


class SubagentDenyTest(unittest.TestCase):
    def denied(self, cmd):
        out = guard.decide({"tool_name": "Bash", "agent_id": "a1",
                            "tool_input": {"command": cmd}}, None, False, "python")
        return bool(out) and "owner rule" in json.dumps(out)

    def test_paths_and_prose_are_not_a_program(self):
        for cmd in ("cat .cargo/config.toml",
                    "git commit -m 'bump cargo deps'",
                    'git commit -m "fix: cargo gate message"',
                    "grep -rn cargo docs/",
                    "ls ~/.cargo/bin",
                    "echo see cargo.toml"):
            with self.subTest(cmd=cmd):
                self.assertFalse(self.denied(cmd))

    def test_the_program_is_still_denied(self):
        for cmd in ("cargo test", "cd x && cargo build", "FOO=1 cargo check", "time cargo fmt",
                    "sh -c 'cargo test'", "C:/Projects/cargo/bin/cargo.exe test",
                    "x | cargo fmt", "echo 'unterminated cargo test"):
            with self.subTest(cmd=cmd):
                self.assertTrue(self.denied(cmd))


class ContinuationTest(unittest.TestCase):
    def test_a_backslash_newline_continuation_joins_the_words(self):
        toks = classify.shell_tokens("go test \\\n ./... -run X", True)
        self.assertEqual(toks, ["go", "test", "./...", "-run", "X"])
        self.assertTrue(classify.classify(toks).heavy)


if __name__ == "__main__":
    unittest.main()
