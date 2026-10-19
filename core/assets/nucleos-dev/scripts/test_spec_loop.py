#!/usr/bin/env python3
"""Tests for spec_loop: the Stop hook that holds a spec session until its checklist is done,
and the report it requires before letting go.

    python test_spec_loop.py -v
"""

from __future__ import annotations

import importlib.util
import json
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location("spec_loop", HERE / "spec_loop.py")
sl = importlib.util.module_from_spec(spec)
spec.loader.exec_module(sl)

REPORT = "## Feito\n- a\n## Falta\nnada\n## Verificação\nx -> 0\n## Próximo passo\nnada\n"


class SpecLoop(unittest.TestCase):
    def setUp(self):
        self._tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self._tmp.cleanup)
        tmp = Path(self._tmp.name)
        self.root = tmp / "project"
        self.state = tmp / "state"
        (self.root / ".nucleos" / "workflow").mkdir(parents=True)
        (self.state / "loops").mkdir(parents=True)
        (self.root / ".nucleos" / "workflow" / "project.json").write_text(
            json.dumps({"id": "t", "state_dir": str(self.state), "bundle_dir": str(HERE.parent)}),
            encoding="utf-8")
        self.loop = self.state / "loops" / "s1.md"
        old = os.environ.get("NUCLEOS_PROJECT_ROOT")
        os.environ["NUCLEOS_PROJECT_ROOT"] = str(self.root)
        self.addCleanup(lambda: os.environ.pop("NUCLEOS_PROJECT_ROOT", None) if old is None
                        else os.environ.__setitem__("NUCLEOS_PROJECT_ROOT", old))

    def write(self, body, status="active"):
        self.loop.write_text(f"# Spec loop\nspec: x.md\nstatus: {status}\n\n## Items\n{body}",
                             encoding="utf-8")

    def stop(self, message="", pushed=False, session="s1"):
        return sl.stop_decision({"session_id": session, "cwd": str(self.root),
                                 "stop_hook_active": pushed, "last_assistant_message": message})

    def test_no_loop_file_lets_the_stop_go(self):
        self.assertIsNone(self.stop())

    def test_another_sessions_loop_is_not_judged(self):
        self.write("- [ ] a\n")
        self.assertIsNone(self.stop(session="s2"))

    def test_open_items_block_and_are_listed(self):
        self.write("- [x] a\n- [ ] b\n")
        out = self.stop(REPORT)
        self.assertEqual(out["decision"], "block")
        self.assertIn("- [ ] b", out["reason"])

    def test_empty_checklist_asks_for_items(self):
        self.write("")
        self.assertIn("no items", self.stop(REPORT)["reason"])

    def test_pushes_that_close_nothing_eventually_let_go(self):
        self.write("- [ ] a\n")
        self.assertEqual(self.stop(REPORT)["decision"], "block")
        self.assertEqual(self.stop(REPORT, pushed=True)["decision"], "block")
        self.assertIsNone(self.stop(REPORT, pushed=True))

    def test_progress_resets_the_idle_count(self):
        self.write("- [ ] a\n- [ ] b\n- [ ] c\n")
        self.stop(REPORT)
        self.stop(REPORT, pushed=True)
        self.write("- [x] a\n- [ ] b\n- [ ] c\n")
        self.assertEqual(self.stop(REPORT, pushed=True)["decision"], "block")

    def test_blocked_needs_a_bang_item(self):
        self.write("- [ ] a\n", status="blocked")
        self.assertEqual(self.stop(REPORT)["decision"], "block")
        self.write("- [ ] a\n- [!] b — precisa de: escolha\n", status="blocked")
        self.assertIsNone(self.stop(REPORT))
        self.assertTrue(self.loop.exists(), "a blocked loop stays open for the next turn")

    def test_handoff_lets_go(self):
        self.write("- [ ] a\n", status="handoff")
        self.assertIsNone(self.stop(REPORT))

    def test_done_without_report_blocks_once_for_it(self):
        self.write("- [x] a\n")
        out = self.stop("feito, tudo bem")
        self.assertIn("Próximo passo", out["reason"])
        self.assertIsNone(self.stop("ainda sem formato", pushed=True))

    def test_done_with_report_closes_the_loop(self):
        self.write("- [x] a\n")
        self.assertIsNone(self.stop(REPORT))
        self.assertFalse(self.loop.exists())
        self.assertTrue(self.loop.with_name("s1.closed.md").exists())

    def test_report_is_not_asked_again_until_the_checklist_moves(self):
        self.write("- [x] a\n- [!] b\n", status="blocked")
        self.assertIsNone(self.stop(REPORT))
        self.assertIsNone(self.stop("resposta curta a outra pergunta"))

    def test_headings_match_without_accents_and_at_any_level(self):
        msg = "### feito\n## FALTA\n# Verificacao\n## Proximo passo: x\n"
        self.assertEqual(sl.missing_headings(msg), [])

    def test_report_read_from_transcript_when_payload_lacks_it(self):
        self.write("- [x] a\n")
        tr = self.root / "t.jsonl"
        tr.write_text("\n".join(json.dumps(r) for r in [
            {"type": "assistant", "message": {"content": [{"type": "text", "text": REPORT}]}},
            {"type": "assistant", "message": {"content": [{"type": "tool_use", "name": "x"}]}},
        ]), encoding="utf-8")
        out = sl.stop_decision({"session_id": "s1", "cwd": str(self.root), "transcript_path": str(tr)})
        self.assertIsNone(out)

    def test_prompt_hook_nudges_only_spec_prompts_without_a_loop(self):
        ask = lambda p: sl.prompt_context({"session_id": "s1", "cwd": str(self.root), "prompt": p})
        self.assertIn('spec_loop.py" start', ask("trabalha na spec specs/x.md"))
        self.assertIsNone(ask("corrige o botão"))
        self.write("- [ ] a\n")
        self.assertIn("1 open", ask("segue"))

    def test_the_suggested_start_command_quotes_the_script_path(self):
        # The library lives under the home directory, which can hold a space ("C:/Users/PC
        # Multimedia/"); unquoted, the suggested command runs a script that does not exist.
        nudge = sl.prompt_context({"session_id": "s1", "cwd": str(self.root), "prompt": "a spec"})
        script = Path(sl.__file__).resolve().as_posix()
        self.assertIn(f'`python "{script}" start ', nudge)

    def test_no_project_json_fails_open(self):
        (self.root / ".nucleos" / "workflow" / "project.json").unlink()
        self.write("- [ ] a\n")
        self.assertIsNone(self.stop())
        self.assertIsNone(sl.prompt_context({"session_id": "s1", "cwd": str(self.root), "prompt": "a spec"}))

    def test_start_without_project_json_fails_loudly(self):
        (self.root / ".nucleos" / "workflow" / "project.json").unlink()
        env = {**os.environ, "CLAUDE_CODE_SESSION_ID": "s9", "NUCLEOS_PROJECT_ROOT": str(self.root)}
        r = subprocess.run([sys.executable, str(HERE / "spec_loop.py"), "start", "x.md", "a"],
                           capture_output=True, text=True, env=env)
        self.assertEqual(r.returncode, 2)
        self.assertIn("project.json", r.stderr)

    def test_start_opens_the_loop_in_the_state_dir(self):
        env = {**os.environ, "CLAUDE_CODE_SESSION_ID": "s9", "NUCLEOS_PROJECT_ROOT": str(self.root)}
        r = subprocess.run([sys.executable, str(HERE / "spec_loop.py"), "start", "x.md", "a", "b"],
                           capture_output=True, text=True, env=env)
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn("- [ ] b", (self.state / "loops" / "s9.md").read_text(encoding="utf-8"))

    def test_hook_cli_fails_open_on_garbage(self):
        r = subprocess.run([sys.executable, str(HERE / "spec_loop.py"), "--stop-hook"],
                           input="not json", capture_output=True, text=True)
        self.assertEqual((r.returncode, r.stdout), (0, ""))


if __name__ == "__main__":
    unittest.main()
