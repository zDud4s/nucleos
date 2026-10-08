"""Heavy commands the shell does not show: inside `bash -c`, `eval`, `xargs`, `find -exec`,
`powershell -Command`, `cmd /c`, a script, a Makefile or an npm script. They have to reach
the broker as a whole, and a cargo inside one has to get a pool slot like a bare cargo.

Found 2026-10-08, after the `for ... do cargo` leak: every one of these ran outside the
queue and built into the shared default target dir."""

import importlib.util
import json
import sys
import tempfile
import unittest
from pathlib import Path

HERE = Path(__file__).parent
PY = "C:/fake/python.exe"


def load(name):
    if name in sys.modules:
        return sys.modules[name]
    spec = importlib.util.spec_from_file_location(name, HERE.parent / f"{name}.py")
    mod = importlib.util.module_from_spec(spec)
    sys.modules[name] = mod
    spec.loader.exec_module(mod)
    return mod


class Base(unittest.TestCase):
    def setUp(self):
        self.hc = load("heavy_classify")
        self.hg = load("heavy_guard")
        self._tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self._tmp.cleanup)
        self.root = Path(self._tmp.name)
        self.main = self.root / "main"
        (self.main / "scripts").mkdir(parents=True)
        (self.main / "scripts" / "heavy.py").write_text("# fake broker\n")
        self.wt = self.root / "wt"
        self.wt.mkdir()
        self.broker = f'{PY} "{self.main.as_posix()}/scripts/heavy.py" --prio 1 --agent main --'

    def write(self, rel, text):
        p = self.wt / rel
        p.parent.mkdir(parents=True, exist_ok=True)
        p.write_text(text, encoding="utf-8")
        return p

    def classify(self, argv):
        return self.hc.classify(argv, cwd=str(self.wt))

    def decide(self, tool, command, agent_id=None):
        p = {"hook_event_name": "PreToolUse", "tool_name": tool, "cwd": str(self.wt),
             "tool_input": {"command": command}}
        if agent_id:
            p["agent_id"] = agent_id
        return self.hg.decide(p, self.main, True, PY)

    def rewritten(self, tool, command):
        out = self.decide(tool, command)
        self.assertIsNotNone(out, f"not routed: {command!r}")
        return out["hookSpecificOutput"]["updatedInput"]["command"]


class ClassifyTests(Base):
    def assertCargo(self, v, weight=2):
        self.assertTrue(v.heavy, v)
        self.assertEqual(v.kind, "cargo", v)
        self.assertEqual(v.weight, weight, v)
        self.assertEqual(Path(v.cargo_argv[0]).stem.lower(), "cargo", v)

    def test_shell_dash_c(self):
        for argv in (["bash", "-c", "cd core && cargo test -p x"],
                     ["sh", "-ec", "for p in a b; do cargo build -p $p; done"],
                     ["bash", "-lc", "cargo clippy"]):
            with self.subTest(argv=argv):
                v = self.classify(argv)
                self.assertCargo(v, 1 if "clippy" in argv[-1] else 2)
                self.assertTrue(v.via)
        v = self.classify(["bash", "-c", "cargo test -p x"])
        self.assertEqual(v.cargo_argv, ["cargo", "test", "-p", "x"])

    def test_eval_and_cmd_and_powershell(self):
        self.assertCargo(self.classify(["eval", "cargo build"]))
        self.assertCargo(self.classify(["cmd", "/c", "cargo build & echo done"]))
        self.assertCargo(self.classify(["cmd", "/S", "/C", "cargo test"]))
        self.assertCargo(self.classify(["powershell", "-NoProfile", "-Command", "cargo test -p x"]))
        self.assertCargo(self.classify(["pwsh", "-c", "foreach ($p in 1) { cargo build }"]))
        self.assertCargo(self.classify(["Invoke-Expression", "cargo build"]))

    def test_xargs_and_find_exec(self):
        self.assertCargo(self.classify(["xargs", "-n", "1", "-P4", "cargo", "test", "-p"]))
        self.assertCargo(self.classify(["find", ".", "-name", "Cargo.toml", "-execdir",
                                        "cargo", "build", ";"]))
        self.assertFalse(self.classify(["xargs", "rm", "-f"]).heavy)
        self.assertFalse(self.classify(["find", ".", "-exec", "grep", "x", "{}", "+"]).heavy)

    def test_shell_script(self):
        self.write("build.sh", "#!/bin/bash\n# cargo test in a comment is not a command\n"
                               "set -e\ncd core\ncargo build --release\n")
        for argv in (["bash", "build.sh"], ["./build.sh"], ["sh", "./build.sh", "arg"]):
            with self.subTest(argv=argv):
                v = self.classify(argv)
                self.assertCargo(v)
                self.assertIn("--release", v.cargo_argv)

    def test_script_calling_a_script(self):
        self.write("a.sh", "#!/bin/sh\nbash ./b.sh\n")
        self.write("b.sh", "#!/bin/sh\ngo test ./...\n")
        v = self.classify(["./a.sh"])
        self.assertTrue(v.heavy)
        self.assertEqual(v.kind, "go")

    def test_powershell_script(self):
        self.write("b.ps1", "param($x)\nif ($x) { cargo test -p $x }\n")
        self.assertCargo(self.classify(["powershell", "-NoProfile", "-File", "b.ps1", "core"]))
        self.assertCargo(self.classify([".\\b.ps1"]))

    def test_python_script_with_a_cargo_subprocess(self):
        self.write("t.py", "import subprocess\n"
                           "subprocess.run(['cargo', 'test', '-p', 'x'], check=True)\n")
        self.assertCargo(self.classify(["python", "t.py"]))

    def test_python_script_that_only_mentions_cargo_is_light(self):
        self.write("msg.py", 'print("run `cargo test -p x` yourself")\n'
                             'ARGV = ["cargo", "test"]\n')
        self.assertFalse(self.classify(["python", "msg.py"]).heavy)

    def test_makefile_and_justfile(self):
        self.write("Makefile", "all:\n\t@cargo build\n\ntest:\n\t-go test ./...\n")
        self.assertCargo(self.classify(["make"]))
        self.write("sub/justfile", "build:\n    cargo build\n")
        self.assertCargo(self.classify(["just", "--justfile", "sub/justfile", "build"]))
        self.assertCargo(self.classify(["make", "-C", "sub", "-f", "justfile"]))

    def test_npm_run_reads_the_script(self):
        self.write("package.json", json.dumps({"scripts": {
            "rust": "cd src-tauri && cargo build", "lint": "eslint ."}}))
        self.assertCargo(self.classify(["npm", "run", "rust"]))
        self.assertFalse(self.classify(["npm", "run", "lint"]).heavy)

    def test_self_brokered_and_gates_stay_as_they_are(self):
        self.write("b.sh", 'python "$MAIN/scripts/heavy.py" -- cargo build\n')
        self.assertFalse(self.classify(["bash", "b.sh"]).heavy)
        self.assertTrue(self.classify(["bash", "-c", "bash scripts/gates.sh all"]).gate)
        self.assertTrue(self.classify(["./scripts/gates.sh", "core"]).gate)

    def test_a_script_marked_skip_is_light(self):
        # run-daemon.ps1: its daemon must not inherit a token, a slot and its target dir.
        self.write("run.ps1", "# heavy-broker: skip\n& cargo build -p core\n& $exe\n")
        self.assertFalse(self.classify(["powershell", "-File", "run.ps1"]).heavy)
        real = HERE.parent / "run-daemon.ps1"
        self.assertFalse(self.hc.classify(["powershell", "-File", str(real)]).heavy)

    def test_powershell_block_comments_are_not_commands(self):
        self.write("c.ps1", "<#\ncargo build\n#>\nWrite-Host hi\n")
        self.assertFalse(self.classify(["powershell", "-File", "c.ps1"]).heavy)

    def test_a_pass_through_script_is_judged_by_its_arguments(self):
        # own-cargo-target.sh <root> <command...>
        self.write("own.sh", 'root="$1"; shift\ncd "$root"\nexec "$@"\n')
        v = self.classify(["bash", "own.sh", "C:/t", "cargo", "test", "-p", "x"])
        self.assertCargo(v)
        self.assertEqual(v.cargo_argv, ["cargo", "test", "-p", "x"])
        self.assertFalse(self.classify(["bash", "own.sh", "C:/t", "ls"]).heavy)

    def test_a_wrapper_with_its_own_target_dir_leases_no_slot(self):
        hb = load("heavy")
        self.write("own.sh", 'export CARGO_TARGET_DIR="$1"; shift\nexec "$@"\n')
        argv = ["bash", "own.sh", "C:/t", "cargo", "test"]
        v = self.classify(argv)
        self.assertTrue(v.heavy and v.kind == "cargo")
        self.assertIsNone(hb.pool_argv(argv, v))
        argv = ["bash", "-c", "cargo build --target-dir C:/x"]
        self.assertIsNone(hb.pool_argv(argv, self.classify(argv)))
        # Reading it is not setting it (stage-bundle-bins.sh).
        self.write("read.sh", 'd="${CARGO_TARGET_DIR:-}"\ncargo build --release\n')
        argv = ["bash", "read.sh"]
        self.assertEqual(hb.pool_argv(argv, self.classify(argv)), ["cargo", "build", "--release"])

    def test_recursion_is_bounded(self):
        self.write("loop.sh", "bash ./loop.sh\ncargo build\n")
        v = self.classify(["bash", "loop.sh"])  # must not recurse for ever
        self.assertTrue(v.heavy)

    def test_missing_or_binary_files_fail_open(self):
        self.assertFalse(self.classify(["bash", "nope.sh"]).heavy)
        (self.wt / "bin.sh").write_bytes(b"\x00\xff" * 100)
        self.assertFalse(self.classify(["bash", "bin.sh"]).heavy)


class GuardTests(Base):
    def test_shell_dash_c_is_wrapped_whole(self):
        cmd = "bash -c 'cargo test -p x' 2>&1 | tail -3"
        self.assertEqual(self.rewritten("Bash", cmd),
                         f"{self.broker} bash -c 'cargo test -p x' 2>&1 | tail -3")

    def test_xargs_is_wrapped_whole(self):
        cmd = "echo a b | xargs -n1 cargo test -p"
        self.assertEqual(self.rewritten("Bash", cmd),
                         f"echo a b | {self.broker} xargs -n1 cargo test -p")

    def test_a_script_runs_through_its_interpreter(self):
        self.write("build.sh", "cargo build\n")
        self.assertEqual(self.rewritten("Bash", "./build.sh --x"),
                         f"{self.broker} bash ./build.sh --x")
        self.write("b.ps1", "cargo build\n")
        self.assertEqual(self.rewritten("PowerShell", ".\\b.ps1"),
                         f"{self.broker} powershell -NoProfile -File .\\b.ps1")
        self.assertEqual(self.rewritten("Bash", "bash build.sh"), f"{self.broker} bash build.sh")

    def test_builtins_become_a_child_shell(self):
        self.assertEqual(self.rewritten("Bash", 'eval "cargo build"'),
                         f'{self.broker} bash -c "cargo build"')
        self.assertEqual(self.rewritten("PowerShell", "iex 'cargo build'"),
                         f"{self.broker} powershell -NoProfile -Command 'cargo build'")

    def test_cd_before_a_script_is_followed(self):
        self.write("sub/build.sh", "cargo build\n")
        self.assertEqual(self.rewritten("Bash", "cd sub && ./build.sh"),
                         f"cd sub && {self.broker} bash ./build.sh")

    def test_a_gate_inside_a_shell_is_left_alone(self):
        self.assertIsNone(self.decide("Bash", "bash -c 'bash scripts/gates.sh all'"))

    def test_subagent_script_with_cargo_is_denied(self):
        self.write("build.sh", "cargo test -p x\n")
        out = self.decide("Bash", "./build.sh", agent_id="a1")
        self.assertEqual(out["hookSpecificOutput"]["permissionDecision"], "deny")
        self.assertIn("No subagent runs cargo", out["hookSpecificOutput"]["permissionDecisionReason"])

    def test_light_wrappers_are_left_alone(self):
        self.write("ok.sh", "echo hi\n")
        for cmd in ("bash -c 'ls -la'", "./ok.sh", "find . -exec grep x {} +",
                    "xargs echo", "python -c 'print(1)'"):
            with self.subTest(cmd=cmd):
                self.assertIsNone(self.decide("Bash", cmd))


class BrokerPoolTests(Base):
    """The broker leases a pool slot for a cargo it found inside a wrapper."""

    def test_pool_argv_is_the_inner_cargo(self):
        hb = load("heavy")
        v = self.hc.classify(["bash", "-c", "cargo test -p x"], cwd=str(self.wt))
        self.assertEqual(hb.pool_argv(["bash", "-c", "cargo test -p x"], v),
                         ["cargo", "test", "-p", "x"])
        self.assertEqual(hb.pool_argv(["cargo", "build"], self.hc.classify(["cargo", "build"])),
                         ["cargo", "build"])
        light = self.hc.classify(["bash", "-c", "go test ./..."])
        self.assertIsNone(hb.pool_argv(["bash", "-c", "go test ./..."], light))


if __name__ == "__main__":
    unittest.main()
