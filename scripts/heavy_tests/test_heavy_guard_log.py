"""guard.jsonl: every deny the heavy guard hands a subagent is logged, append-only; the log
never changes the decision and fails silently (spec 2026-10-21 §13.4)."""

import importlib.util
import json
import os
import subprocess
import sys
import tempfile
import unittest
from datetime import datetime
from pathlib import Path

HERE = Path(__file__).parent


def load():
    spec = importlib.util.spec_from_file_location("heavy_guard", HERE.parent / "heavy_guard.py")
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


def bash(command, agent_id=None, **extra):
    tool_input = {"command": command, "description": "x"}
    tool_input.update(extra)
    payload = {"hook_event_name": "PreToolUse", "tool_name": "Bash", "tool_input": tool_input,
               "session_id": "sess-1"}
    if agent_id:
        payload["agent_id"] = agent_id
        payload["agent_type"] = "worker"
    return payload


class GuardLogTests(unittest.TestCase):
    def setUp(self):
        self.hg = load()
        self._tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self._tmp.cleanup)
        root = Path(self._tmp.name)
        self.main = root / "main"
        (self.main / "scripts").mkdir(parents=True)
        (self.main / "scripts" / "heavy.py").write_text("# fake broker\n")
        self.state = root / "state"
        self.env = dict(os.environ, NUCLEOS_HEAVY_MAIN=str(self.main),
                        NUCLEOS_HEAVY_DIR=str(self.state))

    def run_main(self, payload, env=None):
        return subprocess.run(
            [sys.executable, str(HERE.parent / "heavy_guard.py")],
            input=json.dumps(payload), capture_output=True, text=True,
            env=env or self.env, timeout=60)

    def lines(self):
        path = self.state / "guard.jsonl"
        if not path.exists():
            return []
        return [json.loads(x) for x in path.read_text(encoding="utf-8").splitlines() if x]

    def assertDenied(self, r):
        self.assertEqual(r.returncode, 0, r.stderr)
        out = json.loads(r.stdout)
        self.assertEqual(out["hookSpecificOutput"]["permissionDecision"], "deny")

    def test_subagent_cargo_logs_one_line_with_all_fields(self):
        r = self.run_main(bash("cargo test the_name", agent_id="a1"))
        self.assertDenied(r)
        (line,) = self.lines()
        self.assertEqual(set(line), {"at", "session", "agent", "agent_type", "tool",
                                     "command", "reason"})
        self.assertEqual(line["session"], "sess-1")
        self.assertEqual(line["agent"], "a1")
        self.assertEqual(line["agent_type"], "worker")
        self.assertEqual(line["tool"], "Bash")
        self.assertEqual(line["command"], "cargo test the_name")
        self.assertEqual(line["reason"], "cargo")
        stamp = datetime.fromisoformat(line["at"].replace("Z", "+00:00"))
        self.assertIsNotNone(stamp.tzinfo)

    def test_log_is_append_only_and_command_is_cut(self):
        long_cmd = "cargo build " + "x" * 800
        self.assertDenied(self.run_main(bash(long_cmd, agent_id="a1")))
        self.assertDenied(self.run_main(bash("cargo fmt", agent_id="a2")))
        first, second = self.lines()
        self.assertEqual(len(first["command"]), 500)
        self.assertEqual(second["agent"], "a2")

    def test_warm_and_isolation_reasons(self):
        self.assertDenied(self.run_main(bash("python scripts/heavy.py warm", agent_id="a1")))
        agent = {"tool_name": "Agent", "tool_input": {"isolation": "worktree"},
                 "agent_id": "a1", "session_id": "sess-1"}
        self.assertDenied(self.run_main(agent))
        warm, iso = self.lines()
        self.assertEqual(warm["reason"], "warm")
        self.assertEqual(iso["reason"], "isolation")
        self.assertEqual(iso["tool"], "Agent")
        self.assertEqual(iso["command"], "")

    def test_deny_kind_maps_gate_text(self):
        hg = self.hg
        deny = hg._deny(hg.DENY_REASON)
        self.assertEqual(hg._deny_kind(deny, "bash scripts/gates.sh core"), "gate")
        self.assertEqual(hg._deny_kind(hg._deny(hg.DENY_CARGO), "cargo test x"), "cargo")
        self.assertEqual(hg._deny_kind(hg._deny(hg.DENY_ISOLATION), ""), "isolation")
        self.assertIsNone(hg._deny_kind(None, "cargo test x"))
        self.assertIsNone(hg._deny_kind({"hookSpecificOutput": {"updatedInput": {}}}, "x"))

    def test_main_session_cargo_logs_nothing(self):
        r = self.run_main(bash("cargo build"))  # rewritten through the broker, not denied
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(self.lines(), [])
        # A main-session isolation deny is not a subagent deny either.
        agent = {"tool_name": "Agent", "tool_input": {"isolation": "worktree"}}
        self.assertDenied(self.run_main(agent))
        self.assertEqual(self.lines(), [])

    def test_subagent_passthrough_logs_nothing(self):
        r = self.run_main(bash("git status", agent_id="a1"))
        self.assertEqual(r.stdout.strip(), "")
        self.assertEqual(self.lines(), [])

    def test_unwritable_dir_leaves_decision_unchanged(self):
        blocker = Path(self._tmp.name) / "blocker"
        blocker.write_text("a file, so the state dir cannot be created under it")
        env = dict(self.env, NUCLEOS_HEAVY_DIR=str(blocker / "heavy"))
        quiet = self.run_main(bash("cargo test x", agent_id="a1"), env)
        loud = self.run_main(bash("cargo test x", agent_id="a1"))
        self.assertDenied(quiet)
        self.assertEqual(quiet.stdout, loud.stdout)
        self.assertEqual(quiet.stderr.strip(), "")  # fails silently


if __name__ == "__main__":
    unittest.main()
