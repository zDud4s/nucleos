"""heavy_guard on compound commands: a heavy command behind a shell keyword, inside a
subshell or script block, or sent to the background must still reach the broker.

Found 2026-10-08: `for p in a b; do cargo test -p $p; done` ran outside the queue,
because the segment `do cargo test -p $p` starts with `do` and was judged not heavy."""

import importlib.util
import subprocess
import tempfile
import unittest
from pathlib import Path

HERE = Path(__file__).parent
PY = "C:/fake/python.exe"
GIT_BASH = Path("C:/Program Files/Git/bin/bash.exe")


def load():
    spec = importlib.util.spec_from_file_location("heavy_guard", HERE.parent / "heavy_guard.py")
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


def payload(tool, command, agent_id=None):
    p = {"hook_event_name": "PreToolUse", "tool_name": tool, "tool_input": {"command": command}}
    if agent_id:
        p["agent_id"] = agent_id
    return p


class CompoundTests(unittest.TestCase):
    def setUp(self):
        self.hg = load()
        self._tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self._tmp.cleanup)
        self.main = Path(self._tmp.name) / "main"
        (self.main / "scripts").mkdir(parents=True)
        (self.main / "scripts" / "heavy.py").write_text("# fake broker\n")
        self.broker = f'{PY} "{self.main.as_posix()}/scripts/heavy.py" --prio 1 --agent main --'

    def run_hook(self, tool, command, agent_id=None):
        return self.hg.decide(payload(tool, command, agent_id), self.main, True, PY)

    def rewrite(self, tool, command):
        out = self.run_hook(tool, command)
        self.assertIsNotNone(out, f"not routed: {command!r}")
        return out["hookSpecificOutput"]["updatedInput"]["command"]

    def bash(self, command):
        return self.rewrite("Bash", command)

    def ps(self, command):
        return self.rewrite("PowerShell", command)

    # ---- bash keywords

    def test_for_loop_body_is_routed(self):
        self.assertEqual(
            self.bash("for p in a b; do cargo test -p $p; done"),
            f"for p in a b; do {self.broker} cargo test -p $p; done")

    def test_loop_body_on_its_own_line_is_routed(self):
        self.assertEqual(
            self.bash("for p in a b\ndo\n  cargo build -p $p\ndone"),
            f"for p in a b\ndo\n  {self.broker} cargo build -p $p\ndone")

    def test_while_until_and_negation_are_routed(self):
        self.assertEqual(self.bash("while ! cargo check; do sleep 1; done"),
                         f"while ! {self.broker} cargo check; do sleep 1; done")
        self.assertEqual(self.bash("until cargo test; do :; done"),
                         f"until {self.broker} cargo test; do :; done")

    def test_if_then_else_branches_are_routed(self):
        self.assertEqual(
            self.bash("if cargo check; then cargo test; else npm run build; fi"),
            f"if {self.broker} cargo check; then {self.broker} cargo test; "
            f"else {self.broker} npm run build; fi")

    def test_brace_group_is_routed(self):
        self.assertEqual(self.bash("{ cargo build; cargo test; } 2>&1 | tail -5"),
                         f"{{ {self.broker} cargo build; {self.broker} cargo test; }} 2>&1 | tail -5")

    # ---- subshells, substitution, background

    def test_subshell_is_routed(self):
        self.assertEqual(self.bash("(cd core && cargo build)"),
                         f"(cd core && {self.broker} cargo build)")

    def test_command_substitution_is_routed(self):
        self.assertEqual(self.bash('out=$(cargo test 2>&1); echo "$out"'),
                         f'out=$({self.broker} cargo test 2>&1); echo "$out"')

    def test_both_sides_of_a_background_ampersand_are_routed(self):
        self.assertEqual(self.bash("cargo build & cargo test; wait"),
                         f"{self.broker} cargo build & {self.broker} cargo test; wait")

    def test_redirections_with_ampersand_are_not_cut(self):
        self.assertEqual(self.bash("cargo test &>log.txt"),
                         f"{self.broker} cargo test &>log.txt")
        self.assertEqual(self.bash("cargo test >&2"), f"{self.broker} cargo test >&2")

    def test_parameter_expansion_is_one_segment(self):
        self.assertEqual(self.bash("cargo test -p ${CRATE:-core}"),
                         f"{self.broker} cargo test -p ${{CRATE:-core}}")

    # ---- wrappers that run their argument

    def test_wrappers_keep_their_place_in_front_of_the_broker(self):
        for wrapper in ("exec", "command", "nohup", "env", "env FOO=1", "nice",
                        "nice -n 5", "timeout 600", "timeout -k 5 10m", "do FOO=1 time"):
            with self.subTest(wrapper=wrapper):
                self.assertEqual(self.bash(f"{wrapper} cargo test"),
                                 f"{wrapper} {self.broker} cargo test")

    # ---- PowerShell

    def test_powershell_script_block_is_routed(self):
        self.assertEqual(
            self.ps("foreach ($c in @('a','b')) { cargo test -p $c }"),
            f"foreach ($c in @('a','b')) {{ {self.broker} cargo test -p $c }}")

    def test_powershell_if_else_blocks_are_routed(self):
        self.assertEqual(
            self.ps("if ($x) { cargo build } else { npm run build }"),
            f"if ($x) {{ {self.broker} cargo build }} else {{ {self.broker} npm run build }}")

    def test_powershell_call_operator_is_routed(self):
        self.assertEqual(self.ps("& cargo build"), f"& {self.broker} cargo build")
        self.assertEqual(self.ps("& 'C:\\Projects\\cargo\\bin\\cargo.exe' test"),
                         f"& {self.broker} 'C:\\Projects\\cargo\\bin\\cargo.exe' test")

    def test_powershell_variable_braces_are_one_segment(self):
        self.assertEqual(self.ps("go build -o ${env:OUT} ./..."),
                         f"{self.broker} go build -o ${{env:OUT}} ./...")

    # ---- what must stay untouched

    def test_light_loops_are_left_alone(self):
        for cmd in ("for f in *.rs; do echo $f; done",
                    "if [ -f Cargo.toml ]; then cat Cargo.toml; fi",
                    "(cd core && ls)",
                    "sleep 1 & wait"):
            with self.subTest(cmd=cmd):
                self.assertIsNone(self.run_hook("Bash", cmd))

    def test_an_already_wrapped_body_is_left_alone(self):
        cmd = f"for p in a b; do {self.broker} cargo test -p $p; done"
        self.assertIsNone(self.run_hook("Bash", cmd))

    def test_subagent_unfiltered_go_inside_a_loop_is_denied(self):
        out = self.run_hook("Bash", "for d in a b; do (cd $d && go test ./...); done", "a1")
        self.assertIsNotNone(out)
        self.assertEqual(out["hookSpecificOutput"]["permissionDecision"], "deny")

    # ---- the rewrite must still be valid shell

    @unittest.skipUnless(GIT_BASH.is_file(), "Git Bash not installed")
    def test_rewrites_parse_under_bash(self):
        for cmd in ("for p in a b; do cargo test -p $p; done",
                    "if cargo check; then cargo test; else npm run build; fi",
                    "{ cargo build; cargo test; } 2>&1 | tail -5",
                    "(cd core && cargo build)",
                    'out=$(cargo test 2>&1); echo "$out"',
                    "cargo build & cargo test; wait",
                    "while ! cargo check; do sleep 1; done"):
            with self.subTest(cmd=cmd):
                r = subprocess.run([str(GIT_BASH), "-n", "-c", self.bash(cmd)],
                                   capture_output=True, text=True)
                self.assertEqual(r.returncode, 0, r.stderr)


if __name__ == "__main__":
    unittest.main()
