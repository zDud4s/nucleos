"""Result cache for the heavy broker (`scripts/heavy.py`).

Measured over 7 days: 205 times the same command ran twice in one worktree within 30 minutes and
exited 0 both times, with the source untouched in between (4.8 h of machine time). The broker's
cargo fingerprint only made the second run cheaper (weight 0); this module skips it.

A command that exited 0 in a FULL run is remembered under
    (worktree realpath, normalized argv, source state, result-relevant env)
and the same key again returns 0 without queueing, taking a token or a worktree lock.

Source state: the git TREE id of the worktree's content as it is on disk, i.e. tracked changes,
deletions and untracked files that .gitignore does not hide. Computed with a temporary copy of the
worktree's own index (`GIT_INDEX_FILE=<copy>`, `git add -A`, `git write-tree`): the copy keeps the
stat cache, so only changed files are re-hashed (~60 ms here; a fresh `read-tree HEAD` index
re-hashes everything, ~360 ms). It is the tree, not HEAD: a commit of verified content leaves the
tree as it was and rightly still hits; HEAD is stored in the entry for the record. Ignored files
(target/, node_modules/) are not part of the state.

Never cached: a failure (and it drops the key's entry), a narrowed (failed-first) success, a run
whose source state changed while it ran, anything under NUCLEOS_HEAVY_FULL=1 (the daemon's final
gate always really runs). A failed-first record for the key means the last run failed: no hit.

Switches (environment):
  NUCLEOS_HEAVY_CACHE=0   off: no lookup, no store
  NUCLEOS_HEAVY_FORCE=1   run this once anyway (a success still refreshes the entry)
State: <broker state dir>/rescache/<sha256 of the key>.json, at most MAX_ENTRIES, TTL_S each.
"""

from __future__ import annotations

import collections
import hashlib
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import time
from pathlib import Path

# Source state does not cover the toolchain, the module caches or the network; an entry is a
# statement about a moment. 12 h is a working half-day: long enough to span an agent session and
# its re-verifications, short enough that a toolchain/dependency drift cannot hide for a day.
TTL_S = 12 * 3600
MAX_ENTRIES = 300
TAIL_LINES = 40
TAIL_CHARS = 6000
_KEEP_SEGMENTS = 600

_ENV_EXACT = {
    "CARGO_TARGET_DIR", "CARGO_BUILD_TARGET_DIR", "CARGO_BUILD_TARGET", "RUSTFLAGS",
    "RUSTDOCFLAGS", "CARGO_ENCODED_RUSTFLAGS", "RUSTC", "RUSTUP_TOOLCHAIN", "GOFLAGS", "GOOS",
    "GOARCH", "CGO_ENABLED", "GOTOOLCHAIN", "GOEXPERIMENT", "NODE_ENV", "CI", "RUST_TEST_THREADS",
    "RUST_MIN_STACK",
}
_ENV_PREFIX = ("CARGO_PROFILE_", "CARGO_FEATURES", "CARGO_BUILD_", "VITE_", "NUCLEOS_")
_ENV_SKIP_PREFIX = ("NUCLEOS_HEAVY",)
_ANSI = re.compile(r"\x1b\[[0-9;?]*[A-Za-z]")


def disabled(environ=None) -> bool:
    return (environ if environ is not None else os.environ).get("NUCLEOS_HEAVY_CACHE") == "0"


def forced(environ=None) -> bool:
    return (environ if environ is not None else os.environ).get("NUCLEOS_HEAVY_FORCE") == "1"


_FINAL_GATE_TREE = re.compile(r"/\.nucleos/worktrees/(?:postgate|integration)-[^/]+(?:/|$)")


def final_gate_tree(cwd) -> bool:
    """A cwd inside the daemon's post-gate or integration tree: the final gate. It must really
    run, whoever calls it (scripts/gates.sh exports NUCLEOS_HEAVY_FULL=1 there too, but the outer
    broker looks the command up before gates.sh runs)."""
    try:
        return _FINAL_GATE_TREE.search(os.path.realpath(cwd).replace("\\", "/") + "/") is not None
    except (OSError, ValueError, TypeError):
        return False


def full_forced(environ=None, cwd=None) -> bool:
    env = environ if environ is not None else os.environ
    if "NUCLEOS_HEAVY_FULL" in env:
        return env.get("NUCLEOS_HEAVY_FULL") == "1"
    return cwd is not None and final_gate_tree(cwd)


# ------------------------------------------------------------------ argv


def _prog(argv: list[str]) -> str:
    base = os.path.basename(argv[0]).lower() if argv else ""
    for ext in (".exe", ".cmd", ".bat", ".ps1"):
        if base.endswith(ext):
            return base[: -len(ext)]
    return base


def normalize(argv: list[str]) -> list[str]:
    return [_prog(argv)] + list(argv[1:])


def _words(args: list[str]) -> list[str]:
    return [a for a in args if not a.startswith(("-", "+"))]


def in_family(argv: list[str]) -> bool:
    """Commands whose log row gets a source state: cargo, go, npm, npx, gates.sh."""
    if not argv:
        return False
    prog = _prog(argv)
    if prog in ("cargo", "go", "npm", "npx", "pnpm", "yarn"):
        return True
    return prog in ("bash", "sh") and any(a.replace("\\", "/").endswith("gates.sh")
                                          for a in argv[1:3])


def cacheable(argv: list[str]) -> bool:
    """Verification commands only: their whole product is the exit code. A build, an install or a
    generator leaves files behind that a skipped run would not."""
    if not in_family(argv):
        return False
    prog, rest = _prog(argv), list(argv[1:])
    if any(a in ("--watch", "-watch", "--fix", "--write", "--no-run") for a in rest):
        return False
    words = _words(rest)
    if prog == "cargo":
        return bool(words) and words[0] in ("check", "clippy", "test", "nextest", "t", "c")
    if prog == "go":
        if any(a.split("=")[0] in ("-bench", "-fuzz", "-cpuprofile", "-memprofile", "-o")
               for a in rest):
            return False
        return bool(words) and words[0] in ("vet", "test")
    if prog in ("npm", "pnpm", "yarn"):
        if words[:1] == ["test"]:
            return True
        return (words[:1] == ["run"] and len(words) > 1
                and re.match(r"(test|lint|typecheck|check|tsc)", words[1]) is not None)
    if prog == "npx":
        if words[:1] == ["vitest"]:
            return "run" in words[1:] or "--run" in rest
        return words[:1] in (["tsc"], ["eslint"], ["jest"], ["prettier"]) and (
            words[0] != "prettier" or "--check" in rest)
    # bash scripts/gates.sh is not cached: it picks its targets from a merge-base diff, and the
    # outer lookup runs before it exports NUCLEOS_HEAVY_FULL. Its inner commands cache.
    return False


# ------------------------------------------------------------------ source state


def _git(cwd: str, *args: str, env=None) -> bytes | None:
    try:
        out = subprocess.run(["git", "-c", "core.quotepath=off", *args], cwd=cwd, env=env,
                             capture_output=True, timeout=60)
    except (OSError, subprocess.SubprocessError):
        return None
    return out.stdout if out.returncode == 0 else None


def source_state(cwd: str) -> tuple[str, str] | None:
    """(tree id of the worktree's content on disk, HEAD) or None when git cannot say."""
    top = _git(cwd, "rev-parse", "--show-toplevel")
    if not top:
        return None
    top_s = top.decode("utf-8", "replace").strip()
    head = (_git(top_s, "rev-parse", "HEAD") or b"").decode().strip()
    idx = _git(top_s, "rev-parse", "--path-format=absolute", "--git-path", "index")
    tmpdir = tempfile.mkdtemp(prefix="heavy-idx-")
    try:
        tmp = os.path.join(tmpdir, "index")
        src = idx.decode("utf-8", "replace").strip() if idx else ""
        env = dict(os.environ, GIT_INDEX_FILE=tmp)
        if src and os.path.isfile(src):
            shutil.copyfile(src, tmp)  # keeps the stat cache: only changed files are re-hashed
        elif head:
            if _git(top_s, "read-tree", "HEAD", env=env) is None:
                return None
        if _git(top_s, "add", "-A", env=env) is None:
            return None
        tree = _git(top_s, "write-tree", env=env)
        return (tree.decode().strip(), head) if tree else None
    except OSError:
        return None
    finally:
        shutil.rmtree(tmpdir, ignore_errors=True)


def env_part(environ=None) -> dict:
    environ = environ if environ is not None else os.environ
    out = {}
    for k, v in environ.items():
        if k in _ENV_EXACT or (k.startswith(_ENV_PREFIX) and not k.startswith(_ENV_SKIP_PREFIX)):
            out[k] = v
    return out


def _key(cwd: str, argv: list[str], tree: str, environ=None) -> str:
    wt = os.path.normcase(os.path.realpath(cwd))
    blob = json.dumps([wt, normalize(argv), tree, env_part(environ)], sort_keys=True)
    return hashlib.sha256(blob.encode("utf-8")).hexdigest()[:32]


# ------------------------------------------------------------------ store


def _dir(directory: Path) -> Path:
    return Path(directory) / "rescache"


def _entry_path(directory: Path, key: str) -> Path:
    return _dir(directory) / f"{key}.json"


def _load(directory: Path, key: str) -> dict | None:
    try:
        rec = json.loads(_entry_path(directory, key).read_text(encoding="utf-8"))
    except (OSError, ValueError):
        return None
    if not isinstance(rec, dict) or rec.get("exit") != 0:
        return None
    if time.time() - float(rec.get("ts") or 0) > TTL_S:
        return None
    return rec


def _drop(directory: Path, key: str) -> None:
    try:
        _entry_path(directory, key).unlink()
    except OSError:
        pass


def _prune(directory: Path) -> None:
    files = []
    for p in _dir(directory).glob("*.json"):
        try:
            files.append((p.stat().st_mtime, p))
        except OSError:
            pass
    files.sort()
    now = time.time()
    keep = []
    for m, p in files:
        if now - m > TTL_S:
            p.unlink(missing_ok=True)
        else:
            keep.append(p)
    for p in keep[: max(0, len(keep) - MAX_ENTRIES)]:
        p.unlink(missing_ok=True)


def _store(directory: Path, key: str, rec: dict) -> None:
    p = _entry_path(directory, key)
    p.parent.mkdir(parents=True, exist_ok=True)
    tmp = p.with_name(p.name + f".{os.getpid()}.tmp")
    tmp.write_text(json.dumps(rec), encoding="utf-8")
    os.replace(tmp, p)
    _prune(directory)


def _tail(segments) -> str:
    text = b"".join(s for s in segments if not (s.endswith(b"\r") and b"\n" not in s))
    lines = _ANSI.sub("", text.decode("utf-8", "replace")).replace("\r\n", "\n").split("\n")
    while lines and not lines[-1].strip():
        lines.pop()
    return "\n".join(lines[-TAIL_LINES:])[-TAIL_CHARS:]


# ------------------------------------------------------------------ the broker's hook


class Plan:
    """`hit` (the stored entry, when the run can be skipped), `state` (tree id or None), `tap`
    for the output and `finish` after the run."""

    def __init__(self, directory, cwd, argv, err, out) -> None:
        self.directory, self.cwd, self.argv, self.err, self.out = directory, cwd, argv, err, out
        self.hit: dict | None = None
        self.state: str | None = None
        self.head: str = ""
        self.key: str | None = None
        self.store = False  # a full success of this run may be remembered
        self._segs: collections.deque = collections.deque(maxlen=_KEEP_SEGMENTS)
        self.tap = None

    def _feed(self, seg: bytes) -> None:
        self._segs.append(seg)

    def say(self, text: str) -> None:
        try:
            self.err.write(f"heavy: cache: {text}\n")
            self.err.flush()
        except Exception:
            pass

    def replay(self) -> None:
        """Tell the caller why nothing ran, then show what the recorded run ended with."""
        rec = self.hit or {}
        when = time.strftime("%Y-%m-%d %H:%M:%S", time.localtime(float(rec.get("ts") or 0)))
        age = int(max(0, time.time() - float(rec.get("ts") or 0)) // 60)
        self.say(f"CACHED result, nothing ran: this exact command exited 0 at {when} ({age} min "
                 f"ago) on the identical source state (tree {str(self.state)[:12]}). Force a run "
                 "with NUCLEOS_HEAVY_FORCE=1 (or turn the cache off with NUCLEOS_HEAVY_CACHE=0).")
        tail = rec.get("tail") or ""
        try:
            if tail:
                self.err.write("heavy: cache: --- tail of the recorded output ---\n")
                self.out.write(tail + "\n")
                self.out.flush()
                self.err.write("heavy: cache: --- end of recorded output ---\n")
            self.err.flush()
        except Exception:
            pass

    def finish(self, code: int, *, narrowed: bool = False, interrupted: bool = False) -> None:
        """Remember a full success; forget the key on a failure. Never raises."""
        if self.key is None or interrupted or self.hit is not None:
            return
        try:
            if code != 0:
                _drop(self.directory, self.key)
                return
            if narrowed:
                _drop(self.directory, self.key)
                return
            if not self.store:
                return
            now = source_state(self.cwd)
            if now is None or now[0] != self.state:
                self.say("the source changed while the command ran: the result is not cached")
                return
            _store(self.directory, self.key, {
                "worktree": os.path.realpath(self.cwd), "argv": normalize(self.argv),
                "tree": self.state, "head": self.head, "exit": 0, "ts": time.time(),
                "tail": _tail(self._segs)})
        except Exception as exc:
            self.say(f"bookkeeping failed ({type(exc).__name__}: {exc})")


def prepare(argv: list[str], directory: Path, cwd: str, environ=None, err=None, out=None) -> Plan:
    """Look the command up. Never raises; an inert plan (no key, no hit) on any problem."""
    err = err if err is not None else sys.stderr
    out = out if out is not None else sys.stdout
    plan = Plan(directory, cwd, argv, err, out)
    try:
        if disabled(environ) or not in_family(argv):
            return plan
        st = source_state(cwd)
        if st is None:
            return plan
        plan.state, plan.head = st
        if not cacheable(argv):
            return plan
        plan.key = _key(cwd, argv, plan.state, environ)
        plan.tap = plan._feed
        if full_forced(environ, cwd):
            return plan  # the daemon's final gate: really run, and store nothing
        plan.store = True
        if forced(environ):
            return plan
        if _failfirst_pending(directory, cwd, argv):
            return plan
        plan.hit = _load(directory, plan.key)
    except Exception:
        plan.hit = None
    return plan


def _failfirst_pending(directory: Path, cwd: str, argv: list[str]) -> bool:
    """A failed-first record means the last run of this command failed."""
    try:
        import heavy_failfirst  # type: ignore

        return heavy_failfirst.load(directory, cwd, argv) is not None
    except ImportError:
        return False
