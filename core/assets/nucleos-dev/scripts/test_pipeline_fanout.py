#!/usr/bin/env python3
"""Tests for pipeline_fanout: parallel subprocess nodes with isolated failures.

    python test_pipeline_fanout.py -v
"""

from __future__ import annotations

import json
import subprocess
import sys
import time
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))

import pipeline_fanout  # noqa: E402


def py(code, **extra):
    return {"cmd": [sys.executable, "-c", code], **extra}


def cli(stdin_text):
    return subprocess.run([sys.executable, str(HERE / "pipeline_fanout.py")],
                          input=stdin_text.encode("utf-8"), capture_output=True)


class RunFanout(unittest.TestCase):
    def test_two_nodes_run_in_parallel_and_keep_order(self):
        sleeper = "import time;time.sleep(1.5);print('x')"
        spec = {"nodes": [{"id": "a", **py(sleeper)}, {"id": "b", **py(sleeper)}]}
        start = time.perf_counter()
        res = pipeline_fanout.run_fanout(spec)
        elapsed = time.perf_counter() - start
        self.assertEqual([r["id"] for r in res], ["a", "b"])
        self.assertTrue(all(r["status"] == "ok" for r in res))
        self.assertLess(elapsed, 2.9)

    def test_ok_error_timeout_statuses(self):
        spec = {"nodes": [
            {"id": "ok", **py("print('fine')")},
            {"id": "bad", **py("import sys;sys.stderr.write('boom');sys.exit(3)")},
            {"id": "slow", **py("import time;time.sleep(30)", timeout=1)},
            {"id": "missing", "cmd": ["definitely-not-a-real-binary-xyz"]},
        ]}
        res = {r["id"]: r for r in pipeline_fanout.run_fanout(spec)}
        self.assertEqual(res["ok"]["status"], "ok")
        self.assertEqual(res["ok"]["exit_code"], 0)
        self.assertIn("fine", res["ok"]["stdout"])
        self.assertEqual(res["bad"]["status"], "error")
        self.assertEqual(res["bad"]["exit_code"], 3)
        self.assertIn("boom", res["bad"]["stderr"])
        self.assertEqual(res["slow"]["status"], "timeout")
        self.assertIsNone(res["slow"]["exit_code"])
        self.assertEqual(res["missing"]["status"], "error")
        self.assertTrue(res["missing"]["error"])

    def test_utf8_stdin_round_trip(self):
        code = ("import sys;d=sys.stdin.buffer.read();"
                "sys.stdout.buffer.write(d)")
        text = "ação – 日本語 ✓"
        res = pipeline_fanout.run_fanout({"nodes": [{"id": "u", **py(code, stdin=text)}]})
        self.assertEqual(res[0]["status"], "ok")
        self.assertEqual(res[0]["stdout"], text)

    def test_max_parallel_one_serialises(self):
        sleeper = "import time;time.sleep(1)"
        spec = {"max_parallel": 1,
                "nodes": [{"id": "a", **py(sleeper)}, {"id": "b", **py(sleeper)}]}
        start = time.perf_counter()
        pipeline_fanout.run_fanout(spec)
        self.assertGreaterEqual(time.perf_counter() - start, 1.9)


class BadSpec(unittest.TestCase):
    def test_rejected_specs(self):
        bad = [
            [],
            {},
            {"nodes": []},
            {"nodes": "x"},
            {"nodes": [{"id": "a"}]},
            {"nodes": [{"cmd": ["x"]}]},
            {"nodes": [{"id": "a", "cmd": "x"}]},
            {"nodes": [{"id": "a", "cmd": ["x"]}, {"id": "a", "cmd": ["x"]}]},
            {"nodes": [{"id": "a", "cmd": ["x"], "timeout": 0}]},
            {"nodes": [{"id": "a", "cmd": ["x"], "stdin": 5}]},
            {"max_parallel": True, "nodes": [{"id": "a", "cmd": ["x"]}]},
        ]
        for spec in bad:
            with self.assertRaises(ValueError, msg=repr(spec)):
                pipeline_fanout.run_fanout(spec)

    def test_cli_bad_spec_exits_nonzero(self):
        for text in ("not json", "{}", '{"nodes": []}'):
            res = cli(text)
            self.assertNotEqual(res.returncode, 0, text)
            self.assertIn("pipeline_fanout error", res.stderr.decode("utf-8"))

    def test_cli_success_prints_json_list(self):
        spec = {"nodes": [{"id": "a", **py("print('hi')")}]}
        res = cli(json.dumps(spec))
        self.assertEqual(res.returncode, 0, res.stderr)
        out = json.loads(res.stdout.decode("utf-8"))
        self.assertEqual(out[0]["id"], "a")
        self.assertEqual(out[0]["status"], "ok")


if __name__ == "__main__":
    unittest.main()
