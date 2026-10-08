"""RED tests: heavy_classify must see through every option the guard itself emits
(`--wait-max`) and through `hold-worktree`, so a wrapped command is judged as the inner one."""

import importlib.util
import tempfile
import unittest
from pathlib import Path

HERE = Path(__file__).parent
PY = "C:/fake/python.exe"
BROKER = "C:/Projects/nucleos/scripts/heavy.py"


def load(name):
    spec = importlib.util.spec_from_file_location(name, HERE.parent / f"{name}.py")
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


def bash(command, agent_id=None):
    payload = {"hook_event_name": "PreToolUse", "tool_name": "Bash",
               "tool_input": {"command": command, "description": "x"}}
    if agent_id:
        payload["agent_id"] = agent_id
    return payload


class UnwrapTests(unittest.TestCase):
    def setUp(self):
        self.hc = load("heavy_classify")

    def c(self, *argv):
        return self.hc.classify(["python", BROKER, *argv])

    def test_wait_max_separate(self):
        v = self.c("--wait-max", "90", "--", "cargo", "clippy")
        self.assertTrue(v.heavy)
        self.assertEqual(v.kind, "cargo")
        self.assertEqual(v.inner, ["cargo", "clippy"])

    def test_wait_max_equals(self):
        v = self.c("--wait-max=90", "--", "cargo", "clippy")
        self.assertTrue(v.heavy)
        self.assertEqual(v.kind, "cargo")
        self.assertEqual(v.inner, ["cargo", "clippy"])

    def test_all_options_combined(self):
        v = self.c("--prio", "2", "--agent", "x", "--wait-max", "90", "--kind", "cargo",
                   "--", "cargo", "test", "-p", "a", "foo")
        self.assertTrue(v.heavy)
        self.assertTrue(v.filtered)

    def test_hold_worktree_wrapping_gate(self):
        v = self.c("hold-worktree", "--prio", "0", "--", "python",
                   ".ai/scripts/select_tests.py", "--gate", "--run")
        self.assertTrue(v.gate)

    def test_hold_worktree_wrapping_bare_cargo_test(self):
        v = self.c("hold-worktree", "--", "cargo", "test")
        self.assertTrue(v.heavy)
        self.assertFalse(v.filtered)


class GuardUnwrapTests(unittest.TestCase):
    def setUp(self):
        self.hg = load("heavy_guard")
        self._tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self._tmp.cleanup)
        self.main = Path(self._tmp.name) / "main"
        (self.main / "scripts").mkdir(parents=True)
        (self.main / "scripts" / "heavy.py").write_text("# fake broker\n")

    def denied(self, command):
        out = self.hg.decide(bash(command, agent_id="a1"), self.main, True, PY)
        self.assertIsNotNone(out, f"expected a denial: {command}")
        self.assertEqual(out["hookSpecificOutput"].get("permissionDecision"), "deny", command)

    def test_subagent_wait_max_wrapped_unfiltered_clippy_denied(self):
        self.denied(f"python {BROKER} --wait-max 90 -- cargo clippy")

    def test_subagent_hold_worktree_gate_denied(self):
        self.denied(
            f"python {BROKER} hold-worktree -- python .ai/scripts/select_tests.py --gate --run")


if __name__ == "__main__":
    unittest.main()
