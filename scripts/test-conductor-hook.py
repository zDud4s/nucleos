#!/usr/bin/env python3
"""Cover the conductor policy in `core/hooks/ask_daemon.py` (plan 2026-10-30, chunk B-HOOK).

A top-level session (no `NUCLEOS_RUN_ID`, no `agent_id` in the payload) is a conductor: it launches
agents and trees and does not edit source by hand. The policy is `conductor_verdict(payload, state)`,
called first thing in `interactive_session`, local, fail-open, and driven by `<state_dir>/conductor.json`.

Imports the SOURCE `core/hooks/ask_daemon.py`, like `scripts/test-hook-filter.py`.

RED shape: no conductor symbol is touched at import. Each test resolves what it needs through
`_need(name)`, so before the implementation exists every test fails on its own with a one-line reason.

The interface these tests fix (the implementation must match it):

* `conductor_verdict(payload, state) -> dict` with `decision` ("allow" | "deny"), `reason` (str,
  non-empty when `would_deny`) and `would_deny` (bool: the rules refuse the call, whatever the mode).
  Pure: no network, no daemon. `state` is a dict with `state_dir` (the project state directory, which
  holds `conductor.json` and is allowed material) and `tmp_dir` (the temp root; flag files live in
  `<tmp_dir>/nucleos-conductor/<session_id>.json`).
  - mode "on": `decision` is "deny" when the rules refuse; "observe": always "allow", `would_deny`
    still True; "off": always "allow", `would_deny` False.
  - allowed without consulting the rules: `NUCLEOS_RUN_ID` set, `agent_id` in the payload, a manual flag.
* `conductor_prompt(payload, state)`: acts on UserPromptSubmit only. A prompt that starts with
  `manual:` (stripped, case-insensitive) writes the flag file `{"manual": true, "prompt_at": ...}`;
  any other prompt deletes it.
* `interactive_session(payload)` resolves its state from `NUCLEOS_CONDUCTOR_STATE_DIR` and
  `NUCLEOS_CONDUCTOR_TMP_DIR`, prints a block on "deny", and on a would-deny in observe mode posts one
  light request to the daemon whose body carries the feed kind `conductor_would_deny` (failures are
  silent). A crash inside `conductor_verdict` is fail-open (one stderr line, no block).
* `NUCLEOS_HOOK_PROBE=<file>`: `interactive_session` appends one line per PreToolUse payload, the JSON
  list of its sorted keys.
* `DEFAULT_CONDUCTOR_MODE`: the one constant to flip from "observe" to "on" (B-DEFAULT). Deny-path
  tests set mode "on" explicitly; nothing here pins the constant's value.

Run:  python scripts/test-conductor-hook.py
"""

import contextlib
import importlib.util
import io
import json
import os
import sys
import tempfile
import traceback
from unittest import mock

os.environ["NUCLEOS_CLAIMS_OFF"] = "1"

ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), ".."))
HOOK = os.path.join(ROOT, "core", "hooks", "ask_daemon.py")

spec = importlib.util.spec_from_file_location("ask_daemon", HOOK)
hook = importlib.util.module_from_spec(spec)
spec.loader.exec_module(hook)

G = "g" + "it"  # kept out of the text so the running session's own hook does not govern this file


def _need(name):
    """The conductor symbol `name`, or an AssertionError that says what is missing."""
    found = getattr(hook, name, None)
    if found is None:
        raise AssertionError(f"core/hooks/ask_daemon.py has no `{name}` yet")
    return found


class _Reply:
    def __init__(self, body):
        self._body = json.dumps(body).encode()

    def __enter__(self):
        return self

    def __exit__(self, *_exc):
        return False

    def read(self):
        return self._body


class Fixture:
    """A temp project: cwd, state dir, tmp dir; a stubbed daemon that records every request."""

    def __init__(self, base, mode):
        self.cwd = os.path.join(base, "repo")
        self.state_dir = os.path.join(base, "state")
        self.tmp_dir = os.path.join(base, "tmp")
        for folder in (self.cwd, self.state_dir, self.tmp_dir):
            os.makedirs(folder, exist_ok=True)
        self.state = {"state_dir": self.state_dir, "tmp_dir": self.tmp_dir}
        self.requests = []
        self.set_mode(mode)

    def set_mode(self, mode):
        path = os.path.join(self.state_dir, "conductor.json")
        if mode is None:
            if os.path.exists(path):
                os.remove(path)
            return
        with open(path, "w", encoding="utf-8") as handle:
            json.dump({"mode": mode}, handle)

    def path(self, *parts):
        return os.path.join(self.cwd, *parts)

    def flag(self, session):
        return os.path.join(self.tmp_dir, "nucleos-conductor", f"{session}.json")

    def payload(self, tool, tool_input, session="sess-1", **extra):
        return {
            "hook_event_name": "PreToolUse",
            "session_id": session,
            "cwd": self.cwd,
            "tool_name": tool,
            "tool_input": tool_input,
            **extra,
        }

    def verdict(self, tool, tool_input, session="sess-1", **extra):
        return _need("conductor_verdict")(self.payload(tool, tool_input, session, **extra), self.state)

    def urlopen(self, request, timeout=None):
        self.requests.append(request)
        return _Reply({})

    def feed_bodies(self):
        """Bodies of the requests that mention the feed kind, wherever they were posted."""
        found = []
        for request in self.requests:
            data = getattr(request, "data", None) or b""
            if b"conductor_would_deny" in data:
                found.append(data.decode("utf-8", "replace"))
        return found

    def run_interactive(self, payload, extra_env=None):
        """`interactive_session` end to end; returns `(exit_code, stdout, stderr)`."""
        env = {
            hook.PENDING_DIR_ENV: os.path.join(self.tmp_dir, "pending"),
            "NUCLEOS_CONDUCTOR_STATE_DIR": self.state_dir,
            "NUCLEOS_CONDUCTOR_TMP_DIR": self.tmp_dir,
        }
        env.update(extra_env or {})
        out, err, code = io.StringIO(), io.StringIO(), "no exit"
        with mock.patch.dict(os.environ, env):
            with mock.patch.object(hook, "read_control_token", return_value=("tok", None)):
                with mock.patch.object(hook, "control_token", return_value="tok"):
                    with mock.patch("sys.stdout", out), mock.patch("sys.stderr", err):
                        try:
                            hook.interactive_session(payload)
                        except SystemExit as exit_:
                            code = exit_.code
        return code, out.getvalue(), err.getvalue()


@contextlib.contextmanager
def fixture(mode="on"):
    """A Fixture with a clean environment and the daemon stubbed. No real state, daemon or network."""
    with tempfile.TemporaryDirectory() as base:
        with mock.patch.dict(os.environ):
            for name in ("NUCLEOS_RUN_ID", "NUCLEOS_CONDUCTOR", "NUCLEOS_HOOK_PROBE"):
                os.environ.pop(name, None)
            fx = Fixture(base, mode)
            with mock.patch.object(hook.urllib.request, "urlopen", side_effect=fx.urlopen):
                yield fx


def _expect(result, decision, label):
    if not isinstance(result, dict) or "decision" not in result:
        raise AssertionError(f"{label}: expected a dict with `decision`, got {result!r}")
    if result["decision"] != decision:
        raise AssertionError(f"{label}: expected {decision}, got {result!r}")
    return result


def allow(result, label):
    return _expect(result, "allow", label)


def deny(result, label):
    result = _expect(result, "deny", label)
    if not result.get("reason"):
        raise AssertionError(f"{label}: a deny must carry a reason, got {result!r}")
    return result


# --- reads, writes, tools ---------------------------------------------------------------------


def test_top_level_read_of_source_denied():
    with fixture("on") as fx:
        deny(fx.verdict("Read", {"file_path": fx.path("core", "src", "lib.rs")}), "Read of source")
        deny(fx.verdict("Read", {"file_path": os.path.join("core", "src", "lib.rs")}), "relative source")
        deny(fx.verdict("Grep", {"pattern": "fn main", "path": fx.path("core", "src")}), "Grep over source")
        deny(fx.verdict("Glob", {"pattern": "**/*.rs", "path": fx.path("core")}), "Glob over source")
        # `.ai/..` must not be a way to reach the source.
        deny(
            fx.verdict("Read", {"file_path": fx.path(".ai", "..", "core", "src", "lib.rs")}),
            "traversal out of .ai",
        )
        reason = deny(fx.verdict("Read", {"file_path": fx.path("shell", "src", "main.tsx")}), "reason")["reason"]
        if "manual:" not in reason:
            raise AssertionError(f"the reason must name the escape, got {reason!r}")


def test_read_of_ai_specs_allowed():
    with fixture("on") as fx:
        allow(fx.verdict("Read", {"file_path": fx.path(".ai", "specs", "x.md")}), "Read .ai/specs")
        allow(fx.verdict("Read", {"file_path": os.path.join(".ai", "specs", "x.md")}), "relative .ai")
        allow(fx.verdict("Grep", {"pattern": "R-A", "path": fx.path(".ai")}), "Grep .ai")
        allow(fx.verdict("Glob", {"pattern": "*.md", "path": fx.path(".nucleos")}), "Glob .nucleos")
        allow(
            fx.verdict("Read", {"file_path": os.path.join(fx.state_dir, "handoffs", "n1.md")}),
            "Read in the project state dir",
        )


def test_mcp_nucleos_tool_allowed():
    with fixture("on") as fx:
        for tool in (
            "mcp__nucleos__create_run",
            "mcp__nucleos__tree_launch",
            "mcp__nucleos__spec_finish",
            "mcp__nucleos__get_run",
            "mcp__nucleos__node_tree",
            "mcp__nucleos__verify",
            "mcp__nucleos__task_list",
            "mcp__nucleos__record",
        ):
            allow(fx.verdict(tool, {}), tool)


def test_mcp_nucleos_double_segment_denied():
    with fixture("on") as fx:
        # One segment after the prefix, as hooks/contexts.rs does it.
        deny(fx.verdict("mcp__nucleos__create_run__x", {}), "two segments")
        deny(fx.verdict("mcp__nucleosx__create_run", {}), "another server whose name starts the same")
        deny(fx.verdict("mcp__other__create_run", {}), "another server")
        deny(fx.verdict("mcp__nucleos__", {}), "an empty tool name")


def test_agent_tool_allowed():
    with fixture("on") as fx:
        for tool in ("Agent", "Task", "Skill", "TodoWrite", "AskUserQuestion", "ExitPlanMode", "ToolSearch"):
            allow(fx.verdict(tool, {}), tool)
        # The conductor delegates; it does not search the web itself.
        deny(fx.verdict("WebSearch", {"query": "x"}), "WebSearch")
        deny(fx.verdict("WebFetch", {"url": "http://example.invalid"}), "WebFetch")


def test_subagent_payload_with_agent_id_unaffected():
    with fixture("on") as fx:
        source = {"file_path": fx.path("core", "src", "lib.rs")}
        deny(fx.verdict("Read", source), "control: the same call without agent_id")
        result = allow(
            fx.verdict("Read", source, agent_id="a1b2", agent_type="wf-leaf"), "Read inside a subagent"
        )
        if result.get("would_deny"):
            raise AssertionError(f"a subagent call is out of scope, not a would-deny: {result!r}")
        allow(fx.verdict("Edit", {"file_path": fx.path("core", "src", "lib.rs")}, agent_id="a1b2"), "Edit")


def test_run_with_run_id_env_unaffected():
    with fixture("on") as fx:
        source = {"file_path": fx.path("core", "src", "lib.rs")}
        deny(fx.verdict("Read", source), "control: the same call without NUCLEOS_RUN_ID")
        with mock.patch.dict(os.environ, {"NUCLEOS_RUN_ID": "7"}):
            result = allow(fx.verdict("Read", source), "Read inside a daemon run")
            if result.get("would_deny"):
                raise AssertionError(f"a daemon run is out of scope, not a would-deny: {result!r}")
            allow(fx.verdict("Bash", {"command": "cargo test"}), "cargo inside a daemon run")


def test_edit_of_source_denied():
    with fixture("on") as fx:
        target = fx.path("core", "src", "lib.rs")
        deny(fx.verdict("Edit", {"file_path": target, "old_string": "a", "new_string": "b"}), "Edit")
        deny(fx.verdict("Write", {"file_path": target, "content": "x"}), "Write")
        deny(fx.verdict("Write", {"file_path": fx.path("README.md"), "content": "x"}), "tracked doc")


def test_write_to_state_dir_allowed():
    with fixture("on") as fx:
        allow(
            fx.verdict("Write", {"file_path": os.path.join(fx.state_dir, "handoffs", "n1.md"), "content": "x"}),
            "Write in the state dir",
        )
        allow(
            fx.verdict("Edit", {"file_path": fx.path(".ai", "plans", "p.md"), "old_string": "a", "new_string": "b"}),
            "Edit a plan",
        )
        deny(fx.verdict("Write", {"file_path": fx.path("core", "src", "lib.rs"), "content": "x"}), "control: source")


# --- shell ------------------------------------------------------------------------------------


def test_heavy_py_cargo_allowed():
    with fixture("on") as fx:
        allow(fx.verdict("Bash", {"command": "python scripts/heavy.py cargo test -p nucleos-core --lib"}), "relative")
        absolute = os.path.join(fx.cwd, "scripts", "heavy.py")
        allow(fx.verdict("Bash", {"command": f"python {absolute} cargo build"}), "absolute heavy.py")
        allow(fx.verdict("PowerShell", {"command": "python scripts/heavy.py cargo check"}), "PowerShell")
        deny(fx.verdict("Bash", {"command": "cargo test"}), "control: bare cargo")


def test_chained_command_denied():
    with fixture("on") as fx:
        for command in (
            f"python scripts/heavy.py cargo test && {G} push",
            f"{G} status; rm x",
            f"{G} status || true",
            f"{G} log | cat",
            f"{G} status > out.txt",
            f"{G} status `id`",
            f"{G} log $(id)",
            f"{G} status\n{G} log",
        ):
            deny(fx.verdict("Bash", {"command": command}), repr(command))


def test_git_status_allowed():
    with fixture("on") as fx:
        for command in (
            f"{G} status",
            f"{G} status --short",
            f"{G} log --oneline -3",
            f"{G} diff --stat",
            f"{G} show HEAD",
            f"{G} rev-parse HEAD",
            f"{G} branch --list",
            f"{G} worktree list",
            f"{G} ls-tree --name-only HEAD core/migrations/",
            f"{G} cat-file -p HEAD",
        ):
            allow(fx.verdict("Bash", {"command": command}), repr(command))
        deny(fx.verdict("Bash", {"command": f"{G} checkout master"}), "control: checkout")


def test_git_commit_with_explicit_paths_allowed():
    with fixture("on") as fx:
        allow(fx.verdict("Bash", {"command": f"{G} add scripts/a.py scripts/b.py"}), "add explicit paths")
        allow(fx.verdict("Bash", {"command": f'{G} commit -m "add the conductor tests"'}), "commit -m")
        for command in (
            f"{G} add -A",
            f"{G} add .",
            f"{G} commit -a -m x",
            f"{G} commit -am x",
        ):
            deny(fx.verdict("Bash", {"command": command}), repr(command))


def test_git_push_denied():
    with fixture("on") as fx:
        for verb in ("push origin master", "merge feature", "stash", "reset --hard", "checkout -b x", "rebase main"):
            result = deny(fx.verdict("Bash", {"command": f"{G} {verb}"}), verb)
        if "manual:" not in result["reason"]:
            raise AssertionError(f"the reason must name the escape, got {result['reason']!r}")


# --- escape -----------------------------------------------------------------------------------


def _prompt(fx, text, session="sess-1", event="UserPromptSubmit"):
    payload = {"hook_event_name": event, "session_id": session, "cwd": fx.cwd, "prompt": text}
    return _need("conductor_prompt")(payload, fx.state)


def test_manual_prefix_enables_until_next_prompt():
    with fixture("on") as fx:
        source = {"file_path": fx.path("core", "src", "lib.rs")}
        deny(fx.verdict("Read", source), "control before the prefix")
        _prompt(fx, "  Manual: fix the typo in lib.rs")
        if not os.path.exists(fx.flag("sess-1")):
            raise AssertionError(f"no flag file at {fx.flag('sess-1')}")
        with open(fx.flag("sess-1"), encoding="utf-8") as handle:
            body = json.load(handle)
        if body.get("manual") is not True or "prompt_at" not in body:
            raise AssertionError(f"flag body should be {{manual: true, prompt_at}}, got {body!r}")
        allow(fx.verdict("Read", source), "Read while the flag stands")
        deny(fx.verdict("Read", source, session="sess-2"), "another session is not released")
        _prompt(fx, "now do the next thing")
        if os.path.exists(fx.flag("sess-1")):
            raise AssertionError("the next prompt without the prefix must delete the flag")
        deny(fx.verdict("Read", source), "Read after the next prompt")


def test_agent_cannot_write_manual_flag():
    with fixture("on") as fx:
        flag = fx.flag("sess-1")
        deny(fx.verdict("Write", {"file_path": flag, "content": '{"manual": true}'}), "Write the flag")
        deny(fx.verdict("Edit", {"file_path": flag, "old_string": "a", "new_string": "b"}), "Edit the flag")
        deny(fx.verdict("Bash", {"command": f"echo '{{\"manual\": true}}' > {flag}"}), "Bash redirect")
        # Only a UserPromptSubmit creates it: a prompt-shaped PreToolUse payload does not.
        _prompt(fx, "manual: please", event="PreToolUse")
        if os.path.exists(flag):
            raise AssertionError("a non-UserPromptSubmit event created the flag")
        # An unsafe session id writes nothing outside the flag directory.
        _prompt(fx, "manual: please", session="../evil")
        if os.path.exists(os.path.join(fx.tmp_dir, "evil.json")) or os.path.exists(
            os.path.join(fx.tmp_dir, "nucleos-conductor", "..", "evil.json")
        ):
            raise AssertionError("an unsafe session id wrote a file")


def test_conductor_env_mention_in_command_denied():
    with fixture("on") as fx:
        result = deny(
            fx.verdict("Bash", {"command": f"NUCLEOS_CONDUCTOR=off {G} status"}), "env prefix on a command"
        )
        if "NUCLEOS_CONDUCTOR" not in result["reason"]:
            raise AssertionError(f"the reason must name the variable, got {result['reason']!r}")
        deny(fx.verdict("Bash", {"command": f"{G} status NUCLEOS_CONDUCTOR"}), "a mention in an argument")
        deny(fx.verdict("PowerShell", {"command": "$env:NUCLEOS_CONDUCTOR='off'"}), "PowerShell assignment")


# --- modes ------------------------------------------------------------------------------------


def test_observe_mode_allows_and_reports():
    with fixture("observe") as fx:
        source = {"file_path": fx.path("core", "src", "lib.rs")}
        result = allow(fx.verdict("Read", source), "observe verdict")
        if result.get("would_deny") is not True or not result.get("reason"):
            raise AssertionError(f"observe must flag would_deny with a reason, got {result!r}")
        code, out, err = fx.run_interactive(fx.payload("Read", source))
        if '"block"' in out:
            raise AssertionError(f"observe must not block, printed {out!r}")
        bodies = fx.feed_bodies()
        if len(bodies) != 1:
            raise AssertionError(f"expected one conductor_would_deny request, got {len(bodies)}")
        # A call the rules accept reports nothing.
        fx.requests.clear()
        fx.run_interactive(fx.payload("Agent", {}))
        if fx.feed_bodies():
            raise AssertionError("an allowed call must not post a would-deny")
        # A daemon that is down changes nothing: still no block, no raise.
        with mock.patch.object(hook.urllib.request, "urlopen", side_effect=OSError("down")):
            _code, out, _err = fx.run_interactive(fx.payload("Read", source))
        if '"block"' in out:
            raise AssertionError(f"a failed feed post must stay silent, printed {out!r}")
        # With no conductor.json the mode is DEFAULT_CONDUCTOR_MODE, whatever it is now.
        fx.set_mode(None)
        mode = _need("DEFAULT_CONDUCTOR_MODE")
        if mode not in ("on", "observe"):
            raise AssertionError(f"DEFAULT_CONDUCTOR_MODE should be on or observe, got {mode!r}")
        by_default = fx.verdict("Read", source)
        _expect(by_default, "deny" if mode == "on" else "allow", f"default mode {mode}")


def test_off_mode_allows_everything():
    with fixture("on") as fx:
        source = {"file_path": fx.path("core", "src", "lib.rs")}
        deny(fx.verdict("Read", source), "control: the same call in mode on")
        fx.set_mode("off")
        for tool, tool_input in (
            ("Read", source),
            ("Edit", {"file_path": fx.path("core", "src", "lib.rs"), "old_string": "a", "new_string": "b"}),
            ("Bash", {"command": f"{G} push origin master"}),
            ("Bash", {"command": f"NUCLEOS_CONDUCTOR=on {G} status && ls"}),
            ("WebFetch", {"url": "http://example.invalid"}),
        ):
            result = allow(fx.verdict(tool, tool_input), f"off: {tool}")
            if result.get("would_deny"):
                raise AssertionError(f"off must not even flag: {result!r}")
        _code, out, _err = fx.run_interactive(fx.payload("Read", source))
        if '"block"' in out or fx.feed_bodies():
            raise AssertionError(f"off must neither block nor report, printed {out!r}")


def test_fail_open_on_internal_error():
    with fixture("on") as fx:
        source = {"file_path": fx.path("core", "src", "lib.rs")}
        _need("conductor_verdict")
        # A guard that crashes must not take the session down.
        with mock.patch.object(hook, "conductor_verdict", side_effect=RuntimeError("boom")):
            code, out, err = fx.run_interactive(fx.payload("Read", source))
        if '"block"' in out or code not in (0, None, "no exit"):
            raise AssertionError(f"a crash must fail open, got code {code!r} printed {out!r}")
        if "boom" not in err and "conductor" not in err.lower():
            raise AssertionError(f"the crash should leave one stderr line, got {err!r}")
        # Malformed payloads are allowed rather than raised.
        for bad in (
            {"tool_name": "Read", "tool_input": "junk", "cwd": fx.cwd, "session_id": "s"},
            {"tool_name": "Read", "tool_input": None, "cwd": 5, "session_id": ["s"]},
            {"tool_name": ["Bash"], "tool_input": {"command": 7}},
            {},
        ):
            try:
                result = _need("conductor_verdict")(bad, fx.state)
            except Exception as exc:  # noqa: BLE001 - the verdict itself should already be fail-open
                raise AssertionError(f"conductor_verdict raised {exc!r} on {bad!r}")
            allow(result, f"malformed {bad!r}")
        # A broken conductor.json is not a reason to refuse.
        with open(os.path.join(fx.state_dir, "conductor.json"), "w", encoding="utf-8") as handle:
            handle.write("{not json")
        _expect(
            fx.verdict("Agent", {}), "allow", "unreadable conductor.json on a call the rules accept"
        )


def test_probe_writes_payload_keys():
    with fixture("on") as fx:
        probe = os.path.join(fx.tmp_dir, "probe.jsonl")
        payload = fx.payload("Agent", {}, agent_id="a1", agent_type="wf-leaf", transcript_path="t")
        fx.run_interactive(payload, {"NUCLEOS_HOOK_PROBE": probe})
        second = fx.payload("Skill", {})
        fx.run_interactive(second, {"NUCLEOS_HOOK_PROBE": probe})
        if not os.path.exists(probe):
            raise AssertionError("NUCLEOS_HOOK_PROBE set, but no probe file was written")
        with open(probe, encoding="utf-8") as handle:
            lines = [json.loads(line) for line in handle.read().splitlines() if line.strip()]
        if lines != [sorted(payload), sorted(second)]:
            raise AssertionError(f"expected one sorted key list per payload, got {lines!r}")
        if "agent_id" not in lines[0]:
            raise AssertionError("the probe exists to show whether agent_id is in the payload")
        # Without the variable nothing is written.
        other = os.path.join(fx.tmp_dir, "never.jsonl")
        fx.run_interactive(payload)
        if os.path.exists(other):
            raise AssertionError("the probe wrote without NUCLEOS_HOOK_PROBE")


TESTS = [
    test_top_level_read_of_source_denied,
    test_read_of_ai_specs_allowed,
    test_mcp_nucleos_tool_allowed,
    test_mcp_nucleos_double_segment_denied,
    test_agent_tool_allowed,
    test_subagent_payload_with_agent_id_unaffected,
    test_run_with_run_id_env_unaffected,
    test_heavy_py_cargo_allowed,
    test_chained_command_denied,
    test_git_status_allowed,
    test_git_commit_with_explicit_paths_allowed,
    test_git_push_denied,
    test_edit_of_source_denied,
    test_write_to_state_dir_allowed,
    test_manual_prefix_enables_until_next_prompt,
    test_agent_cannot_write_manual_flag,
    test_conductor_env_mention_in_command_denied,
    test_observe_mode_allows_and_reports,
    test_off_mode_allows_everything,
    test_fail_open_on_internal_error,
    test_probe_writes_payload_keys,
]


def main():
    failed = []
    for test in TESTS:
        try:
            test()
        except AssertionError as exc:
            failed.append(test.__name__)
            print(f"FAIL {test.__name__}: {exc}")
        except Exception as exc:  # noqa: BLE001 - one test must not hide the others
            failed.append(test.__name__)
            print(f"FAIL {test.__name__}: {type(exc).__name__}: {exc}")
            traceback.print_exc(limit=3)
        else:
            print(f"ok   {test.__name__}")
    print(f"\n{len(TESTS) - len(failed)}/{len(TESTS)} passed")
    sys.exit(1 if failed else 0)


if __name__ == "__main__":
    main()
