#!/usr/bin/env python3
"""Cover `scripts/migrate-ai-layout.py` (plan 2026-11-09 ai-layout chunks 4-6, packet P5).

The script moves a repo's `.ai/` agent material into `<state_dir>` and archives everything else
(spec 2026-11-09-migracao-ai-para-state-dir-design, D1, D4, section 4). Dry-run is the default;
`--apply` never deletes and never overwrites anything.

RED shape: the script is loaded lazily by path (its name has a hyphen, so `importlib`). Before it
exists every test fails on its own with a one-line "script missing" reason.

The interface these tests fix:

* `main(argv: list[str]) -> int`, flags `--repo`, `--state-dir`, `--archive`, `--apply`.
* `--state-dir` defaults to `state_dir` in `<repo>/.nucleos/workflow/project.json`; missing or
  unreadable and no flag: one line, return 2.
* `--archive` defaults to `<dev_home>/archive/ai-layout-2026-11-09`, `dev_home` being
  `$NUCLEOS_DEV_HOME`, else `C:\\nucleos-dev`.
* First output line: `stop the daemon before --apply (it scans specs and reads memory)`.
* No `<repo>/.ai`: `nothing to migrate`, return 0.
* One line per action, `move|archive|refuse <src> -> <dst>`, then `N move, N archive, N refuse`.
* Any collision or refusal: all printed, nothing changed, return 1.
* Dry-run ends with `dry run: nothing changed; re-run with --apply`, return 0.

Every test works under `tempfile.TemporaryDirectory()` and passes `--repo` and `--archive`
explicitly; none touches the real `.ai/`, state_dir or archive.

Run:  python scripts/test-migrate-ai-layout.py
"""

import contextlib
import importlib.util
import io
import json
import os
import re
import sys
import tempfile
import traceback
import unittest
from unittest import mock

ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), ".."))
SCRIPT = os.path.join(ROOT, "scripts", "migrate-ai-layout.py")

WARNING = "stop the daemon before --apply (it scans specs and reads memory)"
DRY_RUN_LAST = "dry run: nothing changed; re-run with --apply"
SUMMARY = re.compile(r"(\d+) move, (\d+) archive, (\d+) refuse")

_MODULE = None


def _load():
    global _MODULE
    if _MODULE is not None:
        return _MODULE
    if not os.path.isfile(SCRIPT):
        raise AssertionError(f"script missing: {SCRIPT}")
    spec = importlib.util.spec_from_file_location("migrate_ai_layout", SCRIPT)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    if not callable(getattr(module, "main", None)):
        raise AssertionError("migrate-ai-layout.py has no main(argv)")
    _MODULE = module
    return module


def run(argv, env=None):
    """Call main(argv) with stdout/stderr captured. Returns (rc, stdout lines)."""
    module = _load()
    out, err = io.StringIO(), io.StringIO()
    patched = dict(env or {})
    with mock.patch.dict(os.environ, patched), contextlib.redirect_stdout(
        out
    ), contextlib.redirect_stderr(err):
        try:
            rc = module.main(list(argv))
        except SystemExit as exc:
            rc = exc.code if isinstance(exc.code, int) else (0 if exc.code is None else 1)
    lines = [line for line in out.getvalue().splitlines()]
    return rc, lines


def run_full(argv, env=None):
    """Like `run`, but also returns stderr lines: (rc, stdout lines, stderr lines)."""
    module = _load()
    out, err = io.StringIO(), io.StringIO()
    with mock.patch.dict(os.environ, dict(env or {})), contextlib.redirect_stdout(
        out
    ), contextlib.redirect_stderr(err):
        try:
            rc = module.main(list(argv))
        except SystemExit as exc:
            rc = exc.code if isinstance(exc.code, int) else (0 if exc.code is None else 1)
    return rc, out.getvalue().splitlines(), err.getvalue().splitlines()


def make_dir_link(target, link):
    """A directory link at `link` -> `target`: a junction on Windows, else a symlink.

    Returns False when neither can be created (Windows without the symlink privilege).
    """
    if os.name == "nt":
        try:
            import _winapi

            _winapi.CreateJunction(target, link)
            return True
        except Exception:  # noqa: BLE001 - fall back to a symlink
            pass
    try:
        os.symlink(target, link, target_is_directory=True)
        return True
    except (OSError, NotImplementedError, AttributeError):
        return False


def remove_dir_link(link):
    """Drop the link itself (never its target), best effort."""
    for remove in (os.unlink, os.rmdir):
        try:
            remove(link)
            return
        except OSError:
            continue


def write(path, data):
    os.makedirs(os.path.dirname(path), exist_ok=True)
    mode = "wb" if isinstance(data, bytes) else "w"
    kwargs = {} if isinstance(data, bytes) else {"encoding": "utf-8", "newline": ""}
    with open(path, mode, **kwargs) as fh:
        fh.write(data)


def read(path):
    with open(path, "rb") as fh:
        return fh.read()


def snapshot(top):
    """Every file and directory under `top`, with file bytes, for a no-change check."""
    tree = {}
    for dirpath, dirnames, filenames in os.walk(top):
        for name in dirnames:
            tree[os.path.relpath(os.path.join(dirpath, name), top)] = None
        for name in filenames:
            full = os.path.join(dirpath, name)
            tree[os.path.relpath(full, top)] = read(full)
    return tree


def norm(text):
    return text.replace("\\", "/").lower() if os.name == "nt" else text


class Layout:
    """`<tmp>/repo/.ai`, `<tmp>/state`, `<tmp>/archive`."""

    def __init__(self, tmp):
        self.tmp = tmp
        self.repo = os.path.join(tmp, "repo")
        self.ai = os.path.join(self.repo, ".ai")
        self.state = os.path.join(tmp, "state")
        self.archive = os.path.join(tmp, "archive")
        os.makedirs(self.repo)
        os.makedirs(self.state)

    def ai_file(self, rel, data):
        write(os.path.join(self.ai, *rel.split("/")), data)

    def state_file(self, rel, data):
        write(os.path.join(self.state, *rel.split("/")), data)

    def at(self, base, rel):
        return os.path.join(base, *rel.split("/"))

    def argv(self, *extra, state=True):
        args = ["--repo", self.repo, "--archive", self.archive]
        if state:
            args += ["--state-dir", self.state]
        return args + list(extra)


def isolated_env(tmp):
    return {"NUCLEOS_DEV_HOME": os.path.join(tmp, "devhome")}


def action_lines(lines, verb):
    return [line for line in lines if line.startswith(verb + " ")]


def summary(lines):
    for line in lines:
        m = SUMMARY.search(line)
        if m:
            return tuple(int(g) for g in m.groups())
    raise AssertionError(f"no summary line in output: {lines!r}")


def has_diff(lines, marker):
    """A unified diff was printed whose body mentions `marker`."""
    heads = any(line.startswith("---") for line in lines) and any(
        line.startswith("+++") for line in lines
    )
    body = any(
        line[:1] in "+-" and marker in line and not line.startswith(("---", "+++"))
        for line in lines
    )
    return heads and body


# --- tests -------------------------------------------------------------------


def test_dry_run_lists_actions_and_changes_nothing():
    with tempfile.TemporaryDirectory() as tmp:
        lay = Layout(tmp)
        lay.ai_file("specs/a.md", "spec a\n")
        lay.ai_file("TODO.md", "todo\n")
        before = snapshot(tmp)
        rc, lines = run(lay.argv(), isolated_env(tmp))
        assert rc == 0, f"dry run must return 0, got {rc}: {lines!r}"
        assert snapshot(tmp) == before, "dry run changed the filesystem"
        moves = action_lines(lines, "move")
        archives = action_lines(lines, "archive")
        assert any("a.md" in line for line in moves), f"no move line for specs/a.md: {lines!r}"
        assert any("TODO.md" in line for line in archives), f"no archive line for TODO.md: {lines!r}"
        assert all(" -> " in line for line in moves + archives), f"action lines lack ' -> ': {lines!r}"
        assert summary(lines) == (1, 1, 0), f"summary: {summary(lines)}"
        assert lines[-1] == DRY_RUN_LAST, f"last line: {lines[-1]!r}"


def test_apply_moves_specs_html_plans_notes_spikes_handoffs_keeping_subpaths():
    rels = [
        "specs/a.md",
        "specs/sub/b.html",
        "plans/p.md",
        "notes/deep/n.md",
        "spikes/s/x.txt",
        "handoffs/h.md",
    ]
    with tempfile.TemporaryDirectory() as tmp:
        lay = Layout(tmp)
        for rel in rels:
            lay.ai_file(rel, f"content of {rel}\n")
        rc, lines = run(lay.argv("--apply"), isolated_env(tmp))
        assert rc == 0, f"apply must return 0, got {rc}: {lines!r}"
        for rel in rels:
            dst = lay.at(lay.state, rel)
            assert os.path.isfile(dst), f"{rel} not moved to state: {lines!r}"
            assert read(dst) == f"content of {rel}\n".encode(), f"{rel} bytes changed"
            assert not os.path.exists(lay.at(lay.ai, rel)), f"{rel} still under .ai"
            assert not os.path.exists(lay.at(lay.archive, rel)), f"{rel} archived, not moved"
        assert summary(lines) == (len(rels), 0, 0), f"summary: {summary(lines)}"


def test_different_same_name_refuses_and_touches_nothing():
    with tempfile.TemporaryDirectory() as tmp:
        lay = Layout(tmp)
        lay.ai_file("specs/a.md", "ai version\n")
        lay.state_file("specs/a.md", "state version\n")
        lay.ai_file("plans/unrelated.md", "unrelated\n")
        lay.ai_file("TODO.md", "todo\n")
        before = snapshot(tmp)
        for argv in (lay.argv(), lay.argv("--apply")):
            rc, lines = run(argv, isolated_env(tmp))
            assert rc == 1, f"collision must return 1 ({argv[-1]}), got {rc}: {lines!r}"
            assert any(
                ("a.md" in line) and (line.startswith("refuse") or "collision" in line.lower())
                for line in lines
            ), f"collision on specs/a.md not reported: {lines!r}"
            assert snapshot(tmp) == before, f"a collision changed the filesystem ({argv[-1]})"
        assert read(lay.at(lay.ai, "specs/a.md")) == b"ai version\n"
        assert read(lay.at(lay.state, "specs/a.md")) == b"state version\n"
        assert os.path.isfile(lay.at(lay.ai, "plans/unrelated.md")), "unrelated file moved"
        assert not os.path.exists(lay.at(lay.state, "plans/unrelated.md")), "unrelated file moved"


def test_identical_same_name_is_archived_not_duplicated():
    with tempfile.TemporaryDirectory() as tmp:
        lay = Layout(tmp)
        lay.ai_file("specs/a.md", "same bytes\n")
        lay.state_file("specs/a.md", "same bytes\n")
        rc, lines = run(lay.argv(), isolated_env(tmp))
        assert rc == 0, f"dry run: {rc}: {lines!r}"
        assert any("a.md" in line for line in action_lines(lines, "archive")), lines
        assert not any("a.md" in line for line in action_lines(lines, "move")), lines
        rc, lines = run(lay.argv("--apply"), isolated_env(tmp))
        assert rc == 0, f"apply: {rc}: {lines!r}"
        assert read(lay.at(lay.state, "specs/a.md")) == b"same bytes\n", "state copy changed"
        assert read(lay.at(lay.archive, "specs/a.md")) == b"same bytes\n", "duplicate not archived"
        assert not os.path.exists(lay.at(lay.ai, "specs/a.md")), ".ai copy left behind"
        assert sorted(os.listdir(lay.at(lay.state, "specs"))) == ["a.md"], "state duplicated"


def test_everything_else_is_archived_preserving_tree():
    rels = ["autopilot.yaml", "TODO.md", "dashboard/x", "workflow/y/z"]
    with tempfile.TemporaryDirectory() as tmp:
        lay = Layout(tmp)
        for rel in rels:
            lay.ai_file(rel, f"{rel}\n")
        rc, lines = run(lay.argv("--apply"), isolated_env(tmp))
        assert rc == 0, f"apply: {rc}: {lines!r}"
        for rel in rels:
            dst = lay.at(lay.archive, rel)
            assert os.path.isfile(dst), f"{rel} not archived at its subpath: {lines!r}"
            assert read(dst) == f"{rel}\n".encode()
            assert not os.path.exists(lay.at(lay.state, rel)), f"{rel} went to state"
        assert summary(lines) == (0, len(rels), 0), f"summary: {summary(lines)}"


def test_memory_and_decisions_are_archived_with_a_diff_and_state_copy_untouched():
    with tempfile.TemporaryDirectory() as tmp:
        lay = Layout(tmp)
        lay.ai_file("memory.md", "# memory\n- old ai entry\n")
        lay.state_file("memory.md", "# memory\n- state entry\n")
        lay.ai_file("decisions.md", "# decisions\n- ai decision\n")
        rc, lines = run(lay.argv(), isolated_env(tmp))
        assert rc == 0, f"memory/decisions must not refuse: {rc}: {lines!r}"
        archives = action_lines(lines, "archive")
        assert any("memory.md" in line for line in archives), lines
        assert any("decisions.md" in line for line in archives), lines
        assert not action_lines(lines, "move"), f"memory/decisions must never move: {lines!r}"
        assert has_diff(lines, "old ai entry"), f"no unified diff for memory.md: {lines!r}"
        assert any(
            re.search(r"no .* copy", line) for line in lines
        ), f"decisions.md without a state copy not reported: {lines!r}"
        rc, lines = run(lay.argv("--apply"), isolated_env(tmp))
        assert rc == 0, f"apply: {rc}: {lines!r}"
        assert read(lay.at(lay.state, "memory.md")) == b"# memory\n- state entry\n", "state memory touched"
        assert not os.path.exists(lay.at(lay.state, "decisions.md")), "decisions.md copied to state"
        assert read(lay.at(lay.archive, "memory.md")) == b"# memory\n- old ai entry\n"
        assert read(lay.at(lay.archive, "decisions.md")) == b"# decisions\n- ai decision\n"


def test_memory_archive_and_retractions_move_to_state_root_when_absent():
    with tempfile.TemporaryDirectory() as tmp:
        lay = Layout(tmp)
        lay.ai_file("memory-archive.md", "archived entries\n")
        lay.ai_file("memory-retractions.jsonl", '{"id": 1}\n')
        rc, lines = run(lay.argv("--apply"), isolated_env(tmp))
        assert rc == 0, f"apply: {rc}: {lines!r}"
        assert read(lay.at(lay.state, "memory-archive.md")) == b"archived entries\n"
        assert read(lay.at(lay.state, "memory-retractions.jsonl")) == b'{"id": 1}\n'
        assert not os.path.exists(lay.at(lay.archive, "memory-archive.md"))
        assert summary(lines) == (2, 0, 0), f"summary: {summary(lines)}"


def test_memory_archive_differing_is_archived_with_diff_not_refused():
    with tempfile.TemporaryDirectory() as tmp:
        lay = Layout(tmp)
        lay.ai_file("memory-archive.md", "entry one\nai only entry\n")
        lay.state_file("memory-archive.md", "entry one\nstate only entry\n")
        rc, lines = run(lay.argv(), isolated_env(tmp))
        assert rc == 0, f"a differing memory-archive.md must not refuse: {rc}: {lines!r}"
        assert any("memory-archive.md" in line for line in action_lines(lines, "archive")), lines
        assert not action_lines(lines, "refuse"), lines
        assert has_diff(lines, "ai only entry"), f"no diff printed: {lines!r}"
        rc, lines = run(lay.argv("--apply"), isolated_env(tmp))
        assert rc == 0, f"apply: {rc}: {lines!r}"
        assert read(lay.at(lay.state, "memory-archive.md")) == b"entry one\nstate only entry\n"
        assert read(lay.at(lay.archive, "memory-archive.md")) == b"entry one\nai only entry\n"


def test_existing_archive_destination_refuses():
    with tempfile.TemporaryDirectory() as tmp:
        lay = Layout(tmp)
        lay.ai_file("TODO.md", "new todo\n")
        lay.ai_file("specs/a.md", "spec\n")
        write(lay.at(lay.archive, "TODO.md"), "older archived todo\n")
        before = snapshot(tmp)
        for argv in (lay.argv(), lay.argv("--apply")):
            rc, lines = run(argv, isolated_env(tmp))
            assert rc == 1, f"existing archive destination must return 1, got {rc}: {lines!r}"
            assert any(
                ("TODO.md" in line) and (line.startswith("refuse") or "collision" in line.lower())
                for line in lines
            ), f"archive collision not reported: {lines!r}"
            assert snapshot(tmp) == before, f"filesystem changed ({argv[-1]})"


def test_apply_removes_the_emptied_ai_dir():
    with tempfile.TemporaryDirectory() as tmp:
        lay = Layout(tmp)
        lay.ai_file("specs/deep/a.md", "a\n")
        lay.ai_file("dashboard/x", "x\n")
        os.makedirs(lay.at(lay.ai, "empty/also-empty"))
        rc, lines = run(lay.argv("--apply"), isolated_env(tmp))
        assert rc == 0, f"apply: {rc}: {lines!r}"
        assert not os.path.exists(lay.ai), f".ai still present: {os.listdir(lay.ai)!r}"
        assert os.path.isdir(lay.repo), "the repo itself was removed"


def test_state_dir_comes_from_project_json_and_flag_overrides_it():
    with tempfile.TemporaryDirectory() as tmp:
        lay = Layout(tmp)
        lay.ai_file("specs/a.md", "a\n")
        from_json = os.path.join(tmp, "state-from-json")
        os.makedirs(from_json)
        write(
            os.path.join(lay.repo, ".nucleos", "workflow", "project.json"),
            json.dumps({"id": "t", "state_dir": from_json}),
        )
        rc, lines = run(lay.argv("--apply", state=False), isolated_env(tmp))
        assert rc == 0, f"apply via project.json: {rc}: {lines!r}"
        assert os.path.isfile(os.path.join(from_json, "specs", "a.md")), "project.json state_dir ignored"

        lay.ai_file("specs/b.md", "b\n")
        rc, lines = run(lay.argv("--apply"), isolated_env(tmp))
        assert rc == 0, f"apply via flag: {rc}: {lines!r}"
        assert os.path.isfile(lay.at(lay.state, "specs/b.md")), "--state-dir did not override"
        assert not os.path.exists(os.path.join(from_json, "specs", "b.md")), "project.json won over the flag"


def test_missing_project_json_without_flag_exits_2():
    with tempfile.TemporaryDirectory() as tmp:
        lay = Layout(tmp)
        lay.ai_file("specs/a.md", "a\n")
        before = snapshot(tmp)
        rc, lines = run(lay.argv(state=False), isolated_env(tmp))
        assert rc == 2, f"missing project.json must return 2, got {rc}: {lines!r}"
        assert snapshot(tmp) == before

        write(os.path.join(lay.repo, ".nucleos", "workflow", "project.json"), "{not json")
        before = snapshot(tmp)
        rc, lines = run(lay.argv("--apply", state=False), isolated_env(tmp))
        assert rc == 2, f"unreadable project.json must return 2, got {rc}: {lines!r}"
        assert snapshot(tmp) == before


def test_no_ai_dir_is_a_no_op():
    with tempfile.TemporaryDirectory() as tmp:
        lay = Layout(tmp)
        before = snapshot(tmp)
        for argv in (lay.argv(), lay.argv("--apply")):
            rc, lines = run(argv, isolated_env(tmp))
            assert rc == 0, f"no .ai must return 0, got {rc}: {lines!r}"
            assert any("nothing to migrate" in line for line in lines), lines
            assert snapshot(tmp) == before


def test_archive_default_honours_nucleos_dev_home():
    with tempfile.TemporaryDirectory() as tmp:
        lay = Layout(tmp)
        lay.ai_file("TODO.md", "todo\n")
        dev_home = os.path.join(tmp, "devhome")
        argv = ["--repo", lay.repo, "--state-dir", lay.state]
        before = snapshot(tmp)
        rc, lines = run(argv, {"NUCLEOS_DEV_HOME": dev_home})
        assert rc == 0, f"dry run: {rc}: {lines!r}"
        assert snapshot(tmp) == before, "dry run with the default archive changed the filesystem"
        expected = norm(os.path.join(dev_home, "archive", "ai-layout-2026-11-09"))
        archives = action_lines(lines, "archive")
        assert any(
            expected in norm(line) and "TODO.md".lower() in norm(line).lower() for line in archives
        ), f"archive destination not under {expected}: {lines!r}"


def test_first_line_warns_to_stop_the_daemon():
    with tempfile.TemporaryDirectory() as tmp:
        lay = Layout(tmp)
        rc, lines = run(lay.argv(), isolated_env(tmp))
        assert lines and lines[0] == WARNING, f"first line (no .ai): {lines[:1]!r}"
        lay.ai_file("specs/a.md", "a\n")
        for argv in (lay.argv(), lay.argv("--apply")):
            rc, lines = run(argv, isolated_env(tmp))
            assert lines and lines[0] == WARNING, f"first line ({argv[-1]}): {lines[:1]!r}"


def test_refuses_when_ai_itself_is_a_link():
    with tempfile.TemporaryDirectory() as tmp:
        lay = Layout(tmp)
        target = os.path.join(tmp, "real-ai")
        write(os.path.join(target, "specs", "x.md"), "x\n")
        if not make_dir_link(target, lay.ai):
            raise unittest.SkipTest("cannot create a directory junction or symlink here")
        try:
            target_before = snapshot(target)
            state_before = snapshot(lay.state)
            for argv in (lay.argv(), lay.argv("--apply")):
                rc, lines = run(argv, isolated_env(tmp))
                assert rc == 1, f".ai as a link must return 1 ({argv[-1]}), got {rc}: {lines!r}"
                assert any(
                    ".ai" in line
                    and ("link" in line.lower() or "junction" in line.lower())
                    for line in lines
                ), f".ai not reported as a link ({argv[-1]}): {lines!r}"
                assert snapshot(target) == target_before, f"link target changed ({argv[-1]})"
                assert snapshot(lay.state) == state_before, f"state changed ({argv[-1]})"
                assert not os.path.lexists(lay.archive), f"archive created ({argv[-1]})"
        finally:
            remove_dir_link(lay.ai)


def test_refuses_an_archive_or_state_dir_inside_ai():
    with tempfile.TemporaryDirectory() as tmp:
        lay = Layout(tmp)
        lay.ai_file("specs/a.md", "a\n")
        lay.ai_file("TODO.md", "todo\n")
        cases = [
            (
                "archive inside .ai",
                ["--repo", lay.repo, "--state-dir", lay.state, "--archive", lay.at(lay.ai, "arch")],
            ),
            (
                "state_dir inside .ai",
                ["--repo", lay.repo, "--state-dir", lay.at(lay.ai, "state"), "--archive", lay.archive],
            ),
            (
                ".ai inside the archive",
                ["--repo", lay.repo, "--state-dir", lay.state, "--archive", lay.repo],
            ),
        ]
        for label, base in cases:
            for argv in (base, base + ["--apply"]):
                before = snapshot(tmp)
                rc, lines = run(argv, isolated_env(tmp))
                assert rc in (1, 2), f"{label} ({argv[-1]}) must refuse (1 or 2), got {rc}: {lines!r}"
                assert any(
                    line.startswith("refuse") or "inside" in line.lower() for line in lines
                ), f"{label} ({argv[-1]}): refusal not reported: {lines!r}"
                assert snapshot(tmp) == before, f"{label} ({argv[-1]}) changed the filesystem"


def test_apply_reports_a_partial_failure_and_exits_1():
    with tempfile.TemporaryDirectory() as tmp:
        lay = Layout(tmp)
        for name in ("a.md", "b.md", "c.md"):
            lay.ai_file(f"specs/{name}", f"{name}\n")
        module = _load()
        real_move = module.shutil.move
        calls = []

        def flaky_move(src, dst, *args, **kwargs):
            calls.append(src)
            if len(calls) == 2:
                raise PermissionError(13, "Access is denied", src)
            return real_move(src, dst, *args, **kwargs)

        with mock.patch.object(module.shutil, "move", side_effect=flaky_move):
            try:
                rc, out, err = run_full(lay.argv("--apply"), isolated_env(tmp))
            except Exception as exc:  # noqa: BLE001 - the point is that nothing escapes
                raise AssertionError(
                    f"a failed move escaped main(): {type(exc).__name__}: {exc}"
                ) from None
        assert rc == 1, f"a partial apply must return 1, got {rc}: {out!r} {err!r}"
        assert len(calls) >= 2, f"the failing move was never reached: {calls!r}"
        first = os.path.relpath(calls[0], lay.ai).replace(os.sep, "/")
        rest = [r for r in ("specs/a.md", "specs/b.md", "specs/c.md") if r != first]
        assert os.path.isfile(lay.at(lay.state, first)), f"{first} not at its destination"
        assert not os.path.exists(lay.at(lay.ai, first)), f"{first} still under .ai"
        for rel in rest:
            assert os.path.isfile(lay.at(lay.ai, rel)), f"{rel} no longer under .ai"
            assert not os.path.exists(lay.at(lay.state, rel)), f"{rel} reached state"
        # The report comes after the plan's summary line (or on stderr).
        idx = next((i for i, line in enumerate(out) if SUMMARY.search(line)), -1)
        report = "\n".join(out[idx + 1 :] + err)
        low = report.lower()
        assert "moved" in low, f"report does not say what was moved: {report!r}"
        assert "left" in low, f"report does not say what is left: {report!r}"
        assert "re-run" in low, f"report does not say to re-run: {report!r}"
        assert os.path.basename(first) in report, f"report does not name {first}: {report!r}"
        for rel in rest:
            assert os.path.basename(rel) in report, f"report does not name {rel}: {report!r}"
        assert "Traceback" not in report, f"a traceback was printed: {report!r}"


TESTS = [
    test_dry_run_lists_actions_and_changes_nothing,
    test_apply_moves_specs_html_plans_notes_spikes_handoffs_keeping_subpaths,
    test_different_same_name_refuses_and_touches_nothing,
    test_identical_same_name_is_archived_not_duplicated,
    test_everything_else_is_archived_preserving_tree,
    test_memory_and_decisions_are_archived_with_a_diff_and_state_copy_untouched,
    test_memory_archive_and_retractions_move_to_state_root_when_absent,
    test_memory_archive_differing_is_archived_with_diff_not_refused,
    test_existing_archive_destination_refuses,
    test_apply_removes_the_emptied_ai_dir,
    test_state_dir_comes_from_project_json_and_flag_overrides_it,
    test_missing_project_json_without_flag_exits_2,
    test_no_ai_dir_is_a_no_op,
    test_archive_default_honours_nucleos_dev_home,
    test_first_line_warns_to_stop_the_daemon,
    test_refuses_when_ai_itself_is_a_link,
    test_refuses_an_archive_or_state_dir_inside_ai,
    test_apply_reports_a_partial_failure_and_exits_1,
]


def main():
    failed = []
    skipped = 0
    for test in TESTS:
        try:
            test()
        except unittest.SkipTest as exc:
            skipped += 1
            print(f"skip {test.__name__}: {exc}")
        except AssertionError as exc:
            failed.append(test.__name__)
            print(f"FAIL {test.__name__}: {exc}")
        except Exception as exc:  # noqa: BLE001 - one test must not hide the others
            failed.append(test.__name__)
            print(f"FAIL {test.__name__}: {type(exc).__name__}: {exc}")
            traceback.print_exc(limit=3)
        else:
            print(f"ok   {test.__name__}")
    print(f"\n{len(TESTS) - len(failed) - skipped}/{len(TESTS)} passed, {skipped} skipped")
    sys.exit(1 if failed else 0)


if __name__ == "__main__":
    main()
