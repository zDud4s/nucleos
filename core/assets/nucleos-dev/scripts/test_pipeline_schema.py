#!/usr/bin/env python3
"""Tests for pipeline_schema: the pipeline DAG validator and its `<name>` pre-flight CLI.

    python test_pipeline_schema.py -v
"""

from __future__ import annotations

import json
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))

import pipeline_schema  # noqa: E402

VALID_YAML = """\
nodes:
  - id: in
    kind: input
  - id: a
    agent: reviewer
    depends_on: [in]
  - id: out
    kind: synthesize
    depends_on: [a]
"""


def valid():
    return {
        "nodes": [
            {"id": "in", "kind": "input"},
            {"id": "a", "agent": "reviewer", "depends_on": ["in"]},
            {"id": "out", "kind": "synthesize", "depends_on": ["a"]},
        ]
    }


class Validate(unittest.TestCase):
    def test_valid_dag(self):
        self.assertEqual(pipeline_schema.validate(valid()), (True, []))

    def test_not_a_mapping(self):
        ok, errs = pipeline_schema.validate([])
        self.assertFalse(ok)
        self.assertTrue(errs[0].startswith("pipeline invalid:"))

    def test_missing_nodes(self):
        ok, errs = pipeline_schema.validate({})
        self.assertFalse(ok)
        self.assertIn("pipeline invalid: missing key 'nodes'", errs)

    def test_missing_input(self):
        p = valid()
        p["nodes"][0] = {"id": "in", "agent": "planner"}
        ok, errs = pipeline_schema.validate(p)
        self.assertFalse(ok)
        self.assertTrue(any("exactly one input node (found 0)" in e for e in errs))

    def test_two_sinks(self):
        p = valid()
        p["nodes"].append({"id": "out2", "kind": "collect", "depends_on": ["a"]})
        ok, errs = pipeline_schema.validate(p)
        self.assertFalse(ok)
        self.assertTrue(any("exactly one sink node (found 2)" in e for e in errs))

    def test_cycle(self):
        p = {
            "nodes": [
                {"id": "in", "kind": "input"},
                {"id": "a", "agent": "x", "depends_on": ["in", "b"]},
                {"id": "b", "agent": "y", "depends_on": ["a"]},
                {"id": "out", "kind": "collect", "depends_on": ["b"]},
            ]
        }
        ok, errs = pipeline_schema.validate(p)
        self.assertFalse(ok)
        self.assertIn("pipeline invalid: cycle detected", errs)

    def test_unknown_kind(self):
        p = valid()
        p["nodes"][1] = {"id": "a", "kind": "bogus", "depends_on": ["in"]}
        ok, errs = pipeline_schema.validate(p)
        self.assertFalse(ok)
        self.assertTrue(any("unknown kind 'bogus'" in e for e in errs))

    def test_agent_and_kind_together(self):
        p = valid()
        p["nodes"][1]["kind"] = "collect"
        ok, errs = pipeline_schema.validate(p)
        self.assertFalse(ok)
        self.assertTrue(any("either 'agent' or 'kind'" in e for e in errs))

    def test_duplicate_id_and_unknown_dependency(self):
        p = valid()
        p["nodes"].append({"id": "a", "agent": "z", "depends_on": ["ghost"]})
        ok, errs = pipeline_schema.validate(p)
        self.assertFalse(ok)
        self.assertTrue(any("duplicate id 'a'" in e for e in errs))
        self.assertTrue(any("unknown 'ghost'" in e for e in errs))

    def test_passthrough_needs_one_input(self):
        p = valid()
        p["nodes"][1:1] = [{"id": "b", "agent": "q", "depends_on": ["in"]}]
        p["nodes"][-1] = {"id": "out", "kind": "passthrough", "depends_on": ["a", "b"]}
        ok, errs = pipeline_schema.validate(p)
        self.assertFalse(ok)
        self.assertTrue(any("passthrough sink" in e for e in errs))

    def test_orphan_agent_node(self):
        p = valid()
        p["nodes"].append({"id": "lost", "agent": "z", "depends_on": ["in"]})
        ok, errs = pipeline_schema.validate(p)
        self.assertFalse(ok)
        self.assertTrue(any("'lost' is not connected" in e for e in errs))

    def test_every_error_carries_prefix(self):
        for bad in ([], {}, {"nodes": []}, {"nodes": [1, {"id": ""}, {"id": "a"}]}):
            ok, errs = pipeline_schema.validate(bad)
            self.assertFalse(ok)
            self.assertTrue(errs)
            for e in errs:
                self.assertTrue(e.startswith("pipeline invalid:"), e)


class Cli(unittest.TestCase):
    def setUp(self):
        self._tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self._tmp.cleanup)
        tmp = Path(self._tmp.name)
        self.root = tmp / "project"
        self.state = tmp / "state"
        (self.root / ".nucleos" / "workflow").mkdir(parents=True)
        (self.state / "pipelines").mkdir(parents=True)
        (self.root / ".nucleos" / "workflow" / "project.json").write_text(
            json.dumps({"id": "t", "state_dir": str(self.state), "bundle_dir": str(HERE.parent)}),
            encoding="utf-8")

    def run_cli(self, name):
        env = dict(os.environ, NUCLEOS_PROJECT_ROOT=str(self.root))
        return subprocess.run([sys.executable, str(HERE / "pipeline_schema.py"), name],
                              capture_output=True, text=True, encoding="utf-8", env=env)

    def put(self, name, text):
        (self.state / "pipelines" / f"{name}.yaml").write_text(text, encoding="utf-8")

    def test_valid_prints_resolved_path(self):
        self.put("demo-1", VALID_YAML)
        res = self.run_cli("demo-1")
        self.assertEqual(res.returncode, 0, res.stderr)
        printed = Path(res.stdout.strip())
        self.assertTrue(printed.is_absolute())
        self.assertEqual(printed, (self.state / "pipelines" / "demo-1.yaml").resolve())

    def test_bad_slug(self):
        for name in ("Bad_Name", "../x", "a b"):
            res = self.run_cli(name)
            self.assertEqual(res.returncode, 1, name)
            self.assertIn("pipeline invalid:", res.stderr)

    def test_missing_file(self):
        res = self.run_cli("nope")
        self.assertEqual(res.returncode, 1)
        self.assertIn("pipeline invalid:", res.stderr)

    def test_invalid_pipeline_lists_errors(self):
        self.put("broken", "nodes:\n  - id: a\n    agent: x\n")
        res = self.run_cli("broken")
        self.assertEqual(res.returncode, 1)
        self.assertEqual(res.stdout, "")
        lines = [l for l in res.stderr.splitlines() if l.strip()]
        self.assertGreaterEqual(len(lines), 2)
        for line in lines:
            self.assertTrue(line.startswith("pipeline invalid:"), line)

    def test_unparseable_yaml(self):
        self.put("junk", "nodes: [unclosed\n")
        res = self.run_cli("junk")
        self.assertEqual(res.returncode, 1)
        self.assertIn("pipeline invalid:", res.stderr)


if __name__ == "__main__":
    unittest.main()
