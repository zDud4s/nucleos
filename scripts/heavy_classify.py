"""Argv classifier and main-checkout resolver shared by the heavy-command hook,
the broker (heavy.py) and select_tests.py.

Stdlib only, and it never raises on odd input: a classifier bug must fall open
(treat the command as not heavy), never block a command.
"""

import os
import re
import subprocess
from dataclasses import dataclass, field
from pathlib import Path

_ENV_RE = re.compile(r"^[A-Za-z_][A-Za-z0-9_]*=")

# cargo flags that take a separate value; their value is never a positional.
_CARGO_VALUE_FLAGS = {
    "-p", "--package", "--test", "--bin", "--features", "-F", "--profile",
    "--target", "--target-dir", "--manifest-path", "-j", "--jobs", "--example",
    "--bench", "--config", "-Z", "--color", "--message-format",
}
_NPM_VALUE_FLAGS = {"--prefix", "-C", "--workspace", "-w", "--cwd"}

_CARGO_WEIGHT2 = {"test", "build", "run", "doc"}
_CARGO_WEIGHT1 = {"check", "clippy"}


@dataclass
class Verdict:
    heavy: bool = False
    kind: str | None = None
    weight: int = 0
    filtered: bool = False
    gate: bool = False
    wrapped: bool = False
    inner: list[str] = field(default_factory=list)


def split_env(argv):
    """Split leading NAME=value tokens (and a `time [-p]` keyword) from the rest of the argv."""
    i = 0
    while i < len(argv):
        if _ENV_RE.match(argv[i]):
            i += 1
        elif argv[i] == "time":
            # bash's `time [-p]` keyword: the program it times is what is judged.
            i += 2 if argv[i + 1:i + 2] == ["-p"] else 1
        else:
            break
    return list(argv[:i]), list(argv[i:])


def _prog(token: str) -> str:
    name = token.replace("\\", "/").rsplit("/", 1)[-1].lower()
    for ext in (".exe", ".cmd", ".bat"):
        if name.endswith(ext):
            name = name[: -len(ext)]
            break
    return name


def _base(token: str) -> str:
    return token.replace("\\", "/").rsplit("/", 1)[-1].lower()


def _heavy(kind, weight, filtered=False) -> Verdict:
    return Verdict(heavy=True, kind=kind, weight=weight, filtered=filtered)


def _split_dashdash(args):
    if "--" in args:
        i = args.index("--")
        return args[:i], args[i + 1:]
    return list(args), []


def _cargo_sub(args):
    """Index of the subcommand, skipping +toolchain and flags (with their values)."""
    i = 0
    while i < len(args):
        a = args[i]
        if a.startswith("+"):
            i += 1
        elif a in _CARGO_VALUE_FLAGS:
            i += 2
        elif a.startswith("-"):
            i += 1
        else:
            return i
    return None


def _classify_cargo(args) -> Verdict:
    i = _cargo_sub(args)
    if i is None:
        return Verdict()
    sub, rest = args[i], args[i + 1:]
    if sub == "tauri":
        if rest and rest[0] == "build":
            return _heavy("cargo", 2)
        return Verdict()
    if sub in _CARGO_WEIGHT1:
        return _heavy("cargo", 1)
    if sub == "nextest":
        # `cargo nextest run|list` builds the same test binaries `cargo test` does (the core gate
        # runs it since 2026-10-07); anything else (`self update`, `show-config`) builds nothing.
        if not rest or rest[0] not in ("run", "list"):
            return Verdict()
        before, after = _split_dashdash(rest[1:])
        j, filtered = 0, False
        while j < len(before):
            a = before[j]
            if a in _CARGO_VALUE_FLAGS:
                j += 2
                continue
            if not a.startswith("-"):
                filtered = True
            j += 1
        if any(not a.startswith("-") for a in after):
            filtered = True
        return _heavy("cargo", 2, filtered)
    if sub not in _CARGO_WEIGHT2:
        return Verdict()
    filtered = False
    if sub == "test":
        before, after = _split_dashdash(rest)
        j = 0
        while j < len(before):
            a = before[j]
            if a == "--test" or a.startswith("--test="):
                filtered = True
            if a in _CARGO_VALUE_FLAGS:
                j += 2
                continue
            if not a.startswith("-"):
                filtered = True
            j += 1
        if any(not a.startswith("-") for a in after):
            filtered = True
    return _heavy("cargo", 2, filtered)


def _npm_sub(args):
    i = 0
    while i < len(args):
        a = args[i]
        if a in _NPM_VALUE_FLAGS:
            i += 2
        elif a.startswith("-"):
            i += 1
        else:
            return i
    return None


def _vitest_filtered(args) -> bool:
    for a in args:
        if a in ("-t", "--testNamePattern") or a.startswith("--testNamePattern="):
            return True
    return any(not a.startswith("-") and a not in ("run", "watch") for a in args)


def _classify_npm(args) -> Verdict:
    i = _npm_sub(args)
    if i is None:
        return Verdict()
    sub, rest = args[i], args[i + 1:]
    if sub == "test":
        _, after = _split_dashdash(rest)
        return _heavy("node", 1, _vitest_filtered(after))
    if sub == "run":
        if len(rest) >= 2 and rest[0] == "tauri" and rest[1] == "build":
            return _heavy("node", 2)
        if rest and rest[0] == "build":
            return _heavy("node", 1)
        return Verdict()
    if sub == "exec":
        tool = [a for a in rest if a != "--"]
        if tool and _prog(tool[0]) == "tsc":
            return _heavy("node", 1)
    return Verdict()


def _classify_npx(args) -> Verdict:
    i = 0
    while i < len(args) and args[i].startswith("-"):
        i += 1
    if i >= len(args):
        return Verdict()
    tool, rest = _prog(args[i]), args[i + 1:]
    if tool == "tsc":
        return _heavy("node", 1)
    if tool == "vitest":
        return _heavy("node", 1, _vitest_filtered(rest))
    if tool == "tauri" and rest and rest[0] == "build":
        return _heavy("node", 2)
    return Verdict()


def _classify_go(args) -> Verdict:
    i = 0
    while i < len(args):
        if args[i] == "-C":
            i += 2
        elif args[i].startswith("-"):
            i += 1
        else:
            break
    if i >= len(args):
        return Verdict()
    sub, rest = args[i], args[i + 1:]
    if sub == "test":
        filtered = any(a == "-run" or a.startswith("-run=") for a in rest)
        return _heavy("go", 1, filtered)
    if sub in ("build", "vet"):
        return _heavy("go", 1)
    return Verdict()


def _classify_plain(argv) -> Verdict:
    """Classify an argv that has no env prefix and is not a wrapper or gate."""
    if not argv:
        return Verdict()
    prog, args = _prog(argv[0]), argv[1:]
    if prog == "cargo":
        return _classify_cargo(args)
    if prog == "npm":
        return _classify_npm(args)
    if prog == "npx":
        return _classify_npx(args)
    if prog == "tsc":
        return _heavy("node", 1)
    if prog == "go":
        return _classify_go(args)
    return Verdict()


def _is_python(prog: str) -> bool:
    return prog == "py" or prog.startswith("python")


def _broker_inner(args):
    """argv after heavy.py's own options; a missing `--` is tolerated."""
    i = 0
    if args and args[0] == "hold-worktree":
        i = 1
    while i < len(args):
        a = args[i]
        if a == "--":
            return list(args[i + 1:])
        if a in ("--prio", "--agent", "--kind", "--wait-max"):
            i += 2
        elif a.startswith(("--prio=", "--agent=", "--kind=", "--wait-max=")):
            i += 1
        else:
            break
    return list(args[i:])


def classify(argv) -> Verdict:
    try:
        _, rest = split_env(list(argv))
        if not rest:
            return Verdict()
        prog, args = _prog(rest[0]), rest[1:]

        if _is_python(prog) and args:
            script = _base(args[0])
            if script == "select_tests.py":
                return Verdict(gate=True)
            if script == "heavy.py":
                inner = _broker_inner(args[1:])
                v = classify(inner)
                v.wrapped = True
                v.inner = inner
                return v
        if prog in ("bash", "sh") and args:
            script = _base(args[0])
            if script == "gates.sh":
                return Verdict(gate=True)
            if script == "build-slot.sh":
                inner = list(args[1:])
                v = classify(inner)
                v.gate = True
                v.inner = inner
                return v
        return _classify_plain(rest)
    except Exception:
        return Verdict()


def resolve_main(cwd, env):
    """The main checkout: NUCLEOS_HEAVY_MAIN, else the parent of git's common dir."""
    try:
        override = (env or {}).get("NUCLEOS_HEAVY_MAIN")
        if override:
            return Path(override)
        full_env = {**os.environ, **(env or {})}
        out = subprocess.run(
            ["git", "-C", str(cwd), "rev-parse", "--path-format=absolute", "--git-common-dir"],
            capture_output=True, text=True, env=full_env, timeout=15,
        )
        if out.returncode != 0 or not out.stdout.strip():
            return None
        return Path(out.stdout.strip()).parent
    except Exception:
        return None
