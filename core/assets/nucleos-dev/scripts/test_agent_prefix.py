#!/usr/bin/env python3
"""Tests for agent_prefix.py.

    python test_agent_prefix.py -v
"""

from __future__ import annotations

import json
import sys
import tempfile
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))

import agent_prefix  # noqa: E402
import pipeline_fanout  # noqa: E402

BYPASS = "--dangerously-bypass-approvals-and-sandbox"


def make_prefix(**over):
    base = {
        "schema": 1,
        "name": "reviewer",
        "sandbox": "read-only",
        "web_search": False,
        "prefix": "PREFIX BODY",
    }
    base.update(over)
    return base


def node(prefix, stdin="in"):
    return agent_prefix.build_node("n1", "/bin/codex", "/proj", prefix, "gpt-x", "high", 600, stdin)


def flag(cmd, key):
    """The `-c key=...` override, or None."""
    for i, part in enumerate(cmd[:-1]):
        if part == "-c" and cmd[i + 1].startswith(key + "="):
            return cmd[i + 1]
    return None


class LoadPrefix(unittest.TestCase):
    def test_load_prefix_missing_returns_none(self):
        with tempfile.TemporaryDirectory() as d:
            self.assertIsNone(agent_prefix.load_prefix(Path(d), "nope"))

    def test_load_prefix_rejects_unknown_schema(self):
        with tempfile.TemporaryDirectory() as d:
            (Path(d) / "x.json").write_text(json.dumps(make_prefix(schema=2)), encoding="utf-8")
            with self.assertRaises(ValueError):
                agent_prefix.load_prefix(Path(d), "x")

    def test_load_prefix_reads_schema_1(self):
        with tempfile.TemporaryDirectory() as d:
            (Path(d) / "x.json").write_text(json.dumps(make_prefix()), encoding="utf-8")
            self.assertEqual(agent_prefix.load_prefix(Path(d), "x")["sandbox"], "read-only")


class BuildStdin(unittest.TestCase):
    def test_build_stdin_order_prefix_task_ancestors(self):
        out = agent_prefix.build_stdin(make_prefix(), "DO IT", [("a", "planner", "OUT-A")])
        self.assertLess(out.index("PREFIX BODY"), out.index("## Task"))
        self.assertLess(out.index("DO IT"), out.index("Relevant context from prior steps"))
        self.assertLess(out.index("a (planner)"), out.index("OUT-A"))

    def test_build_stdin_without_ancestors_has_no_context_block(self):
        out = agent_prefix.build_stdin(make_prefix(), "DO IT", [])
        self.assertNotIn("Relevant context", out)

    def test_build_stdin_truncates_ancestors_to_2000(self):
        out = agent_prefix.build_stdin(make_prefix(), "t", [("a", "p", "Z" * 3000)])
        self.assertEqual(out.count("Z"), 2000)


class BuildNode(unittest.TestCase):
    def test_build_node_read_only_has_sandbox_override_and_no_bypass_flag(self):
        cmd = node(make_prefix())["cmd"]
        self.assertEqual(flag(cmd, "sandbox_mode"), 'sandbox_mode="read-only"')
        self.assertNotIn(BYPASS, cmd)
        self.assertEqual(flag(cmd, "model_reasoning_effort"), 'model_reasoning_effort="high"')
        self.assertEqual(cmd[cmd.index("-C") + 1], "/proj")
        self.assertEqual(cmd[cmd.index("-m") + 1], "gpt-x")
        self.assertEqual(cmd[:3], ["/bin/codex", "exec", "--skip-git-repo-check"])

    def test_build_node_workspace_write(self):
        cmd = node(make_prefix(sandbox="workspace-write"))["cmd"]
        self.assertEqual(flag(cmd, "sandbox_mode"), 'sandbox_mode="workspace-write"')
        self.assertNotIn(BYPASS, cmd)

    def test_build_node_web_search_values(self):
        live = node(make_prefix(web_search=True))["cmd"]
        off = node(make_prefix(web_search=False))["cmd"]
        self.assertEqual(flag(live, "web_search"), 'web_search="live"')
        self.assertEqual(flag(off, "web_search"), 'web_search="disabled"')

    def test_build_node_carries_id_stdin_timeout(self):
        n = node(make_prefix(), stdin="hello")
        self.assertEqual((n["id"], n["stdin"], n["timeout"]), ("n1", "hello", 600))


class WithFanout(unittest.TestCase):
    def test_build_node_cmd_is_valid_for_fanout_spec(self):
        nodes, _ = pipeline_fanout._require_spec({"nodes": [node(make_prefix())]})
        self.assertEqual(len(nodes), 1)

    def test_prefix_reaches_node_stdin(self):
        n = node(make_prefix(), stdin=agent_prefix.build_stdin(make_prefix(), "T", []))
        n["cmd"] = [sys.executable, "-c", "import sys;print(sys.stdin.read())"]
        res = pipeline_fanout.run_fanout({"nodes": [n]})
        self.assertEqual(res[0]["status"], "ok")
        self.assertIn("PREFIX BODY", res[0]["stdout"])


if __name__ == "__main__":
    unittest.main()
