"""PreToolUse hook (Bash, PowerShell, Agent): route heavy commands through the broker.

Reads the hook payload on stdin. For a heavy segment it either rewrites that segment
in place (`updatedInput`, never a permission decision) so it runs under the broker
`<main>/scripts/heavy.py`, or denies it (subagents may not run full builds/gates).

Fail-open: any bug, bad input or missing broker prints nothing and exits 0.
"""

import importlib.util
import json
import os
import re
import shlex
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent

DENY_REASON = (
    "Full builds and gates belong to the orchestrator, once, at the end; "
    "run only your own tests with a filter (test_one), e.g. "
    "`cargo test -p nucleos-core <name>`. (builds/gates completos sao do "
    "orquestrador, uma vez no fim; corre so os teus testes com filtro: test_one)"
)
DENY_CARGO = (
    "No subagent runs cargo (owner rule, 2026-10-05): not test, build, check, "
    "clippy, fmt, nor anything that calls it (gates.sh, select_tests.py --run, "
    "heavy.py). Stop and hand back the exact test_one command you would have "
    "run, e.g. `cargo test -p nucleos-core <names>`; the controller runs it. "
    "(nenhum subagente corre cargo: devolve o comando ao controlador)"
)
# Matched against the raw command, quotes included, so `sh -c 'cargo test'`,
# a full path to cargo.exe or an unparsable line cannot slip past. Lowercase
# only and bounded by word/dash, so `Cargo.toml`, `.cargo-target-*` and
# `CARGO_TARGET_DIR` do not trip it.
_CARGO_WORD = re.compile(r"(?<![\w-])(?:cargo(?:-clippy)?|rustfmt|rustc)(?:\.exe)?(?![\w-])")
_WARM = re.compile(r"heavy\.py\b.*\bwarm\b")
DENY_ISOLATION = (
    "Launching a subagent with isolation: worktree is not permitted here; "
    "work in the current checkout."
)

# Leading NAME=value assignments and bash's `time [-p]` keyword: kept in front of the broker
# call, so `time` still times the whole run, queue included.
_ENV_PREFIX = re.compile(
    r"""^(?:\s*(?:[A-Za-z_][A-Za-z0-9_]*=(?:"[^"]*"|'[^']*'|[^\s"'])*|time(?:\s+-p)?(?=\s)))*""")
_REDIRECT = re.compile(r"^(?:\d*|&)(?:>>?|<)")


_BARE = re.compile(r"^[A-Za-z0-9_.\/+:@~][A-Za-z0-9_.\/+:@~-]*$")


def _bare_interpreter(py):
    """Interpreter token usable as a literal, unquoted program name.

    Claude Code's worktree isolation refuses a quoted (computed) program name, so a
    path with spaces falls back to its 8.3 short form, else to bare python/python3.
    Never raises: the hook must fail open."""
    fallback = "python" if sys.platform == "win32" else "python3"
    try:
        p = py.replace("\\", "/")
        if _BARE.match(p):
            return p
        if sys.platform == "win32" and os.path.isfile(py):
            import ctypes
            buf = ctypes.create_unicode_buffer(32768)
            n = ctypes.windll.kernel32.GetShortPathNameW(py, buf, len(buf))
            if 0 < n < len(buf):
                short = buf.value.replace("\\", "/")
                if _BARE.match(short):
                    return short
    except Exception:
        pass
    return fallback


def _load_classify():
    name = "heavy_classify"
    if name in sys.modules:
        return sys.modules[name]
    spec = importlib.util.spec_from_file_location(name, HERE / "heavy_classify.py")
    mod = importlib.util.module_from_spec(spec)
    sys.modules[name] = mod
    spec.loader.exec_module(mod)
    return mod


def split_segments(text, posix=True):
    """Spans (start, end) of the segments of `text`, cut at `&&`, `||`, `;`, `|` and
    newlines outside quotes. Raises ValueError on an unterminated quote."""
    spans = []
    start = 0
    i = 0
    n = len(text)
    quote = None
    while i < n:
        c = text[i]
        if quote:
            if posix and quote == '"' and c == "\\":
                i += 2
                continue
            if c == quote:
                quote = None
            i += 1
            continue
        if c in "'\"":
            quote = c
            i += 1
            continue
        if posix and c == "\\":
            i += 2
            continue
        if not posix and c == "`":
            i += 2
            continue
        two = text[i:i + 2]
        if two in ("&&", "||"):
            spans.append((start, i))
            i += 2
            start = i
            continue
        if c in ";|\n":
            spans.append((start, i))
            i += 1
            start = i
            continue
        i += 1
    if quote:
        raise ValueError("unterminated quote")
    spans.append((start, n))
    return spans


def _tokens(segment, posix):
    if posix:
        toks = shlex.split(segment, posix=True)
    else:
        toks = [t[1:-1] if len(t) >= 2 and t[0] == t[-1] and t[0] in "'\"" else t
                for t in shlex.split(segment, posix=False)]
    out = []
    skip = False
    for t in toks:
        if skip:
            skip = False
            continue
        if _REDIRECT.match(t):
            # A bare operator (`>`, `2>`) takes the next token as its target.
            if re.fullmatch(r"(?:\d*|&)(?:>>?|<)", t):
                skip = True
            continue
        out.append(t)
    return out


def _wait_max(tool_input):
    t = tool_input.get("timeout")
    if isinstance(t, bool) or not isinstance(t, (int, float)):
        return 90
    return max(30, int(t / 1000) - 30)


def _deny(reason):
    return {"hookSpecificOutput": {
        "hookEventName": "PreToolUse",
        "permissionDecision": "deny",
        "permissionDecisionReason": reason,
    }}


def decide(payload, main, broker_ok, py):
    """The hook output for `payload`, or None for 'say nothing'."""
    if not isinstance(payload, dict):
        return None
    tool = payload.get("tool_name")
    tool_input = payload.get("tool_input")
    if not isinstance(tool_input, dict):
        return None
    subagent = bool(payload.get("agent_id"))

    if tool in ("Agent", "Task"):
        if tool_input.get("isolation") == "worktree":
            return _deny(DENY_ISOLATION)
        return None
    if tool not in ("Bash", "PowerShell"):
        return None
    command = tool_input.get("command")
    if not isinstance(command, str) or not command.strip():
        return None
    if subagent and (_CARGO_WORD.search(command) or _WARM.search(command)):
        return _deny(DENY_CARGO)

    posix = tool == "Bash"
    hc = _load_classify()
    try:
        spans = split_segments(command, posix)
    except ValueError:
        return None

    judged = []
    for start, end in spans:
        seg = command[start:end]
        try:
            toks = _tokens(seg, posix)
        except ValueError:
            return None  # unparsable quoting: fail open, say nothing
        if toks and toks[0] == "export" and len(toks) > 1 and toks[1].startswith("PATH="):
            return None
        judged.append((start, end, hc.classify(toks) if toks else hc.Verdict()))

    rewrites = []
    for start, end, v in judged:
        if subagent:
            if v.gate or (v.heavy and not v.filtered):
                return _deny(DENY_REASON)
            if v.heavy and not v.wrapped:
                rewrites.append((start, end, 2))
        else:
            if v.heavy and not v.gate and not v.wrapped:
                rewrites.append((start, end, 1))

    if not rewrites or not broker_ok or main is None:
        return None

    agent = str(payload.get("agent_id")) if subagent else "main"
    broker = f'{_bare_interpreter(py)} "{Path(main).as_posix()}/scripts/heavy.py"'
    parts = []
    pos = 0
    for start, end, prio in rewrites:
        seg = command[start:end]
        body = seg.lstrip()
        lead = seg[:len(seg) - len(body)]
        stripped = body.rstrip()
        trail = body[len(stripped):]
        env = _ENV_PREFIX.match(stripped).group(0).strip()
        rest = stripped[_ENV_PREFIX.match(stripped).end():].strip()
        envelope = f"{env} " if env else ""
        opts = f"--prio {prio} --agent {agent}"
        if subagent:
            opts += f" --wait-max {_wait_max(tool_input)}"
        call = f"{broker} {opts} -- {rest}"
        # No `& ` call operator: a bare program path runs in PowerShell, and the
        # operator would make the program name non-literal for worktree isolation.
        parts.append(command[pos:start] + lead + envelope + call + trail)
        pos = end
    parts.append(command[pos:])
    updated = dict(tool_input)
    updated["command"] = "".join(parts)
    return {"hookSpecificOutput": {
        "hookEventName": "PreToolUse",
        "updatedInput": updated,
    }}


def main():
    try:
        payload = json.loads(sys.stdin.read())
        if not isinstance(payload, dict):
            return 0
        hc = _load_classify()
        cwd = payload.get("cwd") or os.getcwd()
        main_dir = hc.resolve_main(cwd, os.environ)
        # Registered at user level, so it also sees other repositories: act only on a
        # repository whose main checkout carries this workflow.
        if not main_dir or ("NUCLEOS_HEAVY_MAIN" not in os.environ
                and not (Path(main_dir) / "scripts" / "heavy_classify.py").is_file()):
            return 0
        broker_ok = bool(main_dir) and (Path(main_dir) / "scripts" / "heavy.py").is_file()
        out = decide(payload, main_dir, broker_ok, sys.executable.replace("\\", "/"))
        if out is not None:
            sys.stdout.write(json.dumps(out))
    except Exception as exc:  # fail-open: a hook bug must never block a command
        print(f"heavy_guard: {exc}", file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main())
