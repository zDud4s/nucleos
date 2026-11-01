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
from datetime import datetime, timezone
from pathlib import Path
from unittest import mock

HERE = Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location("spec_loop", HERE / "spec_loop.py")
sl = importlib.util.module_from_spec(spec)
spec.loader.exec_module(sl)
sys.modules["spec_loop"] = sl
REAL_REPORT_LET_GO = sl.report_let_go
timer_spec = importlib.util.spec_from_file_location("spec_loop_timer", HERE / "spec_loop_timer.py")
timer = importlib.util.module_from_spec(timer_spec)
timer_spec.loader.exec_module(timer)

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
        # A test run must never write to the live daemon's feed.
        patcher = mock.patch.object(sl, "report_let_go")
        self.reported = patcher.start()
        self.addCleanup(patcher.stop)
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

    def let_go(self, body="- [ ] a\n"):
        """Runs the two pushes out, so the hook lets the stop go with the item still open."""
        self.write(body)
        self.stop(REPORT)
        self.stop(REPORT, pushed=True)
        self.assertIsNone(self.stop(REPORT, pushed=True))

    def state_file(self):
        return json.loads(self.loop.with_suffix(".state.json").read_text(encoding="utf-8"))

    def test_letting_go_reports_once_to_the_feed(self):
        self.let_go("- [ ] a\n- [ ] b\n")
        self.assertEqual(self.reported.call_count, 1)
        payload, n_open = self.reported.call_args.args
        self.assertEqual((payload["session_id"], n_open), ("s1", 2))

    def test_a_push_reports_nothing(self):
        self.write("- [ ] a\n")
        self.assertIsNotNone(self.stop(REPORT))
        self.assertIsNotNone(self.stop(REPORT, pushed=True))
        self.reported.assert_not_called()

    def test_the_timers_wake_does_not_report_the_same_let_go_again(self):
        self.let_go()
        state = self.state_file()
        state["woken"] = True
        self.loop.with_suffix(".state.json").write_text(json.dumps(state), encoding="utf-8")
        self.assertIsNone(self.stop(REPORT))
        self.assertEqual(self.reported.call_count, 1)

    def test_a_new_round_of_pushes_that_closes_nothing_reports_again(self):
        self.let_go()
        self.assertIsNotNone(self.stop(REPORT))
        self.assertIsNotNone(self.stop(REPORT, pushed=True))
        self.assertIsNone(self.stop(REPORT, pushed=True))
        self.assertEqual(self.reported.call_count, 2)

    def test_the_report_posts_one_feed_line(self):
        env = {"NUCLEOS_DAEMON_TOKEN": "tok", "NUCLEOS_DAEMON_URL": "http://daemon"}
        with mock.patch.dict(os.environ, env), mock.patch("urllib.request.urlopen") as urlopen:
            REAL_REPORT_LET_GO({"session_id": "s1", "cwd": "C:/p"}, 2)
        request = urlopen.call_args.args[0]
        self.assertEqual((request.full_url, request.get_method()),
                         ("http://daemon/conductor/feed", "POST"))
        self.assertEqual(request.get_header("Authorization"), "Bearer tok")
        body = json.loads(request.data)
        self.assertEqual((body["kind"], body["session_id"], body["cwd"], body["tool_name"]),
                         ("spec_loop_let_go", "s1", "C:/p", "spec loop"))
        self.assertIn("2", body["reason"])

    def test_the_report_is_silent_when_the_daemon_does_not_answer(self):
        with mock.patch.dict(os.environ, {"NUCLEOS_DAEMON_TOKEN": "tok"}),                 mock.patch("urllib.request.urlopen", side_effect=OSError("refused")):
            REAL_REPORT_LET_GO({"session_id": "s1", "cwd": "C:/p"}, 2)

    def test_the_report_posts_nothing_without_a_token(self):
        with mock.patch("rehydrate.find_token", return_value=None),                 mock.patch("urllib.request.urlopen") as urlopen:
            REAL_REPORT_LET_GO({"session_id": "s1", "cwd": "C:/p"}, 2)
        urlopen.assert_not_called()

    def test_letting_go_records_how_many_items_it_left_open(self):
        self.let_go("- [ ] a\n- [ ] b\n")
        self.assertEqual(self.state_file()["let_go"], 2)

    def test_timer_wakes_a_session_let_go_with_open_items_once(self):
        self.let_go()
        message, slept = self.timer([self.answered(1000)], tasks=())
        self.assertIn("open item", message)
        self.assertEqual(slept, 55 * 60)
        self.assertIsNone(self.timer([self.answered(9000)], tasks=(), clock=9000)[0])

    def test_timer_sees_a_let_go_written_after_it_started(self):
        self.write("- [ ] a\n")
        self.stop(REPORT)
        self.stop(REPORT, pushed=True)

        def hook_answers():
            if self.timer_at == 1030:
                self.stop(REPORT, pushed=True)
        message, _ = self.timer([self.answered(1000)], tasks=(), on_sleep=hook_answers)
        self.assertIn("open item", message)

    def test_timer_stays_out_of_a_session_the_hook_did_not_let_go(self):
        self.write("- [ ] a\n")
        self.assertIsNone(self.timer([self.answered(1000)], tasks=())[0])

    def test_timer_does_not_wake_a_let_go_session_that_got_a_new_turn(self):
        self.let_go()
        records = [self.answered(1000)]

        def new_turn():
            records.append(self.answered(1020))
            self.transcript(*records)
        self.assertIsNone(self.timer(records, tasks=(), on_sleep=new_turn)[0])

    def test_the_wake_after_a_let_go_gets_no_pushes_of_its_own(self):
        self.let_go()
        self.timer([self.answered(1000)], tasks=())
        self.assertIsNone(self.stop(REPORT))

    def test_progress_after_the_wake_earns_the_pushes_back(self):
        self.let_go("- [ ] a\n- [ ] b\n")
        self.timer([self.answered(1000)], tasks=())
        self.write("- [x] a\n- [ ] b\n")
        self.assertEqual(self.stop(REPORT)["decision"], "block")

    def test_a_new_prompt_after_the_wake_earns_another_wake(self):
        self.let_go()
        self.timer([self.answered(1000)], tasks=())
        self.stop(REPORT)  # the woken turn, let go at once
        self.stop(REPORT)  # the owner's next prompt, pushed again
        self.stop(REPORT, pushed=True)
        self.assertIsNone(self.stop(REPORT, pushed=True))
        self.assertIn("open item", self.timer([self.answered(9000)], tasks=(), clock=9000)[0])

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

    def stop_with(self, tasks, message="A aguardar.", pushed=False):
        return sl.stop_decision({"session_id": "s1", "cwd": str(self.root), "stop_hook_active": pushed,
                                 "last_assistant_message": message, "background_tasks": tasks})

    @staticmethod
    def task(task="b1", status="running", kind="shell"):
        return {"id": task, "type": kind, "status": status, "description": "x"}

    def test_running_background_work_lets_the_stop_go_with_no_report(self):
        self.write("- [ ] a\n")
        for kind in ("shell", "subagent", "monitor", "workflow"):
            self.assertIsNone(self.stop_with([self.task(kind=kind)]), kind)
        self.assertTrue(self.loop.exists(), "the loop stays open for when the work wakes the session")

    def test_no_running_background_work_pushes(self):
        self.write("- [ ] a\n")
        for tasks in ([], [self.task(status="completed")], None, "garbage", ["garbage"]):
            self.assertEqual(self.stop_with(tasks)["decision"], "block", tasks)

    def test_one_task_still_running_holds_the_push_back(self):
        self.write("- [ ] a\n")
        self.assertIsNone(self.stop_with([self.task("a", "completed"), self.task("b")]))

    def test_background_work_does_not_excuse_a_finished_checklist_from_its_report(self):
        self.write("- [x] a\n")
        out = self.stop_with([self.task()], message="feito")
        self.assertIn("Próximo passo", out["reason"])

    def transcript(self, *records):
        tr = self.root / "t.jsonl"
        tr.write_text("\n".join(json.dumps(r) for r in records) + "\n", encoding="utf-8")
        return str(tr)

    @staticmethod
    def answered(at, cache_kind="ephemeral_1h_input_tokens", tokens=10, **extra):
        when = datetime.fromtimestamp(at, timezone.utc).isoformat().replace("+00:00", "Z")
        usage = {"cache_creation": {cache_kind: tokens}}
        return {"type": "assistant", "timestamp": when, "message": {"usage": usage}, **extra}

    def timer(self, records, tasks=("b1",), clock=None, on_sleep=None):
        """Runs the timer on a fake clock that starts at 1000; returns (message, total slept)."""
        self.timer_at = 1000 if clock is None else clock
        slept = []

        def sleep(secs):
            slept.append(secs)
            self.timer_at += secs
            if on_sleep:
                on_sleep()
        message = timer.wake_message(
            {"session_id": "s1", "cwd": str(self.root), "transcript_path": self.transcript(*records),
             "background_tasks": [self.task(t) for t in tasks]},
            now=lambda: self.timer_at, sleep=sleep)
        return message, sum(slept)

    def test_timer_wakes_an_idle_session_just_before_the_cache_expires(self):
        self.write("- [ ] a\n")
        message, slept = self.timer([self.answered(1000)])
        self.assertIn("b1", message)
        self.assertEqual(slept, 55 * 60)

    def test_timer_uses_the_ttl_the_last_write_went_out_with(self):
        self.write("- [ ] a\n")
        _, slept = self.timer([self.answered(1000, "ephemeral_5m_input_tokens")])
        self.assertEqual(slept, 4 * 60)

    def test_timer_looks_past_a_request_that_only_read_the_cache(self):
        self.write("- [ ] a\n")
        _, slept = self.timer([self.answered(900, "ephemeral_5m_input_tokens"),
                               self.answered(1000, tokens=0)])
        self.assertEqual(slept, 4 * 60)

    def test_timer_measures_from_the_last_request_not_from_its_own_start(self):
        self.write("- [ ] a\n")
        _, slept = self.timer([self.answered(1000)], clock=1000 + 50 * 60)
        self.assertEqual(slept, 5 * 60)

    def test_timer_ignores_a_subagents_records(self):
        self.write("- [ ] a\n")
        _, slept = self.timer([self.answered(1000), self.answered(1500, isSidechain=True)])
        self.assertEqual(slept, 55 * 60)

    def test_timer_bows_out_when_the_session_moves_on(self):
        self.write("- [ ] a\n")
        tr = self.root / "t.jsonl"

        def turn():
            with tr.open("a", encoding="utf-8") as f:
                f.write(json.dumps(self.answered(1100)) + "\n")
        message, slept = self.timer([self.answered(1000)], on_sleep=turn)
        self.assertIsNone(message)
        self.assertEqual(slept, timer.POLL_SECS)

    def test_timer_stays_quiet_with_nothing_to_wait_for(self):
        self.write("- [ ] a\n")
        self.assertEqual(self.timer([self.answered(1000)], tasks=()), (None, timer.LET_GO_GRACE_SECS))
        self.write("- [x] a\n")
        self.assertEqual(self.timer([self.answered(1000)]), (None, 0))
        self.write("- [ ] a\n- [!] b\n", status="blocked")
        self.assertEqual(self.timer([self.answered(1000)]), (None, 0))
        self.loop.unlink()
        self.assertEqual(self.timer([self.answered(1000)]), (None, 0))

    def test_timer_stays_quiet_if_the_loop_closed_while_it_slept(self):
        self.write("- [ ] a\n")
        message, _ = self.timer([self.answered(1000)], on_sleep=lambda: self.write("- [x] a\n"))
        self.assertIsNone(message)

    def test_timer_stops_waking_once_unchanged_tasks_outlast_the_budget(self):
        self.write("- [ ] a\n")
        self.assertIsNotNone(self.timer([self.answered(1000)])[0])
        end = self.timer_at + timer.MAX_WAKE_SECS
        self.assertIsNotNone(self.timer([self.answered(1000)], clock=end - 1)[0])
        self.assertEqual(self.timer([self.answered(1000)], clock=end), (None, 0))
        self.assertIsNotNone(self.timer([self.answered(1000)], tasks=("b2",), clock=end)[0],
                             "new work resets it")

    def test_timer_budget_is_time_so_a_short_cache_gets_the_same_hours(self):
        self.write("- [ ] a\n")
        at = 1000
        for _ in range(20):
            message, _ = self.timer([self.answered(at, "ephemeral_5m_input_tokens")], clock=at)
            self.assertIsNotNone(message)
            at = self.timer_at

    def test_timer_treats_a_count_left_by_an_older_version_as_a_new_budget(self):
        self.write("- [ ] a\n")
        self.timer([self.answered(1000)])
        state_path = self.loop.with_suffix(".state.json")
        state = json.loads(state_path.read_text())
        state["wakes"] = {"tasks": state["wakes"]["tasks"], "count": 6}
        state_path.write_text(json.dumps(state))
        self.assertIsNotNone(self.timer([self.answered(1000)], clock=1000 + 100 * 3600)[0])

    def test_a_wake_leaves_the_stop_hooks_state_alone(self):
        self.write("- [ ] a\n")
        self.stop(REPORT)
        self.timer([self.answered(1000)])
        self.assertIn("last_block", json.loads(self.loop.with_suffix(".state.json").read_text()))

    def test_timer_exits_2_only_with_a_wake_and_stays_silent_otherwise(self):
        self.write("- [ ] a\n")
        tr = self.transcript(self.answered(1000))
        run = lambda tasks: subprocess.run(
            [sys.executable, str(HERE / "spec_loop_timer.py")], capture_output=True, text=True,
            input=json.dumps({"session_id": "s1", "cwd": str(self.root), "transcript_path": tr,
                              "background_tasks": tasks}))
        idle = run([self.task()])
        self.assertEqual(idle.returncode, 2, idle.stderr)
        self.assertIn("prompt cache", idle.stderr)
        quiet = run([])
        self.assertEqual((quiet.returncode, quiet.stderr), (0, ""))
        broken = subprocess.run([sys.executable, str(HERE / "spec_loop_timer.py")],
                                capture_output=True, input=b"not json")
        self.assertEqual(broken.returncode, 0)

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

    def run_start(self, session, *args, name_env=None):
        env = {**os.environ, "CLAUDE_CODE_SESSION_ID": session, "NUCLEOS_PROJECT_ROOT": str(self.root)}
        env.pop("NUCLEOS_LOOP_NAME", None)
        if name_env is not None:
            env["NUCLEOS_LOOP_NAME"] = name_env
        return subprocess.run([sys.executable, str(HERE / "spec_loop.py"), "start", *args],
                              capture_output=True, text=True, env=env)

    def test_start_name_flag_replaces_the_session_id_in_the_file_name(self):
        r = self.run_start("s9", "--name", "branch-a", "x.md", "a")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn("- [ ] a", (self.state / "loops" / "branch-a.md").read_text(encoding="utf-8"))
        self.assertFalse((self.state / "loops" / "s9.md").exists())

    def test_start_name_env_replaces_the_session_id_in_the_file_name(self):
        r = self.run_start("s9", "x.md", "a", name_env="branch-b")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertTrue((self.state / "loops" / "branch-b.md").exists())
        self.assertFalse((self.state / "loops" / "s9.md").exists())

    def test_start_name_flag_wins_over_the_env(self):
        r = self.run_start("s9", "--name", "from-flag", "x.md", "a", name_env="from-env")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertTrue((self.state / "loops" / "from-flag.md").exists())
        self.assertFalse((self.state / "loops" / "from-env.md").exists())

    def test_start_name_does_not_leak_into_the_spec_and_items(self):
        r = self.run_start("s9", "--name", "n", "x.md", "a", "b")
        self.assertEqual(r.returncode, 0, r.stderr)
        text = (self.state / "loops" / "n.md").read_text(encoding="utf-8")
        self.assertIn("spec: x.md", text)
        self.assertNotIn("--name", text)
        self.assertIn("- [ ] b", text)

    def test_start_refuses_a_named_loop_owned_by_another_session(self):
        first = self.run_start("s1", "--name", "shared", "x.md", "a")
        self.assertEqual(first.returncode, 0, first.stderr)
        before = (self.state / "loops" / "shared.md").read_text(encoding="utf-8")
        r = self.run_start("s2", "--name", "shared", "y.md", "other")
        self.assertEqual(r.returncode, 2)
        self.assertIn("shared", r.stdout + r.stderr)
        self.assertEqual((self.state / "loops" / "shared.md").read_text(encoding="utf-8"), before)

    def test_start_by_the_owning_session_again_is_still_a_no_op(self):
        self.run_start("s1", "--name", "shared", "x.md", "a")
        r = self.run_start("s1", "--name", "shared", "x.md", "a")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn("already open", r.stdout)

    def test_hooks_still_look_loops_up_by_session_id_only(self):
        # A named loop is not found by a session whose id differs from the name.
        (self.state / "loops" / "n.md").write_text(
            "# Spec loop\nspec: x.md\nstatus: active\n\n## Items\n- [ ] a\n", encoding="utf-8")
        self.assertIsNone(self.stop(session="s1"))
        self.assertIsNone(sl.prompt_context({"session_id": "s1", "cwd": str(self.root), "prompt": "hi"}))

    def test_hook_cli_fails_open_on_garbage(self):
        r = subprocess.run([sys.executable, str(HERE / "spec_loop.py"), "--stop-hook"],
                           input="not json", capture_output=True, text=True)
        self.assertEqual((r.returncode, r.stdout), (0, ""))

    def test_hook_cli_reads_the_accented_report_as_utf8(self):
        # Claude Code writes the payload as UTF-8; Windows' default stdin code page is not.
        self.write("- [x] a\n")
        payload = {"session_id": "s1", "cwd": str(self.root), "last_assistant_message": REPORT}
        env = {**os.environ, "PYTHONIOENCODING": "cp1252"}
        r = subprocess.run([sys.executable, str(HERE / "spec_loop.py"), "--stop-hook"],
                           input=json.dumps(payload, ensure_ascii=False).encode("utf-8"),
                           capture_output=True, env=env)
        self.assertEqual((r.returncode, r.stdout), (0, b""), r.stderr)


if __name__ == "__main__":
    unittest.main()
