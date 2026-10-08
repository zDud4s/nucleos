"""Tests for heavy_guard: the fail-open PreToolUse hook that routes heavy commands
through the broker (rewrite in place) or denies them for subagents."""

import importlib.util
import json
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

HERE = Path(__file__).parent
PY = "C:/fake/python.exe"


def load():
    # Imported by path so the test does not depend on sys.path. A missing module
    # fails loudly here, which is the RED state.
    spec = importlib.util.spec_from_file_location("heavy_guard", HERE.parent / "heavy_guard.py")
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


def bash(command, agent_id=None, **extra):
    tool_input = {"command": command, "description": "x"}
    tool_input.update(extra)
    payload = {"hook_event_name": "PreToolUse", "tool_name": "Bash", "tool_input": tool_input}
    if agent_id:
        payload["agent_id"] = agent_id
    return payload


def powershell(command, agent_id=None):
    payload = {"hook_event_name": "PreToolUse", "tool_name": "PowerShell",
               "tool_input": {"command": command}}
    if agent_id:
        payload["agent_id"] = agent_id
    return payload


class HeavyGuardTests(unittest.TestCase):
    def setUp(self):
        self.hg = load()
        self._tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self._tmp.cleanup)
        self.main = Path(self._tmp.name) / "main"
        (self.main / "scripts").mkdir(parents=True)
        (self.main / "scripts" / "heavy.py").write_text("# fake broker\n")
        self.main_posix = self.main.as_posix()
        self.broker = f'{PY} "{self.main_posix}/scripts/heavy.py"'

    def decide(self, payload, broker_ok=True):
        return self.hg.decide(payload, self.main, broker_ok, PY)

    def rewritten(self, out):
        self.assertIsNotNone(out, "expected a rewrite")
        spec = out["hookSpecificOutput"]
        self.assertEqual(spec["hookEventName"], "PreToolUse")
        self.assertNotIn("permissionDecision", spec)
        return spec["updatedInput"]

    def assertDenied(self, out, label=""):
        self.assertIsNotNone(out, f"expected a denial: {label}")
        spec = out["hookSpecificOutput"]
        self.assertEqual(spec["hookEventName"], "PreToolUse")
        self.assertEqual(spec["permissionDecision"], "deny", label)
        self.assertIn("test_one", spec["permissionDecisionReason"], label)
        self.assertNotIn("updatedInput", spec, label)

    # 7
    def test_subagent_filtered_test_rewritten_in_place(self):
        # cargo never reaches this path for a subagent (see the owner-rule test);
        # go and npm still do.
        cmd = "cd sidecars/echo && go test -run TestX ./... 2>&1 | tail -5"
        ui = self.rewritten(self.decide(bash(cmd, agent_id="a1")))
        new = ui["command"]
        # Only the heavy segment changes; cd, the redirect and the pipe stay intact.
        self.assertTrue(new.startswith("cd sidecars/echo && "), new)
        self.assertTrue(new.endswith(" 2>&1 | tail -5"), new)
        self.assertIn(
            f"{self.broker} --prio 2 --agent a1", new)
        self.assertIn("-- go test -run TestX ./...", new)
        # Description and the other fields of tool_input survive.
        self.assertEqual(ui["description"], "x")
        # `timeout` is read-only to hooks: never rewritten.
        self.assertEqual(ui.get("timeout"), None)

        # --wait-max = timeout/1000 - 30; absent timeout -> 120000 -> 90; floor 30.
        self.assertIn("--wait-max 90", new)
        ui = self.rewritten(self.decide(
            bash("go test -run TestX ./...", agent_id="a1", timeout=300000)))
        self.assertIn("--wait-max 270", ui["command"])
        self.assertEqual(ui["timeout"], 300000)
        ui = self.rewritten(self.decide(
            bash("go test -run TestX ./...", agent_id="a1", timeout=40000)))
        self.assertIn("--wait-max 30", ui["command"])
        self.assertEqual(ui["timeout"], 40000)

        # Other filtered runners get the same treatment.
        for filtered in ("npm test -- -t the_name", "go test -run TestX ./..."):
            with self.subTest(cmd=filtered):
                ui = self.rewritten(self.decide(bash(filtered, agent_id="a1")))
                self.assertIn("--prio 2", ui["command"])
                self.assertTrue(ui["command"].endswith(f"-- {filtered}"), ui["command"])

    # 8
    def test_env_prefix_stays_before_envelope(self):
        ui = self.rewritten(self.decide(
            bash("GOFLAGS=-v FOO=1 go test -run TestX ./...", agent_id="a1")))
        new = ui["command"]
        self.assertTrue(
            new.startswith(f"GOFLAGS=-v FOO=1 {self.broker} --prio 2 --agent a1"), new)
        self.assertIn("--wait-max 90", new)
        self.assertTrue(new.endswith("-- go test -run TestX ./..."), new)

    def test_a_time_prefix_is_seen_through_and_kept(self):
        # 2026-10-08: `time npm test ...` ran unbrokered, because classify saw `time` as the
        # program. The keyword stays in front, so it still times the whole (queued) run.
        for prefix in ("time ", "time -p ", "FOO=1 time ", "time FOO=1 "):
            with self.subTest(prefix=prefix):
                ui = self.rewritten(self.decide(bash(f"{prefix}npm test -- -t x > f 2>&1")))
                new = ui["command"]
                self.assertTrue(new.startswith(f"{prefix}{self.broker} --prio 1"), new)
                self.assertTrue(new.endswith("-- npm test -- -t x > f 2>&1"), new)
        out = self.hg.decide(bash("time cargo build", agent_id="a1"), self.main, True, PY)
        self.assertEqual(out["hookSpecificOutput"]["permissionDecision"], "deny")
        self.assertIsNone(self.decide(bash("time ls -la")))

    # 9
    def test_subagent_denials(self):
        denied = [
            "bash scripts/gates.sh core",
            "bash scripts/gates.sh all",
            "python .ai/scripts/select_tests.py --gate --run",
            "bash scripts/build-slot.sh cargo build",
            "cargo test",
            "cargo test -p nucleos-core",
            "cargo test -p nucleos-core -- --nocapture",
            "cargo check",
            "cargo clippy --all-targets -- -D warnings",
            "cargo build",
            "cargo run",
            "cargo doc",
            "cargo tauri build",
            "npx tsc -b",
            "npm exec tsc -- -b",
            "npm run build",
            "npm run tauri build",
            "go vet ./...",
            "go test ./...",
            "cd sidecars/echo && go test -race ./...",
            # Already wrapped: the inner command is judged, never trusted.
            f'"{PY}" "{self.main_posix}/scripts/heavy.py" --prio 0 -- cargo build',
        ]
        for cmd in denied:
            with self.subTest(cmd=cmd):
                self.assertDenied(self.decide(bash(cmd, agent_id="a1")), cmd)
        # A wrapped filtered (non-cargo) test is fine and is NOT rewritten a second time.
        wrapped = f'"{PY}" "{self.main_posix}/scripts/heavy.py" --prio 2 -- go test -run TestX ./...'
        self.assertIsNone(self.decide(bash(wrapped, agent_id="a1")))

    # 10
    def test_main_session_gate_passes_other_heavy_prio1(self):
        for gate in ("bash scripts/gates.sh core", "python .ai/scripts/select_tests.py --gate --run"):
            with self.subTest(gate=gate):
                self.assertIsNone(self.decide(bash(gate)))
        ui = self.rewritten(self.decide(bash("cargo build")))
        self.assertIn(f"{self.broker} --prio 1 --agent main -- cargo build", ui["command"])
        ui = self.rewritten(self.decide(bash("cd shell && npm run build && echo done")))
        new = ui["command"]
        self.assertTrue(new.startswith("cd shell && "), new)
        self.assertTrue(new.endswith(" && echo done"), new)
        self.assertIn(f"{self.broker} --prio 1 --agent main -- npm run build", new)
        # The main session may run an unfiltered test: heavy, but not denied.
        ui = self.rewritten(self.decide(bash("cargo test")))
        self.assertIn("--prio 1 --agent main", ui["command"])

    # 11
    def test_powershell_rewrite(self):
        ui = self.rewritten(self.decide(
            powershell("cd C:\\x; go test -run TestX ./...", agent_id="a1")))
        new = ui["command"]
        self.assertTrue(new.startswith("cd C:\\x; "), new)
        self.assertIn(
            f'{PY} "{self.main_posix}/scripts/heavy.py" --prio 2 --agent a1', new)
        self.assertIn("--wait-max 90", new)
        self.assertTrue(new.endswith("-- go test -run TestX ./..."), new)
        self.assertNotIn("timeout", ui)
        # Denials apply to PowerShell too.
        self.assertDenied(self.decide(powershell("cargo build", agent_id="a1")))
        # Main session, PowerShell.
        ui = self.rewritten(self.decide(powershell("cargo check")))
        self.assertIn("--prio 1 --agent main -- cargo check", ui["command"])

    # 12
    def test_agent_isolation_worktree_denied(self):
        for agent_id in (None, "a1"):
            with self.subTest(agent_id=agent_id):
                payload = {"hook_event_name": "PreToolUse", "tool_name": "Agent",
                           "tool_input": {"prompt": "p", "isolation": "worktree"}}
                if agent_id:
                    payload["agent_id"] = agent_id
                out = self.decide(payload)
                self.assertIsNotNone(out)
                spec = out["hookSpecificOutput"]
                self.assertEqual(spec["permissionDecision"], "deny")
                self.assertTrue(spec["permissionDecisionReason"])
        plain = {"hook_event_name": "PreToolUse", "tool_name": "Agent",
                 "tool_input": {"prompt": "p"}}
        self.assertIsNone(self.decide(plain))

    # 13
    def test_broker_absent_no_rewrite_denials_apply(self):
        self.assertIsNone(self.decide(bash("go test -run TestX ./...", agent_id="a1"), broker_ok=False))
        self.assertDenied(self.decide(bash("cargo test the_name", agent_id="a1"), broker_ok=False))
        self.assertIsNone(self.decide(bash("cargo build"), broker_ok=False))
        self.assertDenied(self.decide(bash("cargo build", agent_id="a1"), broker_ok=False))
        self.assertDenied(self.decide(bash("bash scripts/gates.sh core", agent_id="a1"), broker_ok=False))
        # End to end: a main checkout without a broker -> no stdout for a rewritable command,
        # a denial still printed.
        bare = Path(self._tmp.name) / "bare"
        bare.mkdir()
        env = dict(os.environ, NUCLEOS_HEAVY_MAIN=str(bare))
        quiet = self.run_main(bash("go test -run TestX ./...", agent_id="a1"), env)
        self.assertEqual(quiet.returncode, 0)
        self.assertEqual(quiet.stdout.strip(), "")
        loud = self.run_main(bash("cargo build", agent_id="a1"), env)
        self.assertEqual(loud.returncode, 0)
        self.assertEqual(
            json.loads(loud.stdout)["hookSpecificOutput"]["permissionDecision"], "deny")

    def run_main(self, payload, env, raw=None):
        return subprocess.run(
            [sys.executable, str(HERE.parent / "heavy_guard.py")],
            input=raw if raw is not None else json.dumps(payload),
            capture_output=True, text=True, env=env, timeout=60)

    # 14
    def test_passthrough_and_fail_open(self):
        for cmd in (
            "ls -la", "git status", "cargo fmt --all", "echo hi | tail -1", "",
            # Heavy words inside quotes are not commands.
            'echo "a && cargo build"', "echo 'x; cargo test'",
            # A whole line carrying an `export PATH=` is left untouched.
            "export PATH=/x:$PATH && cargo build",
        ):
            with self.subTest(cmd=cmd):
                self.assertIsNone(self.decide(bash(cmd)))
                # The main session only; a subagent naming cargo is denied
                # outright (test_owner_rule_no_cargo_in_subagents).
                if "cargo" not in cmd:
                    self.assertIsNone(self.decide(bash(cmd, agent_id="a1")))
        # Other tools and malformed payloads: nothing, and no exception.
        for payload in (
            {"tool_name": "Read", "tool_input": {"file_path": "x"}},
            {"tool_name": "Bash"},
            {"tool_name": "Bash", "tool_input": None},
            {"tool_name": "Bash", "tool_input": {"command": 5}},
            {"tool_name": "Bash", "tool_input": {"command": "echo 'unterminated"}},
            {},
        ):
            with self.subTest(payload=payload):
                self.assertIsNone(self.decide(payload))
        # main(): bad stdin exits 0 with no stdout.
        env = dict(os.environ, NUCLEOS_HEAVY_MAIN=str(self.main))
        for raw in ("", "not json", "[1, 2]", "null", '{"tool_name": "Bash"'):
            with self.subTest(raw=raw):
                r = self.run_main(None, env, raw=raw)
                self.assertEqual(r.returncode, 0)
                self.assertEqual(r.stdout.strip(), "")
        # main(): a good payload reaches the same rewrite as decide().
        r = self.run_main(bash("go test -run TestX ./...", agent_id="a1"), env)
        self.assertEqual(r.returncode, 0)
        ui = json.loads(r.stdout)["hookSpecificOutput"]["updatedInput"]
        self.assertIn("--prio 2 --agent a1", ui["command"])
        self.assertIn("heavy.py", ui["command"])

    # 15
    def test_never_emits_allow(self):
        payloads = [
            bash("cargo test the_name", agent_id="a1"),
            bash("cargo build"),
            bash("RUST_LOG=1 npm run build"),
            powershell("cargo check"),
            powershell("cargo test the_name", agent_id="a1"),
            bash("cargo build", agent_id="a1"),
            {"tool_name": "Agent", "tool_input": {"isolation": "worktree"}},
        ]
        for payload in payloads:
            with self.subTest(payload=payload):
                out = self.decide(payload)
                self.assertIsNotNone(out)
                spec = out["hookSpecificOutput"]
                self.assertNotEqual(spec.get("permissionDecision"), "allow")
                self.assertNotIn("allow", json.dumps(out).replace("allowed", ""))
                if "updatedInput" in spec:
                    # A rewrite carries no decision at all: it goes through the
                    # normal permission flow.
                    self.assertNotIn("permissionDecision", spec)

    # 16
    def test_owner_rule_no_cargo_in_subagents(self):
        denied = [
            "cargo fmt --all",
            "cargo test -p nucleos-core the_name",
            "cd core && cargo test the_name 2>&1 | tail -5",
            "CARGO_TARGET_DIR=C:/x cargo test the_name",
            "C:/Projects/cargo/bin/cargo.exe test the_name",
            "cargo.exe check",
            "cargo-clippy",
            "rustfmt core/src/main.rs",
            "sh -c 'cargo test the_name'",
            'echo "a && cargo build"',
            "echo 'unterminated cargo test",
            f'"{PY}" "{self.main_posix}/scripts/heavy.py" --prio 2 -- cargo test the_name',
            f'python "{self.main_posix}/scripts/heavy.py" warm',
        ]
        for cmd in denied:
            with self.subTest(cmd=cmd):
                for payload in (bash(cmd, agent_id="a1"), powershell(cmd, agent_id="a1")):
                    out = self.decide(payload)
                    self.assertDenied(out, cmd)
                    self.assertIn("owner rule", out["hookSpecificOutput"]["permissionDecisionReason"])
        # Names that only look like cargo are left alone.
        for cmd in ("cat core/Cargo.toml", "ls C:/Projects/.cargo-target-pool-1",
                    "echo $CARGO_TARGET_DIR", "grep -n cargos x.md"):
            with self.subTest(cmd=cmd):
                self.assertIsNone(self.decide(bash(cmd, agent_id="a1")))
        # The main session keeps running cargo through the broker.
        ui = self.rewritten(self.decide(bash("cargo test the_name")))
        self.assertIn("--prio 1 --agent main", ui["command"])


if __name__ == "__main__":
    unittest.main()
