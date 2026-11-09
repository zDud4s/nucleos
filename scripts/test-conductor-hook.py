#!/usr/bin/env python3
"""Cover the conductor policy in `core/hooks/ask_daemon.py` (plan 2026-10-30, chunk B-HOOK).

A top-level session (no `NUCLEOS_RUN_ID`, no `agent_id` in the payload) is a conductor: it launches
agents and trees; it may read up to 15k tokens a call and edit at most 2 files / 40 lines per owner prompt. The policy is `conductor_verdict(payload, state)`,
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
  `manual:` (stripped, case-insensitive) writes the flag file `{"manual": true, "sticky": false,
  "prompt_at": ...}`; `manual:on` writes it with `sticky: true`, which later prompts leave alone until
  a `manual:off` prompt deletes it; any other prompt deletes a non-sticky flag.
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


def test_conductor_tools_are_allowed_when_on():
    with fixture("on") as fx:
        material = fx.path(".ai")
        for tool, tool_input in (
            ("Agent", {}),
            ("Read", {"file_path": fx.path(".ai", "plans", "p.md")}),
            ("Glob", {"pattern": "*.md", "path": material}),
            ("Grep", {"pattern": "B-DEFAULT", "path": material}),
            ("ToolSearch", {}),
            ("TaskOutput", {}),
            ("BashOutput", {}),
            ("SendMessage", {}),
            ("LSP", {}),
            ("mcp__nucleos__spec_backlog", {}),
            ("mcp__nucleos__spec_finish", {}),
            ("mcp__nucleos__tree_launch", {}),
            ("mcp__nucleos__tree_status", {}),
            ("mcp__nucleos__wait_advice", {}),
        ):
            allow(fx.verdict(tool, tool_input), tool)
        # No project marker in the fixture, so these paths lie outside "the project": still refused.
        source = fx.path("core", "src", "lib.rs")
        for tool, tool_input in (
            ("NotebookEdit", {"notebook_path": source}),
            ("Edit", {"file_path": source, "old_string": "a", "new_string": "b"}),
            ("Write", {"file_path": source, "content": "x"}),
        ):
            deny(fx.verdict(tool, tool_input), tool)


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


def _project(fx):
    """Make the fixture's cwd a project (the gate script marks it), as `.nucleos/workflow/project.json` would."""
    os.makedirs(fx.path("scripts"), exist_ok=True)
    with open(fx.path("scripts", "gates.sh"), "w", encoding="utf-8") as handle:
        handle.write("#!/usr/bin/env bash\n")


def _edit(fx, name, lines=1, tool="Edit", session="sess-1"):
    """A verdict for an Edit/Write of `lines` new lines to `src/<name>` in the project."""
    target = fx.path("src", name)
    text = "\n".join("x" for _ in range(lines))
    if tool == "Write":
        return fx.verdict("Write", {"file_path": target, "content": text}, session=session)
    return fx.verdict("Edit", {"file_path": target, "old_string": "a", "new_string": text}, session=session)


def test_small_edit_of_source_allowed():
    with fixture("on") as fx:
        _project(fx)
        allow(_edit(fx, "lib.rs", 3), "small Edit")
        allow(_edit(fx, "other.rs", 5, tool="Write"), "small Write of a second file")
        allow(_edit(fx, "lib.rs", 2), "the same file again is not a new file")
        allow(
            fx.verdict(
                "MultiEdit",
                {"file_path": fx.path("src", "lib.rs"), "edits": [{"new_string": "a\nb"}, {"new_string": "c"}]},
            ),
            "MultiEdit counts its edits",
        )
        deny(fx.verdict("Edit", {"file_path": fx.path("..", "elsewhere.rs"), "new_string": "b"}), "outside the project")
        # What decided the policy is still protected, even inside the project.
        deny(fx.verdict("Write", {"file_path": fx.flag("sess-1"), "content": "x"}), "the flag")
        deny(fx.verdict("Edit", {"file_path": os.path.join(fx.state_dir, "conductor.json"), "new_string": "x"}), "conductor.json")


def test_third_file_refused():
    with fixture("on") as fx:
        _project(fx)
        allow(_edit(fx, "a.rs"), "first file")
        allow(_edit(fx, "b.rs"), "second file")
        result = deny(_edit(fx, "c.rs"), "third file")
        for needle in ("worker", "manual:on"):
            if needle not in result["reason"]:
                raise AssertionError(f"the refusal should mention {needle!r}, got {result['reason']!r}")
        allow(_edit(fx, "a.rs"), "a refused edit was not recorded, and the first file stays editable")
        allow(_edit(fx, "a.rs", session="sess-2"), "another session has its own budget")


def test_more_than_forty_lines_refused():
    with fixture("on") as fx:
        _project(fx)
        deny(_edit(fx, "a.rs", 41), "41 lines at once")
        allow(_edit(fx, "a.rs", 30), "30 lines")
        deny(_edit(fx, "a.rs", 11), "30 + 11 crosses 40")
        allow(_edit(fx, "a.rs", 10), "30 + 10 is exactly 40")
        deny(_edit(fx, "a.rs", 1, tool="Write"), "the 41st line")
        deny(
            fx.verdict("MultiEdit", {"file_path": fx.path("src", "z.rs"), "edits": [{"new_string": "a\nb"}] * 25}),
            "MultiEdit sums its edits",
        )


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


def test_gates_script_allowed_from_the_root_session():
    with fixture("on") as fx:
        absolute = os.path.join(fx.cwd, "scripts", "gates.sh")
        log = os.path.join(fx.tmp_dir, "gate.log")
        git_bash = "C:\\Program Files\\Git\\bin\\bash.exe"
        for tool, command in (
            ("Bash", f'"{git_bash}" scripts/gates.sh hooks'),
            ("Bash", "bash scripts/gates.sh hooks"),
            ("Bash", "scripts/gates.sh hooks"),
            ("Bash", "./scripts/gates.sh core"),
            ("Bash", "scripts/gates.sh"),
            ("Bash", f"bash {absolute} shell"),
            ("Bash", f"{absolute} hooks"),
            ("PowerShell", f"& '{git_bash}' scripts/gates.sh hooks"),
            ("PowerShell", f'& "{git_bash}" scripts/gates.sh hooks'),
            ("Bash", f'bash scripts/gates.sh hooks > "{log}" 2>&1'),
            ("Bash", f"bash scripts/gates.sh hooks > {log}"),
            ("Bash", "bash scripts/gates.sh hooks 2>&1"),
            ("PowerShell", f"& '{git_bash}' scripts/gates.sh hooks *> '{log}'"),
        ):
            allow(fx.verdict(tool, {"command": command}), repr(command))
        # Below the folder that holds scripts/gates.sh, as in a subdirectory of a worktree.
        nested = os.path.join(fx.cwd, "core")
        os.makedirs(nested, exist_ok=True)
        payload = fx.payload("Bash", {"command": f"bash {absolute} hooks"})
        payload["cwd"] = nested
        allow(_need("conductor_verdict")(payload, fx.state), "cwd inside the project")


def test_gates_script_refused_when_chained_or_elsewhere():
    with fixture("on") as fx:
        outside = os.path.join(os.path.dirname(fx.cwd), "elsewhere", "scripts", "gates.sh")
        project_file = fx.path("core", "src", "lib.rs")
        # The fixture sits under the OS temp dir, so the project must show itself (as a real one
        # does, by holding scripts/gates.sh) for a log aimed at its files to be told from scratch.
        os.makedirs(fx.path("scripts"), exist_ok=True)
        with open(fx.path("scripts", "gates.sh"), "w", encoding="utf-8") as handle:
            handle.write("#!/usr/bin/env bash\n")
        for command in (
            "bash scripts/gates.sh hooks; rm x",
            "bash scripts/gates.sh hooks && " + G + " push",
            "bash scripts/gates.sh hooks || true",
            "bash scripts/gates.sh hooks | cat",
            "bash scripts/gates.sh hooks $(id)",
            "bash scripts/gates.sh hooks `id`",
            "bash scripts/gates.sh hooks\nrm x",
            "bash scripts/gates.sh hooks & rm x",
            "bash scripts/gates.sh hooks > log.txt rm x",
            "bash scripts/gates.sh hooks 2>&1 | tee x",
            "bash scripts/gates.sh hooks extra --flag",
            "bash scripts/gates.sh ../../x",
            f"bash scripts/gates.sh hooks > {project_file}",
            "bash scripts/gates.sh hooks > scripts/gates.sh",
            "bash scripts/gates.sh hooks >> " + os.path.join(fx.tmp_dir, "gate.log"),
            f"bash {outside} hooks",
            f"{outside} hooks",
            "bash scripts/other.sh hooks",
            "bash scripts/heavy.py hooks",
            "sh -c scripts/gates.sh",
            "bash -c 'scripts/gates.sh hooks'",
            "bash /tmp/evil/bash scripts/gates.sh hooks",
            "/tmp/evil/bash scripts/gates.sh hooks",
            "& " + G + " status",
        ):
            deny(fx.verdict("Bash", {"command": command}), repr(command))


def test_chained_command_denied():
    with fixture("on") as fx:
        _gate_marker(fx)
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


def _gate_marker(fx):
    """Gives the fixture the gate script a real project holds, so a redirect into it is told from scratch."""
    os.makedirs(fx.path("scripts"), exist_ok=True)
    with open(fx.path("scripts", "gates.sh"), "w", encoding="utf-8") as handle:
        handle.write("#!/usr/bin/env bash\n")


def _project(fx):
    """Marks the fixture's cwd as a project, as the workflow pointer does; returns its worktrees dir."""
    pointer = fx.path(".nucleos", "workflow")
    os.makedirs(pointer, exist_ok=True)
    with open(os.path.join(pointer, "project.json"), "w", encoding="utf-8") as handle:
        json.dump({"state_dir": fx.state_dir}, handle)
    worktrees = fx.path(".claude", "worktrees")
    os.makedirs(os.path.join(worktrees, "feat"), exist_ok=True)
    return worktrees


def test_read_only_git_additions_allowed():
    with fixture("on") as fx:
        for tool in ("Bash", "PowerShell"):
            for command in (
                f"{G} merge-base --is-ancestor fix/a master",
                f"{G} -C . merge-base --is-ancestor HEAD master",
                f"{G} worktree list",
                f"{G} branch --list",
                f"{G} branch --list 'fix/*'",
            ):
                allow(fx.verdict(tool, {"command": command}), f"{tool}: {command!r}")
        for command in (
            f"{G} merge-base master fix/a",
            f"{G} merge-base --octopus a b",
            f"{G} merge-base --is-ancestor a",
            f"{G} merge-base --is-ancestor a b c",
            f"{G} merge-base --is-ancestor a b; rm x",
            f"{G} worktree add x",
            f"{G} worktree prune",
            f"{G} branch -D x",
            f"{G} branch --list -d x",
        ):
            deny(fx.verdict("Bash", {"command": command}), repr(command))


def test_worktree_remove_allowed_only_under_the_project_worktrees():
    with fixture("on") as fx:
        worktrees = _project(fx)
        inside = (worktrees + "/feat").replace("\\", "/")
        for tool in ("Bash", "PowerShell"):
            for command in (
                f"{G} worktree remove {inside}",
                f"{G} worktree remove .claude/worktrees/feat",
                f'{G} worktree remove "{inside}"',
            ):
                allow(fx.verdict(tool, {"command": command}), f"{tool}: {command!r}")
        outside = os.path.join(os.path.dirname(fx.cwd), "elsewhere").replace("\\", "/")
        for command in (
            f"{G} worktree remove --force {inside}",
            f"{G} worktree remove -f {inside}",
            f"{G} worktree remove {inside} --force",
            f"{G} worktree remove x",
            f"{G} worktree remove {outside}",
            f"{G} worktree remove {fx.cwd}".replace("\\", "/"),
            f"{G} worktree remove {worktrees}".replace("\\", "/"),
            f"{G} worktree remove .claude/worktrees/../..",
            f"{G} worktree remove .claude/worktrees/feat/../../..",
            f"{G} worktree remove",
            f"{G} worktree remove {inside} {inside}",
            f"{G} worktree remove {inside}; rm x",
            f"{G} worktree remove {inside} | cat",
        ):
            deny(fx.verdict("Bash", {"command": command}), repr(command))


def test_worktree_remove_refused_when_it_holds_a_node_modules_junction():
    with fixture("on") as fx:
        worktrees = _project(fx)
        inside = (worktrees + "/feat").replace("\\", "/")
        os.makedirs(os.path.join(worktrees, "feat", "shell", "node_modules"))
        deny(fx.verdict("Bash", {"command": f"{G} worktree remove {inside}"}), "node_modules present")


def _land_fixture(fx):
    worktrees = _project(fx)
    dev_home = os.path.join(os.path.dirname(fx.cwd), "dev").replace("\\", "/")
    os.environ["NUCLEOS_DEV_HOME"] = dev_home
    return (worktrees + "/feat").replace("\\", "/"), dev_home + "/run/nucleos-core.exe", dev_home


def test_land_allowed_from_the_root_session():
    with fixture("on") as fx:
        inside, exe, _dev = _land_fixture(fx)
        exe_bs = exe.replace("/", "\\")
        for tool, command in (
            ("Bash", f"{exe} --land"),
            ("Bash", f"{exe} --land master"),
            ("Bash", f'"{exe}" --land master'),
            ("Bash", f"cd {inside} && {exe} --land"),
            ("Bash", f'cd "{inside}" && "{exe}" --land master'),
            ("Bash", f"cd .claude/worktrees/feat && {exe} --land fix/a"),
            ("PowerShell", f"& '{exe}' --land"),
            ("PowerShell", f'& "{exe_bs}" --land master'),
            ("PowerShell", f"{exe_bs} --land master"),
            ("PowerShell", f'cd "{inside}" && & "{exe_bs}" --land master'),
        ):
            allow(fx.verdict(tool, {"command": command}), f"{tool}: {command!r}")
    with fixture("on") as fx:
        _project(fx)
        os.environ.pop("NUCLEOS_DEV_HOME", None)
        allow(fx.verdict("Bash", {"command": "C:/nucleos-dev/run/nucleos-core.exe --land"}), "default dev home")


def test_land_refused_when_widened_or_elsewhere():
    with fixture("on") as fx:
        inside, exe, dev = _land_fixture(fx)
        _gate_marker(fx)
        outside = os.path.join(os.path.dirname(fx.cwd), "elsewhere").replace("\\", "/")
        for command in (
            f"{exe} --print-token",
            f"{exe} --workflow-sync {inside}",
            f"{exe} --settings-reset a/b",
            f"{exe}",
            f"{exe} --land master extra",
            f"{exe} --land --force",
            f"{exe} --land -x",
            f"{exe} --land master; rm x",
            f"{exe} --land; rm x",
            f"{exe} --land || true",
            f"{exe} --land | cat",
            f"{exe} --land $(id)",
            f"{exe} --land `id`",
            f"{exe} --land\nrm x",
            f"{exe} --land > out.txt",
            f"{dev}/other/nucleos-core.exe --land",
            f"{dev}/run/evil.exe --land",
            f"/tmp/nucleos-core.exe --land",
            f"cd {outside} && {exe} --land",
            f"cd .. && {exe} --land",
            f"cd {inside} && {exe} --print-token",
            f"cd {inside} || {exe} --land",
            f"rm x && {exe} --land",
        ):
            deny(fx.verdict("Bash", {"command": command}), repr(command))


def test_composition_of_accepted_parts_and_closing_tails():
    with fixture("on") as fx:
        inside, exe, dev = _land_fixture(fx)
        _gate_marker(fx)
        log = f"{dev}/scratch/g.log"
        for command in (
            f"cd {inside} && scripts/gates.sh core",
            f"cd {inside} && {exe} --land",
            f"cd {fx.cwd} && {exe} --land".replace("\\", "/"),
            f"cd x && {G} status",
            f"cd {inside} && cd {inside} && {exe} --land master",
            f"cd {inside} ; {exe} --land",
            f"{G} status && {exe} --land",
            f"{exe} --land && cd {inside}",
            f"scripts/gates.sh core && {G} status",
            f"{G} status; {G} log --oneline -3",
            f"cd .claude/worktrees/feat; python scripts/heavy.py cargo test",
            f"cd {inside} && {G} status",
            f"{G} status 2>&1",
            f"{G} log | tail -n 5",
            f"{G} log | tail -5",
            f"{G} log | head -n 3",
            f"{G} log|head -20",
            f"{G} log 2>&1 | tail -n 5",
            f"scripts/gates.sh core > {log} 2>&1 | tail -20",
            f"scripts/gates.sh core 2>&1 | tail -20",
            f"{G} log > {log}",
            f"python scripts/heavy.py cargo test > {log} 2>&1",
            f"{exe} --land 2>&1",
            f"{G} status > {os.path.join(fx.tmp_dir, 'x')}",
            f"cd {inside} && scripts/gates.sh core > {log} 2>&1 | tail -n 40",
            f"CARGO_BUILD_JOBS=2 python scripts/heavy.py cargo test",
            f"RUSTC_BOOTSTRAP=1 RUST_BACKTRACE=1 scripts/gates.sh core",
            f"NUCLEOS_GATE_KEEP_GOING=1 bash scripts/gates.sh core",
            f"NUCLEOS_HEAVY_STALL_SECS=0 python scripts/heavy.py --prio 1 cargo check",
            f"CARGO_TERM_COLOR=never python scripts/heavy.py cargo test",
            f"CARGO_TARGET_DIR=x CARGO_INCREMENTAL=0 python scripts/heavy.py cargo test",
            f"RUST_BACKTRACE=1 python scripts/heavy.py cargo test",
            f"RUSTC_BOOTSTRAP=1 python scripts/heavy.py cargo test",
        ):
            allow(fx.verdict("Bash", {"command": command}), repr(command))
        allow(fx.verdict("PowerShell", {"command": f"cd {inside} ; & '{exe}' --land *> '{log}'"}), "PowerShell")
        outside = os.path.join(os.path.dirname(fx.cwd), "elsewhere").replace("\\", "/")
        flag = fx.flag("sess-1").replace("\\", "/")
        for command in (
            f"{G} status | cat",
            f"{G} log | tail -n 5 | cat",
            f"{G} log | tail -f x",
            f"{G} log | tail -n 5 && {G} status",
            f"{G} log | head -n 5 > out.txt",
            f"{G} status || true",
            f"{G} status && {G} status || true",
            f"{G} status $(id)",
            f"{G} status `id`",
            f"{G} status\n{G} log",
            f"{G} log >> {log}",
            f"{G} status > out.txt",
            f"{G} status > {flag}",
            f"scripts/gates.sh core > {flag}",
            f"{G} status > {G}",
            f"{G} status && {G} push",
            f"{G} status && rm x",
            f"{G} status; rm x",
            f"cd {outside} && {G} status",
            f"cd {outside}/.. && {G} status",
            f"cd .. && {G} status",
            f"cd /tmp && {G} status",
            f"cd && {G} status",
            f"cd - && {G} status",
            f"cd {inside} extra && {G} status",
            f"{G} status &",
            f"{G} status & {G} log",
            f"{G} status &&& {G} log",
            f"{G} status;",
            f"; {G} status",
            f"{G} status &&",
            f"PATH=/tmp/x python scripts/heavy.py cargo test",
            f"RUSTFLAGS=-Cx scripts/gates.sh core",
            f"CARGO_BUILD_RUSTC=x python scripts/heavy.py cargo test",
            f"CARGO_BUILD_RUSTC_WRAPPER=x python scripts/heavy.py cargo test",
            f"CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_RUNNER=x python scripts/heavy.py cargo test",
            f"CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_LINKER=x python scripts/heavy.py cargo test",
            f"RUSTC_WRAPPER=x python scripts/heavy.py cargo test",
            f"CARGO_HOME=x python scripts/heavy.py cargo test",
            f"RUST_FOO=x python scripts/heavy.py cargo test",
            f"NUCLEOS_HEAVY_PYTHON=x scripts/gates.sh core",
            f"NUCLEOS_HEAVY_GDB=x python scripts/heavy.py cargo test",
            f"NUCLEOS_HEAVY_MAIN=x scripts/gates.sh core",
            f"CARGO_BUILD_JOBS=2 {G} status",
            f"CARGO_BUILD_JOBS=2 {exe} --land",
            f"CARGO_BUILD_JOBS=2 cd {inside}",
            f"CARGO_BUILD_JOBS=2",
            f"CARGO_BUILD_JOBS=2; {G} status",
            f"cat <<EOF\nx\nEOF",
            f"scripts/gates.sh core < input.txt",
        ):
            deny(fx.verdict("Bash", {"command": command}), repr(command))
        deny(fx.verdict("PowerShell", {"command": f"cd {inside} ; & {G} status"}), "call operator on git")


def test_broker_runs_cargo_go_node_and_status():
    with fixture("on") as fx:
        for command in (
            "python scripts/heavy.py go test ./...",
            "python scripts/heavy.py go build",
            "python scripts/heavy.py npx tsc --noEmit",
            "python scripts/heavy.py npm run build",
            "python scripts/heavy.py --kind node npm test",
            "python scripts/heavy.py --kind=go go vet ./...",
            "python scripts/heavy.py --wait-max 600 --prio 1 --agent me cargo build",
            "python scripts/heavy.py --wait-max=600 cargo build",
            "python scripts/heavy.py -- cargo test",
            "python scripts/heavy.py --prio 1 -- npm test",
            "python scripts/heavy.py cargo test -- --nocapture",
            "python scripts/heavy.py status",
            "python scripts/heavy.py status > " + os.path.join(fx.tmp_dir, "s.log"),
        ):
            allow(fx.verdict("Bash", {"command": command}), repr(command))
        for command in (
            "python scripts/heavy.py status extra",
            "python scripts/heavy.py report",
            "python scripts/heavy.py gc",
            "python scripts/heavy.py warm",
            "python scripts/heavy.py hold-worktree",
            "python scripts/heavy.py",
            "python scripts/heavy.py --kind node",
            "python scripts/heavy.py --prio",
            "python scripts/heavy.py --bogus cargo test",
            "python scripts/heavy.py -x cargo test",
            "python scripts/heavy.py -- bash -c x",
            "python scripts/heavy.py rm -rf x",
            "python scripts/heavy.py python evil.py",
            "python scripts/heavy.py --kind node node evil.js",
            "python scripts/heavy.py -- -- cargo test",
            "python scripts/heavy.py status; rm x",
            "python scripts/heavy.py npm test | cat",
            "go test ./...",
            "npm test",
            "npx tsc",
        ):
            deny(fx.verdict("Bash", {"command": command}), repr(command))


def test_daemon_script_and_verify_ticket_accepted():
    with fixture("on") as fx:
        inside, exe, dev = _land_fixture(fx)
        _gate_marker(fx)
        script = fx.path("scripts", "run-daemon.ps1").replace("\\", "/")
        script_bs = script.replace("/", "\\")
        for tool, command in (
            ("PowerShell", f"& '{script_bs}'"),
            ("PowerShell", f'& "{script}"'),
            ("PowerShell", f"{script_bs}"),
            ("PowerShell", "scripts/run-daemon.ps1"),
            ("PowerShell", ".\\scripts\\run-daemon.ps1"),
            ("PowerShell", f"pwsh -File {script_bs}"),
            ("PowerShell", f"powershell -File '{script_bs}'"),
            ("PowerShell", f"powershell.exe -File {script}"),
            ("Bash", f"pwsh -File {script}"),
            ("Bash", f"cd {inside} && pwsh -File {script}"),
            ("Bash", f"{exe} --verify-ticket 123"),
            ("Bash", f"{exe} --verify-ticket 123 2>&1 | tail -5"),
            ("PowerShell", f"& '{exe.replace('/', chr(92))}' --verify-ticket 7"),
        ):
            allow(fx.verdict(tool, {"command": command}), f"{tool}: {command!r}")
        for command in (
            f"{script} -Foo",
            f"{script} extra",
            f"pwsh -File {script} extra",
            f"pwsh -File {script} -Foo",
            f"pwsh -Command {script}",
            f"pwsh -NoProfile -File {script}",
            f"pwsh {script}",
            f"pwsh -File {os.path.dirname(fx.cwd).replace(chr(92), '/')}/elsewhere/scripts/run-daemon.ps1",
            "pwsh -File /tmp/evil/scripts/run-daemon.ps1",
            "bash -File scripts/run-daemon.ps1",
            "scripts/run-daemon.ps1; rm x",
            "scripts/run-daemon.ps1 | cat",
            "scripts/run-daemon.ps1 && rm x",
            "scripts/run-daemon.ps1 > out.txt",
            f"{exe} --verify-ticket abc",
            f"{exe} --verify-ticket -1",
            f"{exe} --verify-ticket 1.5",
            f"{exe} --verify-ticket 12 13",
            f"{exe} --verify-ticket",
            f"{exe} --verify-ticket 1; rm x",
            f"{dev}/other/nucleos-core.exe --verify-ticket 1",
            f"/tmp/nucleos-core.exe --verify-ticket 1",
            f"{exe} --print-token",
            f"{exe} --settings-reset a/b",
            f"{exe} --settings-rollback a/b 1",
            f"{exe} --workflow-sync {inside}",
            f"{exe} --verify-ticket 1 --land",
        ):
            deny(fx.verdict("PowerShell", {"command": command}), repr(command))


def test_small_read_only_commands_and_list_agents():
    with fixture("on") as fx:
        _gate_marker(fx)
        allow(fx.verdict("ListAgents", {}), "ListAgents")
        for tool, command in (
            ("Bash", "date"),
            ("Bash", "date +%H:%M"),
            ("Bash", "date '+%Y-%m-%d %H:%M'"),
            ("PowerShell", "Get-Date"),
            ("PowerShell", "get-date -Format 'HH:mm'"),
            ("Bash", "sleep 5"),
            ("Bash", "sleep 60"),
            ("PowerShell", "Start-Sleep 30"),
            ("PowerShell", "Start-Sleep -Seconds 30"),
            ("PowerShell", "start-sleep 1"),
            ("Bash", f"sleep 20 && {G} status"),
            ("Bash", f"date; {G} log --oneline -3"),
        ):
            allow(fx.verdict(tool, {"command": command}), f"{tool}: {command!r}")
        for command in (
            "sleep 61",
            "sleep 600",
            "sleep",
            "sleep 5m",
            "sleep 1.5",
            "sleep -1",
            "sleep 5 5",
            "sleep infinity",
            "Start-Sleep 61",
            "Start-Sleep -Milliseconds 5",
            "Start-Sleep -Seconds 61",
            "Start-Sleep",
            "date -s 2020-01-01",
            "date --set=x",
            "date +%H > out.txt",
            "date 010112002020",
            "date +%H extra",
            "Get-Date -Foo",
            "Get-Date -Format x y",
            "Set-Date x",
            "sleep 5 &",
            "sleep 5 | cat",
        ):
            deny(fx.verdict("PowerShell", {"command": command}), repr(command))


def test_refusal_says_what_the_root_may_do_itself():
    with fixture("on") as fx:
        reason = deny(fx.verdict("Bash", {"command": f"{G} push"}), "push")["reason"]
        if not reason.startswith("This session is a conductor: launch an agent"):
            raise AssertionError(f"the first sentence must stay, got {reason!r}")
        for needle in (
            "scripts/gates.sh",
            "scripts/heavy.py cargo|go|npx|npm",
            "read-only git",
            "--land",
            "--verify-ticket",
            "run-daemon.ps1",
            "Read/Grep/Glob",
            "15k tokens",
            "Edit/Write",
            "`manual:on`",
            "neither the root nor an agent may do",
            "`manual:`",
        ):
            if needle not in reason:
                raise AssertionError(f"the refusal should mention {needle!r}, got {reason!r}")
        if reason.count("\n") > 1 or len(reason) > 700:
            raise AssertionError(f"the refusal should stay compact, got {len(reason)} characters")


def _git_bash(path):
    """`C:\\x\\y` as Git Bash spells it, `/c/x/y`."""
    path = path.replace("\\", "/")
    return f"/{path[0].lower()}{path[2:]}"


def test_git_bash_drive_paths_are_understood_on_windows():
    if os.name != "nt":
        return
    with fixture("on") as fx:
        inside, exe, dev = _land_fixture(fx)
        os.makedirs(fx.path("scripts"), exist_ok=True)
        with open(fx.path("scripts", "gates.sh"), "w", encoding="utf-8") as handle:
            handle.write("#!/usr/bin/env bash\n")
        gb_exe, gb_inside, gb_gates = _git_bash(exe), _git_bash(inside), _git_bash(fx.path("scripts", "gates.sh"))
        for command in (
            f"scripts/gates.sh core > {_git_bash(dev)}/scratch/g.log 2>&1",
            f"{gb_exe} --land",
            f"cd {gb_inside} && {gb_exe} --land",
            f"cd {gb_inside} && {exe} --land",
            f"{G} worktree remove {gb_inside}",
            f"{gb_gates} core",
            f"bash {gb_gates} core",
        ):
            allow(fx.verdict("Bash", {"command": command}), repr(command))
        for command in (
            f"scripts/gates.sh core > {_git_bash(fx.path('core', 'src', 'lib.rs'))}",
            f"scripts/gates.sh core > /tmp/g.log",
            f"{_git_bash(dev)}/other/nucleos-core.exe --land",
            f"{G} worktree remove {_git_bash(fx.cwd)}",
            f"{G} worktree remove {_git_bash(os.path.dirname(fx.cwd))}/elsewhere",
            f"/z/nowhere/scripts/gates.sh core",
        ):
            deny(fx.verdict("Bash", {"command": command}), repr(command))


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


def test_manual_prefix_lets_the_owner_work_by_hand():
    with fixture("on") as fx:
        target = fx.path("core", "src", "lib.rs")
        deny(fx.verdict("Bash", {"command": f"{G} push"}), "control")
        _prompt(fx, "manual: fix the typo in lib.rs")
        allow(
            fx.verdict("Edit", {"file_path": target, "old_string": "a", "new_string": "b"}),
            "manual prompt permits owner Edit",
        )


def test_escape_is_only_at_the_start_of_the_prompt():
    with fixture("on") as fx:
        target = fx.path("core", "src", "lib.rs")
        _prompt(fx, "Please use manual: to fix the typo in lib.rs")
        deny(
            fx.verdict("Edit", {"file_path": target, "old_string": "a", "new_string": "b"}),
            "manual: in the middle is not an escape",
        )


def test_manual_prefix_enables_until_next_prompt():
    with fixture("on") as fx:
        source = {"file_path": fx.path("core", "src", "lib.rs")}
        deny(fx.verdict("Read", source), "control before the prefix")
        _prompt(fx, "  Manual: fix the typo in lib.rs")
        if not os.path.exists(fx.flag("sess-1")):
            raise AssertionError(f"no flag file at {fx.flag('sess-1')}")
        with open(fx.flag("sess-1"), encoding="utf-8") as handle:
            body = json.load(handle)
        if body.get("manual") is not True or body.get("sticky") is not False or "prompt_at" not in body:
            raise AssertionError(f"flag body should be {{manual: true, sticky: false, prompt_at}}, got {body!r}")
        allow(fx.verdict("Read", source), "Read while the flag stands")
        deny(fx.verdict("Read", source, session="sess-2"), "another session is not released")
        _prompt(fx, "now do the next thing")
        if os.path.exists(fx.flag("sess-1")):
            raise AssertionError("the next prompt without the prefix must delete the flag")
        deny(fx.verdict("Read", source), "Read after the next prompt")


def test_manual_on_survives_ordinary_prompts_until_manual_off():
    with fixture("on") as fx:
        source = {"file_path": fx.path("core", "src", "lib.rs")}
        deny(fx.verdict("Read", {"file_path": _sized(fx, "big.rs", 80000)}), "control before manual:on")
        _prompt(fx, "  Manual:ON keep going by hand")
        with open(fx.flag("sess-1"), encoding="utf-8") as handle:
            if json.load(handle).get("sticky") is not True:
                raise AssertionError("manual:on must write a sticky flag")
        allow(fx.verdict("Read", source), "Read under manual:on")
        _prompt(fx, "a perfectly ordinary prompt")
        allow(fx.verdict("Read", source), "manual:on survives an ordinary prompt")
        _prompt(fx, "<task-notification>\n<task-id>x</task-id>")
        allow(fx.verdict("Read", source), "and a notification")
        _prompt(fx, "manual: one prompt only")  # a plain manual: after manual:on rewrites the flag as not sticky
        _prompt(fx, "ordinary again")
        deny(fx.verdict("Read", source), "a plain manual: replaced the sticky flag, so the next prompt cleared it")
        _prompt(fx, "manual:on again")
        _prompt(fx, "manual:off")
        if os.path.exists(fx.flag("sess-1")):
            raise AssertionError("manual:off must delete the flag")
        deny(fx.verdict("Read", source), "Read after manual:off")
        _prompt(fx, "manual:off", session="sess-2")  # nothing to clear is not an error
        # Only a UserPromptSubmit sets it, as with plain manual:.
        _prompt(fx, "manual:on please", event="PreToolUse")
        if os.path.exists(fx.flag("sess-1")):
            raise AssertionError("a non-UserPromptSubmit event created the sticky flag")


def test_plain_manual_is_still_one_prompt():
    with fixture("on") as fx:
        source = {"file_path": fx.path("core", "src", "lib.rs")}
        _prompt(fx, "manual: just this")
        allow(fx.verdict("Read", source), "manual: opens the prompt")
        _prompt(fx, "the next one")
        deny(fx.verdict("Read", source), "and the next prompt closes it")


def test_agent_cannot_write_manual_flag():
    with fixture("on") as fx:
        _project(fx)  # the flag is refused even where small edits are allowed
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


def test_default_mode_is_on_when_probe_confirmed():
    with fixture("on") as fx:
        fx.set_mode(None)
        if _need("PROBE_CONFIRMED") is not True:
            raise AssertionError("the measured probe must enable conductor mode by default")
        mode = _need("conductor_mode")(fx.state)
        if mode != "on":
            raise AssertionError(f"confirmed probe should default to on, got {mode!r}")
        deny(
            fx.verdict(
                "Edit", {"file_path": fx.path("core", "src", "lib.rs"), "old_string": "a", "new_string": "b"}
            ),
            "top-level Edit in the confirmed default mode",
        )


def test_default_mode_observe_when_probe_unconfirmed():
    with fixture("on") as fx:
        fx.set_mode(None)
        _need("PROBE_CONFIRMED")
        with mock.patch.object(hook, "PROBE_CONFIRMED", False):
            mode = _need("conductor_mode")(fx.state)
            if mode != "observe":
                raise AssertionError(f"unconfirmed probe should default to observe, got {mode!r}")
            allow(
                fx.verdict(
                    "Edit", {"file_path": fx.path("core", "src", "lib.rs"), "old_string": "a", "new_string": "b"}
                ),
                "top-level Edit in the unconfirmed default mode",
            )


def test_subagent_is_unaffected_when_on():
    with fixture("on") as fx:
        allow(
            fx.verdict(
                "Edit",
                {"file_path": fx.path("core", "src", "lib.rs"), "old_string": "a", "new_string": "b"},
                agent_id="a1b2",
                agent_type="wf-leaf",
            ),
            "subagent Edit in mode on",
        )


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


# --- small bounded reads by the root ------------------------------------------------------------


def _sized(fx, name, size):
    """A file of `size` bytes under the project, outside `.ai`; its path."""
    path = fx.path("src", name)
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "wb") as handle:
        handle.write(b"x" * size)
    return path


def test_small_read_allowed_large_read_denied():
    with fixture("on") as fx:
        allow(fx.verdict("Read", {"file_path": _sized(fx, "small.rs", 400)}), "small Read")
        big = _sized(fx, "big.rs", 80000)  # 20000 tokens
        result = deny(fx.verdict("Read", {"file_path": big}), "large Read")
        for needle in ("15000", "Explore", "manual:"):
            if needle not in result["reason"]:
                raise AssertionError(f"the refusal should mention {needle!r}, got {result['reason']!r}")
        allow(fx.verdict("Read", {"file_path": big, "limit": 100}), "large Read with a small limit")
        deny(fx.verdict("Read", {"file_path": big, "limit": 1500}), "limit that still exceeds the call budget")
        deny(fx.verdict("Read", {"file_path": fx.path("src", "missing.rs")}), "unreadable path")
        image = _sized(fx, "shot.png", 100)
        deny(fx.verdict("Read", {"file_path": image}), "image")
        pdf = _sized(fx, "doc.pdf", 100)
        deny(fx.verdict("Read", {"file_path": pdf, "pages": "1-2"}), "paged pdf")
        deny(fx.verdict("Read", {"file_path": _sized(fx, "n.ipynb", 100)}), "notebook")


def test_grep_modes_and_head_limit():
    with fixture("on") as fx:
        _sized(fx, "a.rs", 10)
        where = fx.path("src")
        allow(fx.verdict("Grep", {"pattern": "x", "path": where}), "Grep default mode")
        allow(
            fx.verdict("Grep", {"pattern": "x", "path": where, "output_mode": "files_with_matches"}),
            "Grep files_with_matches",
        )
        allow(fx.verdict("Grep", {"pattern": "x", "path": where, "output_mode": "count"}), "Grep count")
        content = {"pattern": "x", "path": where, "output_mode": "content"}
        deny(fx.verdict("Grep", content), "content without head_limit")
        allow(fx.verdict("Grep", {**content, "head_limit": 20}), "content with head_limit")
        deny(fx.verdict("Grep", {**content, "head_limit": 0}), "head_limit 0")
        deny(fx.verdict("Grep", {**content, "head_limit": 61}), "head_limit over 60")
        deny(fx.verdict("Grep", {"pattern": "x", "path": where, "head_limit": 0}), "head_limit 0, default mode")
        deny(fx.verdict("Grep", {"pattern": "x", "path": fx.path("nope")}), "Grep of a missing path")


def test_grep_context_flags_are_charged():
    with fixture("on") as fx:
        _sized(fx, "a.rs", 10)
        base = {"pattern": "x", "path": fx.path("src"), "output_mode": "content", "head_limit": 20}
        allow(fx.verdict("Grep", {**base, "-C": 2}), "content with -C 2")
        allow(fx.verdict("Grep", {**base, "head_limit": 60, "-A": 5}), "head_limit 60 with -A 5 is 4320")
        deny(fx.verdict("Grep", {**base, "head_limit": 60, "-A": 20}), "head_limit 60 with -A 20 is over 15000")


def test_glob_allowed():
    with fixture("on") as fx:
        _sized(fx, "a.rs", 10)
        allow(fx.verdict("Glob", {"pattern": "*.rs", "path": fx.path("src")}), "Glob")


def test_grep_and_glob_on_a_file_cost_like_a_read():
    with fixture("on") as fx:
        small = _sized(fx, "small.rs", 400)
        big = _sized(fx, "big.rs", 80000)
        allow(fx.verdict("Grep", {"pattern": "x", "path": small}), "Grep list mode on a small file")
        allow(fx.verdict("Grep", {"pattern": "x", "path": small, "output_mode": "content"}), "content, small file")
        allow(fx.verdict("Glob", {"pattern": "*", "path": small}), "Glob with a file path")
        content = {"pattern": "x", "path": big, "output_mode": "content"}
        deny(fx.verdict("Grep", content), "content on a big file without head_limit")
        allow(fx.verdict("Grep", {**content, "head_limit": 20}), "content on a big file, head_limit 20")
        deny(fx.verdict("Grep", {**content, "head_limit": 0}), "head_limit 0 on a big file")
        deny(fx.verdict("Grep", {**content, "head_limit": 61}), "head_limit over 60 on a big file")
        allow(fx.verdict("Grep", {"pattern": "x", "path": big}), "list mode on a big file stays a list")
        deny(fx.verdict("Grep", {"pattern": "x", "path": fx.path("src", "missing.rs")}), "missing file")


def test_new_prompt_resets_the_edit_budget():
    with fixture("on") as fx:
        _project(fx)
        allow(_edit(fx, "a.rs", 30), "30 lines")
        deny(_edit(fx, "a.rs", 11), "over 40")
        _prompt(fx, "go on")
        allow(_edit(fx, "a.rs", 40), "after the reset")
        deny(_edit(fx, "a.rs", 1), "budget used again")
        _prompt(fx, "manual: do it by hand")
        _prompt(fx, "and now back to the root")
        allow(_edit(fx, "b.rs", 40), "a manual prompt and the next one reset it too")


def test_manual_bypasses_the_edit_budget():
    with fixture("on") as fx:
        _project(fx)
        deny(_edit(fx, "a.rs", 41), "over the budget")
        _prompt(fx, "manual: do it")
        allow(_edit(fx, "a.rs", 41), "manual Edit, however large")
        allow(_edit(fx, "c.rs", 1), "manual, third file")


def test_allow_listed_paths_are_not_budgeted():
    with fixture("on") as fx:
        _project(fx)
        os.makedirs(fx.path(".ai", "specs"), exist_ok=True)
        spec = fx.path(".ai", "specs", "x.md")
        with open(spec, "wb") as handle:
            handle.write(b"x" * 400000)
        for _ in range(3):
            allow(fx.verdict("Read", {"file_path": spec}), "allow-listed Read, however large")
        for index in range(4):  # material writes use no file or line budget
            allow(
                fx.verdict("Write", {"file_path": fx.path(".ai", f"n{index}.md"), "content": "x\n" * 100}),
                "allow-listed Write",
            )


def test_state_failure_fails_open():
    with fixture("on") as fx:
        _project(fx)
        folder = os.path.join(fx.tmp_dir, "nucleos-conductor")
        with open(folder, "w", encoding="utf-8") as handle:  # a file where the directory should be
            handle.write("x")
        allow(_edit(fx, "a.rs"), "the budget cannot be written")
        allow(fx.verdict("Read", {"file_path": _sized(fx, "small.rs", 400)}), "reads need no state")
    with fixture("on") as fx:
        _project(fx)
        folder = os.path.join(fx.tmp_dir, "nucleos-conductor")
        os.makedirs(folder)
        with open(os.path.join(folder, "sess-1.edits.json"), "w", encoding="utf-8") as handle:
            handle.write("{not json")
        allow(_edit(fx, "a.rs"), "unreadable budget file")
        allow(_edit(fx, "a.rs", session="bad/id"), "unsafe session id")


def test_observe_mode_still_only_observes_a_big_read():
    with fixture("observe") as fx:
        result = fx.verdict("Read", {"file_path": _sized(fx, "big.rs", 80000)})
        allow(result, "observe")
        if not result.get("would_deny"):
            raise AssertionError("observe should still flag the big read")


# --- notifications -----------------------------------------------------------------------------

NOTIFICATIONS = (
    "<task-notification>\n<task-id>a176f27f5d0c5b7ca</task-id>\n<status>completed</status>\n</task-notification>",
    "  \n<task-notification>\n<task-id>x</task-id>\n</task-notification>",
    "<system-reminder>\nnote\n</system-reminder>\n<task-notification>\n<task-id>x</task-id>",
    'Another Claude session sent a message:\n<agent-message from="x">hi</agent-message>',
)


def test_notifications_neither_set_nor_clear_manual():
    with fixture("on") as fx:
        source = {"file_path": fx.path("core", "src", "lib.rs")}
        _prompt(fx, "manual: fix the typo")
        for text in NOTIFICATIONS:
            _prompt(fx, text)
            allow(fx.verdict("Read", source), f"manual survives {text[:30]!r}")
        _prompt(fx, "now something else")
        deny(fx.verdict("Read", source), "a real prompt still clears it")
    with fixture("on") as fx:
        source = {"file_path": fx.path("core", "src", "lib.rs")}
        for text in NOTIFICATIONS:
            _prompt(fx, text + "\nmanual: do it")
            deny(fx.verdict("Read", source), f"a notification never sets manual {text[:30]!r}")


def test_notifications_do_not_reset_the_edit_budget():
    with fixture("on") as fx:
        _project(fx)
        allow(_edit(fx, "a.rs", 30), "30 lines")
        for text in NOTIFICATIONS:
            _prompt(fx, text)
        deny(_edit(fx, "a.rs", 11), "notifications keep the budget, so 30 + 11 still crosses 40")
        _prompt(fx, "go on")
        allow(_edit(fx, "a.rs", 11), "a real prompt resets it")


TESTS = [
    test_small_read_allowed_large_read_denied,
    test_grep_modes_and_head_limit,
    test_grep_context_flags_are_charged,
    test_glob_allowed,
    test_new_prompt_resets_the_edit_budget,
    test_manual_bypasses_the_edit_budget,
    test_allow_listed_paths_are_not_budgeted,
    test_state_failure_fails_open,
    test_observe_mode_still_only_observes_a_big_read,
    test_top_level_read_of_source_denied,
    test_read_of_ai_specs_allowed,
    test_mcp_nucleos_tool_allowed,
    test_mcp_nucleos_double_segment_denied,
    test_agent_tool_allowed,
    test_conductor_tools_are_allowed_when_on,
    test_subagent_payload_with_agent_id_unaffected,
    test_run_with_run_id_env_unaffected,
    test_heavy_py_cargo_allowed,
    test_gates_script_allowed_from_the_root_session,
    test_gates_script_refused_when_chained_or_elsewhere,
    test_chained_command_denied,
    test_git_status_allowed,
    test_read_only_git_additions_allowed,
    test_worktree_remove_allowed_only_under_the_project_worktrees,
    test_worktree_remove_refused_when_it_holds_a_node_modules_junction,
    test_land_allowed_from_the_root_session,
    test_land_refused_when_widened_or_elsewhere,
    test_git_commit_with_explicit_paths_allowed,
    test_git_push_denied,
    test_small_edit_of_source_allowed,
    test_third_file_refused,
    test_more_than_forty_lines_refused,
    test_write_to_state_dir_allowed,
    test_manual_prefix_lets_the_owner_work_by_hand,
    test_escape_is_only_at_the_start_of_the_prompt,
    test_manual_prefix_enables_until_next_prompt,
    test_manual_on_survives_ordinary_prompts_until_manual_off,
    test_plain_manual_is_still_one_prompt,
    test_agent_cannot_write_manual_flag,
    test_conductor_env_mention_in_command_denied,
    test_default_mode_is_on_when_probe_confirmed,
    test_default_mode_observe_when_probe_unconfirmed,
    test_subagent_is_unaffected_when_on,
    test_observe_mode_allows_and_reports,
    test_off_mode_allows_everything,
    test_fail_open_on_internal_error,
    test_probe_writes_payload_keys,
    test_grep_and_glob_on_a_file_cost_like_a_read,
    test_git_bash_drive_paths_are_understood_on_windows,
    test_composition_of_accepted_parts_and_closing_tails,
    test_broker_runs_cargo_go_node_and_status,
    test_daemon_script_and_verify_ticket_accepted,
    test_small_read_only_commands_and_list_agents,
    test_refusal_says_what_the_root_may_do_itself,
    test_notifications_neither_set_nor_clear_manual,
    test_notifications_do_not_reset_the_edit_budget,
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
