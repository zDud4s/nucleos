"""Failed-first re-runs for the heavy broker (`scripts/heavy.py`).

Agents re-run a whole suite after one test failed. When a brokered TEST command fails, the
failing test ids are recorded, keyed by (worktree, normalized command); the next run of the same
command in that worktree runs only those tests. A pass clears the record, so the run after that
is full again unless a new failure is recorded.

Supported runners: `cargo nextest run`, `cargo test`, `go test`, vitest/jest through `npm test`,
`npm run test`, `npx vitest`, `vitest`. A runner or an argv that cannot be narrowed safely
(a name filter of its own, an existing `-run`/`-E`, a watch mode) runs in full, and says why.

Never silent: every narrowed run prints a stderr line that says which tests ran and that the
rest did NOT. Switches (environment):
  NUCLEOS_HEAVY_FAILFIRST=0   off entirely (no narrowing, no recording)
  NUCLEOS_HEAVY_FULL=1        the full suite is wanted (scripts/gates.sh exports it, which is the
                              signal the daemon's gate/land path inherits); the run is full but
                              still records a failure and clears a record on a pass

State: <broker state dir>/failfirst/<sha1 of worktree + command>.json
  {"worktree", "argv", "runner", "ids", "head", "dirty", "ts"}
The key uses the ORIGINAL argv, never the narrowed one, so a narrowed run updates its own record.
"""

from __future__ import annotations

import hashlib
import json
import os
import re
import subprocess
import sys
import threading
import time
from pathlib import Path

TTL_S = 6 * 3600  # a record older than this is ignored: the failure it names is history
MAX_IDS = 200  # past this the failure is not "one test", and a filter this long is not safe
MAX_KEPT_LINES = 20000  # bound on the output lines the collector holds
_ANSI = re.compile(r"\x1b\[[0-9;?]*[A-Za-z]")
_INTEREST = (b"FAIL", b"TIMEOUT", b"SIGABRT", b"SIGSEGV", b"LEAK")

_CARGO_VALUE_FLAGS = {
    "-p", "--package", "--test", "--bench", "--example", "--bin", "--features", "-F",
    "--manifest-path", "--target", "--target-dir", "--profile", "--exclude", "-j", "--jobs",
    "--color", "--message-format", "--config", "-Z", "--lockfile-path", "--timings",
    "--cargo-profile", "-P", "--partition", "--retries", "--test-threads", "--max-fail",
    "--run-ignored", "--build-jobs", "--cargo-message-format", "--user-config-file",
    "--final-status-level", "--status-level", "--failure-output", "--success-output",
    "--no-tests", "--archive-file", "--extract-to", "--workspace-remap", "--target-dir-remap",
}
_LIBTEST_VALUE_FLAGS = {
    "--test-threads", "--skip", "--format", "--logfile", "--color", "--shuffle-seed",
    "--report-time", "--ensure-time", "--test-timeout",
}
_NX_FAIL = re.compile(
    r"^\s*(?:FAIL|TIMEOUT|LEAK-FAIL|EXEC-FAIL|SIGABRT|SIGSEGV|SIGBUS|SIGILL|"
    r"SIGFPE|SIGTRAP|SIGKILL|SIGTERM|ABORT)\s+\[[^\]]*\]\s+(?:\(\s*\d+/\d+\)\s+)?(\S+)\s+(\S+)\s*$")
_CARGO_FAIL = re.compile(r"^test (\S.*?) \.\.\. FAILED\b")
_GO_FAIL = re.compile(r"^\s*--- FAIL: (\S+)")
_JS_FAIL = re.compile(r"^\s*FAIL\s+(?:\|[^|]*\|\s+)?(\S+?\.(?:test|spec)\.[cm]?[jt]sx?)\b")
_SAFE_NAME = re.compile(r"^[\w:]+$")


# ------------------------------------------------------------------ switches


def disabled(environ=None) -> bool:
    return (environ if environ is not None else os.environ).get("NUCLEOS_HEAVY_FAILFIRST") == "0"


def full_forced(environ=None) -> bool:
    return (environ if environ is not None else os.environ).get("NUCLEOS_HEAVY_FULL") == "1"


# ------------------------------------------------------------------ argv


def _prog(argv: list[str]) -> str:
    base = os.path.basename(argv[0]).lower() if argv else ""
    for ext in (".exe", ".cmd", ".bat", ".ps1"):
        if base.endswith(ext):
            return base[: -len(ext)]
    return base


def normalize(argv: list[str]) -> list[str]:
    return [_prog(argv)] + list(argv[1:])


def _split_dd(args: list[str]) -> tuple[list[str], list[str] | None]:
    if "--" in args:
        i = args.index("--")
        return args[:i], args[i + 1:]
    return args, None


def _positionals(args: list[str], value_flags: set[str]) -> list[str]:
    """Tokens that are neither flags nor a flag's value: for a test runner, name filters."""
    out, skip = [], False
    for a in args:
        if skip:
            skip = False
        elif a.startswith("-"):
            skip = "=" not in a and a in value_flags
        elif not a.startswith("+"):
            out.append(a)
    return out


def detect(argv: list[str]) -> str | None:
    """The runner kind this argv is, or None when it is not a test command we know."""
    if not argv:
        return None
    prog, rest = _prog(argv), list(argv[1:])
    if prog == "cargo":
        words = [a for a in rest if not a.startswith(("-", "+"))]
        if words[:2] == ["nextest", "run"] or words[:2] == ["nextest", "r"]:
            return "nextest"
        if words[:1] == ["test"] and "--no-run" not in rest:
            return "cargo-test"
        return None
    if prog == "go":
        return "go" if rest[:1] == ["test"] else None
    if prog in ("npm", "pnpm", "yarn"):
        words, skip = [], False
        for a in rest:
            if skip:
                skip = False
            elif a in ("--prefix", "-C", "--workspace", "-w"):
                skip = True
            elif not a.startswith("-"):
                words.append(a)
        if words[:1] == ["test"] or words[:2] == ["run", "test"]:
            return "js"
        return None
    if prog == "vitest":
        return "js"
    if prog == "npx" and any(a == "vitest" for a in rest[:3]):
        return "js"
    return None


# ------------------------------------------------------------------ parsing


def parse_failures(runner: str, lines) -> list[str] | None:
    """Failing test ids read from the output lines; None when none could be read."""
    ids: list[str] = []
    seen = set()

    def add(x: str) -> None:
        if x not in seen:
            seen.add(x)
            ids.append(x)

    for raw in lines:
        line = _ANSI.sub("", raw).rstrip("\r\n")
        if runner == "nextest":
            m = _NX_FAIL.match(line)
            if m:
                add(m.group(2))
        elif runner == "cargo-test":
            m = _CARGO_FAIL.match(line)
            if m:
                add(m.group(1))
        elif runner == "go":
            m = _GO_FAIL.match(line)
            if m:
                add(m.group(1).split("/")[0])  # a failing subtest re-runs through its parent
        elif runner == "js":
            m = _JS_FAIL.match(line)
            if m:
                add(m.group(1).replace("\\", "/"))
    return ids or None


# ------------------------------------------------------------------ narrowing


def _insert_before_dd(argv: list[str], extra: list[str]) -> list[str]:
    head, tail = _split_dd(list(argv))
    return head + extra + ([] if tail is None else ["--"] + tail)


def narrow(runner: str, argv: list[str], ids: list[str],
           changed_tests: list[str] | None = None) -> tuple[list[str] | None, str]:
    """(narrowed argv, "") or (None, why it cannot be narrowed safely)."""
    if not ids:
        return None, "the record holds no test ids"
    if len(ids) > MAX_IDS:
        return None, f"{len(ids)} tests failed, too many to name"
    head, tail = _split_dd(list(argv[1:]))
    if runner == "nextest":
        if any(a in ("-E", "--filter-expr") or a.startswith("--filter-expr=") for a in head):
            return None, "the command already has a -E filter expression"
        words = [a for a in head if not a.startswith(("-", "+"))]
        after = head[head.index(words[1]) + 1:] if len(words) > 1 else []
        if _positionals(after, _CARGO_VALUE_FLAGS):
            return None, "the command has a test name filter of its own"
        bad = [i for i in ids if not _SAFE_NAME.match(i)]
        if bad:
            return None, f"test name {bad[0]!r} cannot go into a nextest filter expression safely"
        expr = " | ".join(f"test(={i})" for i in ids)
        return _insert_before_dd(argv, ["-E", expr]), ""
    if runner == "cargo-test":
        words = [a for a in head if not a.startswith(("-", "+"))]
        after = head[head.index(words[0]) + 1:] if words else []
        if _positionals(after, _CARGO_VALUE_FLAGS):
            return None, "the command has a test name filter of its own"
        if tail and _positionals(tail, _LIBTEST_VALUE_FLAGS):
            return None, "the command passes a test name filter after --"
        extra = ([] if tail is not None else ["--"]) + list(ids) + ["--exact"]
        return list(argv) + extra, ""
    if runner == "go":
        if any(a == "-run" or a == "--run" or a.startswith(("-run=", "--run=")) for a in argv[2:]):
            return None, "the command already has a -run pattern"
        bad = [i for i in ids if not re.match(r"^\w+$", i)]
        if bad:
            return None, f"test name {bad[0]!r} cannot go into a -run pattern safely"
        return [argv[0], "test", "-run", "^(" + "|".join(ids) + ")$"] + list(argv[2:]), ""
    if runner == "js":
        prog = _prog(argv)
        if prog in ("npm", "pnpm", "yarn"):
            if tail is not None and _positionals(tail, set()):
                return None, "the command passes file filters after --"
            if any(a in ("--watch", "-w") for a in argv):
                return None, "watch mode"
            files = list(ids) + [f for f in (changed_tests or []) if f not in ids]
            return list(argv) + ([] if tail is not None else ["--"]) + files, ""
        if "run" not in head and "--run" not in head:
            return None, "vitest without `run` is watch mode"
        if _positionals([a for a in head if a not in ("run", "vitest")], set()):
            return None, "the command has a file filter of its own"
        files = list(ids) + [f for f in (changed_tests or []) if f not in ids]
        return list(argv) + files, ""
    return None, f"no narrowing for runner {runner}"


# ------------------------------------------------------------------ state


def _git(cwd: str, *args: str) -> str | None:
    try:
        r = subprocess.run(["git", "-C", cwd, *args], capture_output=True, text=True, timeout=10)
    except Exception:
        return None
    return r.stdout if r.returncode == 0 else None


def _snapshot(cwd: str) -> tuple[str | None, dict]:
    """HEAD and the mtime of every dirty file, to tell later what changed since a failure."""
    head = _git(cwd, "rev-parse", "HEAD")
    dirty: dict = {}
    out = _git(cwd, "status", "--porcelain", "-uall")
    top = _git(cwd, "rev-parse", "--show-toplevel")
    if out and top:
        for line in out.splitlines():
            rel = line[3:].split(" -> ")[-1].strip().strip('"')
            try:
                dirty[rel] = os.stat(os.path.join(top.strip(), rel)).st_mtime_ns
            except OSError:
                dirty[rel] = 0
    return (head.strip() if head else None), dirty


def _is_test_file(path: str) -> bool:
    return re.search(r"\.(?:test|spec)\.[cm]?[jt]sx?$", path) is not None


def changed_tests_since(cwd: str, rec: dict) -> list[str]:
    """Test files touched since the failure was recorded (vitest/jest only: a test file's own
    name is a filter they accept; for the other runners no change-to-test map exists)."""
    old_head, old_dirty = rec.get("head"), rec.get("dirty") or {}
    head, dirty = _snapshot(cwd)
    changed = {p for p, m in dirty.items() if old_dirty.get(p) != m}
    if old_head and head and old_head != head:
        out = _git(cwd, "diff", "--name-only", old_head, head)
        changed.update(out.split() if out else [])
    return sorted(os.path.basename(p) for p in changed if _is_test_file(p))


def _key(cwd: str, argv: list[str]) -> str:
    wt = os.path.normcase(os.path.realpath(cwd))
    blob = json.dumps([wt, normalize(argv)])
    return hashlib.sha1(blob.encode("utf-8")).hexdigest()[:20]


def _path(directory: Path, key: str) -> Path:
    return Path(directory) / "failfirst" / f"{key}.json"


def load(directory: Path, cwd: str, argv: list[str]) -> dict | None:
    try:
        rec = json.loads(_path(directory, _key(cwd, argv)).read_text(encoding="utf-8"))
    except (OSError, ValueError):
        return None
    if not isinstance(rec, dict) or not isinstance(rec.get("ids"), list):
        return None
    if time.time() - float(rec.get("ts") or 0) > TTL_S:
        return None
    return rec


def clear(directory: Path, cwd: str, argv: list[str]) -> None:
    try:
        _path(directory, _key(cwd, argv)).unlink()
    except OSError:
        pass


def save(directory: Path, cwd: str, argv: list[str], runner: str, ids: list[str]) -> None:
    head, dirty = _snapshot(cwd)
    p = _path(directory, _key(cwd, argv))
    p.parent.mkdir(parents=True, exist_ok=True)
    rec = {"worktree": os.path.realpath(cwd), "argv": normalize(argv), "runner": runner,
           "ids": ids, "head": head, "dirty": dirty, "ts": time.time()}
    tmp = p.with_name(p.name + f".{os.getpid()}.tmp")
    tmp.write_text(json.dumps(rec), encoding="utf-8")
    os.replace(tmp, p)


# ------------------------------------------------------------------ the broker's hook


class Collector:
    """Keeps only the output lines that can name a failure, so a long suite costs no memory."""

    def __init__(self) -> None:
        self.lines: list[str] = []
        self._lock = threading.Lock()

    def feed(self, seg: bytes) -> None:
        if any(k in seg for k in _INTEREST):
            with self._lock:
                if len(self.lines) < MAX_KEPT_LINES:
                    self.lines.append(seg.decode("utf-8", "replace"))


class Plan:
    """What the broker does around one run: `run_argv`, a `tap` for the output, `finish`."""

    def __init__(self, directory, cwd, argv, runner, active, narrowed, ids, err) -> None:
        self.directory, self.cwd, self.argv = directory, cwd, argv
        self.runner, self.active, self.narrowed, self.ids = runner, active, narrowed, ids
        self.err = err
        self.collector = Collector() if active else None
        self.run_argv = argv

    @property
    def tap(self):
        return self.collector.feed if self.collector is not None else None

    def say(self, text: str) -> None:
        try:
            self.err.write(f"heavy: failed-first: {text}\n")
            self.err.flush()
        except Exception:
            pass

    def finish(self, code: int, compile_error: bool = False, interrupted: bool = False) -> None:
        if not self.active or interrupted or compile_error:
            return  # nothing ran to a verdict: the record stays as it was
        try:
            if code == 0:
                clear(self.directory, self.cwd, self.argv)
                if self.narrowed:
                    self.say("the narrowed run PASSED, but only the previously failed tests ran, "
                             "not the whole suite; the record is cleared, the next run is full")
                return
            ids = parse_failures(self.runner, self.collector.lines)
            if ids:
                save(self.directory, self.cwd, self.argv, self.runner, ids)
                if not self.narrowed:
                    self.say(f"recorded {len(ids)} failing test(s); the next identical run in "
                             "this worktree will run only those")
            else:
                clear(self.directory, self.cwd, self.argv)
                self.say("could not read failing test ids from the output; the next run is full")
        except Exception as exc:
            self.say(f"bookkeeping failed ({type(exc).__name__}: {exc}); the next run is full")


def prepare(argv: list[str], run_argv: list[str], directory: Path, cwd: str,
            environ=None, err=None) -> Plan:
    """Decide how to run `argv` (`run_argv` is it with the broker's own flags). Never raises."""
    err = err if err is not None else sys.stderr
    inert = Plan(directory, cwd, argv, None, False, False, [], err)
    inert.run_argv = run_argv
    try:
        if disabled(environ):
            return inert
        runner = detect(argv)
        if runner is None:
            return inert
        plan = Plan(directory, cwd, argv, runner, True, False, [], err)
        plan.run_argv = run_argv
        rec = load(directory, cwd, argv)
        if rec is None or rec.get("runner") != runner:
            return plan
        ids = [str(i) for i in rec["ids"]]
        if full_forced(environ):
            plan.say(f"a full run was requested (NUCLEOS_HEAVY_FULL=1); the {len(ids)} recorded "
                     "failure(s) are not run first")
            return plan
        changed = changed_tests_since(cwd, rec) if runner == "js" else []
        narrowed, why = narrow(runner, run_argv, ids, changed)
        if narrowed is None:
            plan.say(f"not narrowed ({why}); running in full")
            return plan
        plan.narrowed, plan.ids, plan.run_argv = True, ids, narrowed
        shown = ", ".join(ids[:5]) + (f" and {len(ids) - 5} more" if len(ids) > 5 else "")
        extra = f" (+ changed test files: {', '.join(changed)})" if changed else ""
        plan.say(f"re-running ONLY the {len(ids)} test(s) that failed last time: {shown}{extra}. "
                 "The rest of the suite did NOT run, so a pass here is not a green suite. "
                 "Force a full run with NUCLEOS_HEAVY_FULL=1 (or turn this off with "
                 "NUCLEOS_HEAVY_FAILFIRST=0).")
        return plan
    except Exception:
        return inert
