"""The rewritten heavy command must start with a literal bare-word program name.

Claude Code's worktree-isolation guard refuses a command whose program name is
quoted ("computed at runtime"); the name must match BARE below. A space in the
interpreter path used to force quotes, so the rewrite has to fall back to a
bare `python`/`python3` (or an 8.3 short path on Windows) instead."""

import re
import sys
import unittest
from unittest import mock

import test_heavy_guard as base

BARE = re.compile(r"^[A-Za-z0-9_.\/+:@~][A-Za-z0-9_.\/+:@~-]*$")
ASSIGN = re.compile(r"^[A-Za-z_][A-Za-z0-9_]*=\S*$")
SPACE_PY = "C:/Nonexistent Dir/Python311/python.exe"
FALLBACK = "python" if sys.platform == "win32" else "python3"


def first_program(command):
    for tok in command.split():
        if not ASSIGN.match(tok):
            return tok
    raise AssertionError(f"no program token in {command!r}")


class LiteralProgramTests(base.HeavyGuardTests):
    # Reuse setUp/decide/rewritten helpers only; do not re-run the parent's tests.
    def run_decide(self, payload, py=SPACE_PY):
        with mock.patch.object(sys, "executable", py):
            return self.hg.decide(payload, self.main, True, py)

    def command_of(self, payload, py=SPACE_PY):
        return self.rewritten(self.run_decide(payload, py))["command"]

    def test_bash_main_first_token_is_bare(self):
        cmd = self.command_of(base.bash("cargo test -p nucleos-core foo"))
        tok = first_program(cmd)
        self.assertRegex(tok, BARE, cmd)
        self.assertEqual(tok, FALLBACK, cmd)

    def test_powershell_has_no_call_operator_and_bare_first_token(self):
        cmd = self.command_of(base.powershell("cargo test -p nucleos-core foo"))
        self.assertFalse(cmd.lstrip().startswith("&"), cmd)
        self.assertRegex(first_program(cmd), BARE, cmd)

    def test_subagent_env_prefix_kept_and_program_bare(self):
        cmd = self.command_of(
            base.bash("GOFLAGS=-v go test -run TestFoo ./...", agent_id="a1"))
        self.assertTrue(cmd.startswith("GOFLAGS=-v "), cmd)
        self.assertRegex(first_program(cmd), BARE, cmd)
        self.assertIn("-- go test -run TestFoo ./...", cmd)

    def test_space_free_interpreter_is_used_unquoted(self):
        py = "C:/Python311/python.exe"
        cmd = self.command_of(base.bash("cargo test -p nucleos-core foo"), py)
        self.assertEqual(first_program(cmd), py, cmd)
        self.assertRegex(first_program(cmd), BARE, cmd)


# Do not collect the inherited tests of the parent class under this module.
for _name in [n for n in dir(base.HeavyGuardTests) if n.startswith("test_")]:
    if _name not in LiteralProgramTests.__dict__:
        setattr(LiteralProgramTests, _name, None)

if __name__ == "__main__":
    unittest.main()
