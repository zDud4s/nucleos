"""Argv classifier and main-checkout resolver shared by the heavy-command hook,
the broker (heavy.py) and select_tests.py.

Stdlib only, and it never raises on odd input: a classifier bug must fall open
(treat the command as not heavy), never block a command.
"""

import base64
import json
import os
import re
import shlex
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
    # Set when the heavy command was found inside something else (`bash -c`, a script, a
    # Makefile...): what it was found in. The hook then routes the whole wrapper.
    via: str | None = None
    # The cargo argv itself, plain or found inside a wrapper: what the broker leases a
    # target-dir pool slot for.
    cargo_argv: list[str] = field(default_factory=list)
    # How the hook must run the wrapper under the broker, which spawns a program: a script
    # through its interpreter, a shell builtin (`eval`, `iex`) as a child shell, whose word
    # is then dropped.
    run_with: str | None = None
    drop_word: bool = False


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


# ------------------------------------------------------------------ shell text
# Shared by the hook (heavy_guard) and by the wrappers below, which read a command out of
# `bash -c`, a script or a Makefile with the same rules the hook applies to a tool call.

# What may stand in front of the program a segment runs, kept in front of the broker call:
# NAME=value assignments, bash's `time [-p]` (so it still times the whole run, queue
# included), the reserved words that open a command list (`do cargo test` inside a loop
# ran outside the queue until 2026-10-08), and wrappers that exec their argument.
_LEAD_POSIX = re.compile(
    r"""^(?:\s*(?:[A-Za-z_][A-Za-z0-9_]*=(?:"[^"]*"|'[^']*'|[^\s"'])*"""
    r"""|time(?:\s+-p)?|do|then|else|elif|if|while|until|!|\{"""
    r"""|exec|command|builtin|nohup|env(?:\s+-i)?|nice(?:\s+-n\s*-?\d+|\s+-\d+)?"""
    r"""|timeout(?:\s+-[ks]\s+\S+|\s+--?[a-z][\w=-]*)*\s+\d+(?:\.\d+)?[smhd]?)(?=\s|$))*""")
# PowerShell's call and dot-source operators.
_LEAD_PS = re.compile(r"^(?:\s*[&.](?=\s))*")


def lead_end(body, posix):
    """Offset in `body` where the program of the segment starts."""
    return (_LEAD_POSIX if posix else _LEAD_PS).match(body).end()


_REDIRECT = re.compile(r"^(?:\d*|&)(?:>>?|<)")


def split_segments(text, posix=True):
    """Spans (start, end) of the segments of `text`, cut at `&&`, `||`, `;`, `|`,
    newlines, parentheses (subshells, `$(...)`, PowerShell's `(...)`) and outside
    quotes; bash also at a background `&`, PowerShell also at script-block braces.
    `${...}` is never cut. Raises ValueError on an unterminated quote."""
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
        if two == "${":
            depth = 0
            while i < n:
                if text[i] == "{":
                    depth += 1
                elif text[i] == "}":
                    depth -= 1
                    if depth == 0:
                        break
                i += 1
            i += 1
            continue
        if two in ("&&", "||"):
            spans.append((start, i))
            i += 2
            start = i
            continue
        # A background `&`, but not the one in `>&2`, `2>&1`, `<&0` or `&>file`.
        background = (posix and c == "&" and text[i - 1:i] not in ("<", ">")
                      and text[i + 1:i + 2] != ">")
        if c in ";|\n()" or background or (not posix and c in "{}"):
            spans.append((start, i))
            i += 1
            start = i
            continue
        i += 1
    if quote:
        raise ValueError("unterminated quote")
    spans.append((start, n))
    return spans


def shell_tokens(segment, posix=True):
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


# ------------------------------------------------------------------ wrappers
# A heavy command the shell does not show: inside `bash -c`, `eval`, `xargs`, `find -exec`,
# `powershell -Command`, `cmd /c`, a script, a Makefile or an npm script. Each of these ran
# outside the queue, into the shared default target dir, until 2026-10-08. Reading a file
# is best effort and bounded (depth, size); anything odd is judged light, never an error.

_MAX_DEPTH = 3
_MAX_READ = 1 << 20
_SHELLS = {"bash", "sh", "dash", "zsh"}
_POWERSHELLS = {"powershell", "pwsh"}
_SCRIPT_KINDS = {".sh": "bash", ".bash": "bash", ".ps1": "powershell", ".py": "python"}
_RUN_WITH = {"bash": "bash", "powershell": "powershell -NoProfile -File", "python": "python"}
# A Python script is heavy only through a list-form subprocess call: a message that tells
# the reader to run `cargo test` is not one.
_PY_SPAWN = re.compile(
    r"""\bsubprocess\.(?:run|call|check_call|check_output|Popen)\(\s*\[\s*"""
    r"""(['"])(cargo|go|npm|npx|tsc)(?:\.exe|\.cmd)?\1(?:\s*,\s*(['"])([\w-]+)\3)?""")
_SELF_BROKERED = re.compile(r"\bheavy\.py\b")
# A script that starts something long-lived (the daemon) must not run under the broker: the
# child would hold a token and a target slot for its whole life, and inherit the slot's
# CARGO_TARGET_DIR into every build it starts. It says so with this marker.
_SKIP = re.compile(r"heavy-broker:\s*skip")
_PS_BLOCK_COMMENT = re.compile(r"<#.*?#>", re.S)
# A script that ends by running its own arguments (`exec "$@"`, own-cargo-target.sh).
_PASS_THROUGH = re.compile(r'(?m)^\s*(?:exec\s+)?"\$@"')
# Wrapper text that picks its own target dir: queued, but no pool slot is leased for it.
_OWN_TARGET = re.compile(r"CARGO_(?:BUILD_)?TARGET_DIR\s*=|--target-dir\b")
_COMMENT_LINE = re.compile(r"(?m)^\s*#(?!!).*$")
_RECIPE_PREFIX = re.compile(r"(?m)^(\s*)[@+-]+")
_XARGS_VALUE_FLAGS = {
    "-I", "-L", "-n", "-P", "-s", "-d", "-E", "-a", "--max-args", "--max-procs",
    "--delimiter", "--arg-file", "--max-lines", "--max-chars", "--eof",
}
_PS_VALUE_FLAGS = {
    "-executionpolicy", "-ep", "-ex", "-windowstyle", "-w", "-outputformat", "-o",
    "-inputformat", "-if", "-configurationname", "-workingdirectory", "-wd", "-version",
    "-v", "-psconsolefile", "-settingsfile",
}


def combine(verdicts, via=None) -> Verdict:
    """One verdict for several commands: heavy as the heaviest unbrokered one among them."""
    heavy = [v for v in verdicts if v.heavy and not v.wrapped and not v.gate]
    gate = any(v.gate for v in verdicts)
    if not heavy:
        return Verdict(gate=gate, wrapped=not gate and any(v.wrapped for v in verdicts))
    top = max(heavy, key=lambda v: v.weight)
    cargo = next((v for v in heavy if v.kind == "cargo"), None)
    return Verdict(heavy=True, kind="cargo" if cargo else top.kind, weight=top.weight,
                   filtered=all(v.filtered for v in heavy), gate=gate, via=via,
                   cargo_argv=list(cargo.cargo_argv) if cargo else [])


def classify_text(text, posix=True, cwd=None, depth=0) -> Verdict:
    """Classify shell source: every command in it, `cd` followed."""
    try:
        spans = split_segments(text, posix)
    except ValueError:
        # An unbalanced quote somewhere (a comment, a heredoc): judge it line by line.
        if "\n" not in text:
            return Verdict()
        return combine([classify_text(line, posix, cwd, depth) for line in text.splitlines()])
    out = []
    here = cwd
    for start, end in spans:
        seg = text[start:end]
        try:
            toks = shell_tokens(seg[lead_end(seg, posix):], posix)
        except ValueError:
            continue
        if not toks:
            continue
        if toks[0].lower() in ("cd", "pushd", "set-location", "sl") and len(toks) == 2:
            here = str(Path(here or os.getcwd()) / toks[1])
            continue
        out.append(classify(toks, cwd=here, _depth=depth))
    return combine(out)


def _resolve(path, cwd):
    p = Path(path)
    if not p.is_absolute():
        p = Path(cwd or os.getcwd()) / p
    return p if p.is_file() else None


def _read(p):
    data = p.read_bytes()[:_MAX_READ]
    if b"\x00" in data:
        return None
    return data.decode("utf-8", "replace")


def _script_kind(p, text):
    kind = _SCRIPT_KINDS.get(p.suffix.lower())
    if kind:
        return kind
    first = text.split("\n", 1)[0]
    if first.startswith("#!"):
        if "python" in first:
            return "python"
        if "pwsh" in first or "powershell" in first:
            return "powershell"
    return "bash"


def _python_text(text) -> Verdict:
    out = []
    for m in _PY_SPAWN.finditer(text):
        out.append(classify([m.group(2)] + ([m.group(4)] if m.group(4) else [])))
    return combine(out)


def _script(path, cwd, depth, kind=None, args=()) -> Verdict:
    p = _resolve(path, cwd)
    if p is None or depth > _MAX_DEPTH:
        return Verdict()
    text = _read(p)
    if text is None or _SKIP.search(text):
        return Verdict()
    if _SELF_BROKERED.search(text):
        return Verdict(wrapped=True)  # it routes its own heavy commands
    kind = kind or _script_kind(p, text)
    if kind == "python":
        v = _python_text(text)
    elif kind == "powershell":
        v = classify_text(_COMMENT_LINE.sub("", _PS_BLOCK_COMMENT.sub("", text)), False, cwd, depth)
    else:
        v = classify_text(_COMMENT_LINE.sub("", text), True, cwd, depth)
        if _PASS_THROUGH.search(text):
            # Its arguments are a command, possibly after a few of its own (a root, a name).
            for k in range(min(3, len(args))):
                w = classify(list(args[k:]), cwd, depth)
                if w.heavy or w.gate or w.wrapped:
                    v = combine([v, w])
                    break
    return _via(v, "script", text)


def _via(v, via, text=None) -> Verdict:
    if v.heavy:
        v.via = via
        if text and _OWN_TARGET.search(text):
            v.cargo_argv = []
    return v


def _shell_args(args):
    """(text of -c, None) or (None, index of the script) or (None, None)."""
    i = 0
    while i < len(args):
        a = args[i]
        if a in ("-o", "+o", "-O", "+O", "--rcfile", "--init-file"):
            i += 2
        elif a.startswith("--"):
            i += 1
        elif a[:1] in ("-", "+") and len(a) > 1:
            if "c" in a[1:]:
                return (args[i + 1] if i + 1 < len(args) else None), None
            i += 1
        else:
            return None, i
    return None, None


def _powershell_args(prog, args):
    """(command text, None) or (None, index of the -File script) or (None, None)."""
    i = 0
    while i < len(args):
        low = args[i].lower()
        if low in ("-command", "-c", "-com", "-comm", "-comma", "-comman"):
            return " ".join(args[i + 1:]), None
        if low in ("-file", "-f"):
            return None, (i + 1 if i + 1 < len(args) else None)
        if low in ("-encodedcommand", "-e", "-ec", "-enc"):
            try:
                return base64.b64decode(args[i + 1]).decode("utf-16-le"), None
            except Exception:
                return None, None
        if low in _PS_VALUE_FLAGS:
            i += 2
        elif low.startswith("-"):
            i += 1
        elif prog == "powershell":
            return " ".join(args[i:]), None  # powershell.exe defaults to -Command
        else:
            return None, i  # pwsh defaults to -File
    return None, None


def _build_file(prog, args, cwd):
    """The Makefile/justfile `make`/`just` would read, and the directory it runs in."""
    just = prog == "just"
    here = Path(cwd or os.getcwd())
    named = None
    i = 0
    while i < len(args):
        a = args[i]
        name, eq, val = a.partition("=")
        if name in ("-C", "--directory", "--working-directory") or (just and name == "-d"):
            v = val if eq else (args[i + 1] if i + 1 < len(args) else "")
            here = here / v
            i += 1 if eq else 2
        elif name in ("-f", "--file", "--makefile", "--justfile"):
            named = val if eq else (args[i + 1] if i + 1 < len(args) else None)
            i += 1 if eq else 2
        else:
            i += 1
    if named:
        return (here / named if not Path(named).is_absolute() else Path(named)), here
    names = ("justfile", "Justfile", ".justfile") if just else ("GNUmakefile", "makefile", "Makefile")
    for n in names:
        if (here / n).is_file():
            return here / n, here
    return None, here


def _npm_script(args, cwd, depth) -> Verdict:
    i = _npm_sub(args)
    if i is None or args[i] not in ("run", "run-script") or i + 1 >= len(args):
        return Verdict()
    name = args[i + 1]
    pkg = Path(cwd or os.getcwd())
    for j, a in enumerate(args[:i]):
        if a in ("--prefix", "-C") and j + 1 < len(args):
            pkg = pkg / args[j + 1]
    try:
        scripts = json.loads((pkg / "package.json").read_text(encoding="utf-8")).get("scripts") or {}
    except Exception:
        return Verdict()
    body = [scripts.get(k) for k in (f"pre{name}", name, f"post{name}")]
    text = "\n".join(b for b in body if isinstance(b, str))
    return _via(classify_text(text, True, str(pkg), depth), "npm run") if text else Verdict()


def _wrapped(prog, args, rest, cwd, depth) -> Verdict | None:
    """The verdict of a wrapper around other commands, or None when `rest` is not one."""
    if depth > _MAX_DEPTH:
        return None
    d = depth + 1
    if prog in _SHELLS and args:
        text, i = _shell_args(args)
        if text is not None:
            return _via(classify_text(text, True, cwd, d), f"{prog} -c", text)
        if i is None:
            return None
        script = _base(args[i])
        if script == "gates.sh":
            return Verdict(gate=True)
        if script == "build-slot.sh":
            inner = list(args[i + 1:])
            v = classify(inner, cwd, d)
            v.gate = True
            v.inner = inner
            return v
        return _script(args[i], cwd, d, "bash", list(args[i + 1:]))
    if prog == "eval":
        v = _via(classify_text(" ".join(args), True, cwd, d), "eval", " ".join(args))
        v.run_with, v.drop_word = "bash -c", v.heavy
        return v
    if prog in ("iex", "invoke-expression"):
        text = " ".join(a for a in args if a.lower() != "-command")
        v = _via(classify_text(text, False, cwd, d), "iex")
        v.run_with, v.drop_word = "powershell -NoProfile -Command", v.heavy
        return v
    if prog in _POWERSHELLS:
        text, i = _powershell_args(prog, args)
        if text is not None:
            return _via(classify_text(text, False, cwd, d), prog, text)
        return _script(args[i], cwd, d, "powershell") if i is not None else None
    if prog == "cmd":
        j = next((k for k, a in enumerate(args) if a.lower() in ("/c", "/k")), None)
        if j is None:
            return None
        return _via(classify_text(" ".join(args[j + 1:]), False, cwd, d), "cmd")
    if prog == "xargs":
        i = 0
        while i < len(args) and args[i].startswith("-"):
            i += 2 if args[i] in _XARGS_VALUE_FLAGS else 1
        return _via(classify(args[i:], cwd, d), "xargs") if i < len(args) else None
    if prog == "find":
        found = []
        i = 0
        while i < len(args):
            if args[i] in ("-exec", "-execdir", "-ok", "-okdir"):
                j = i + 1
                while j < len(args) and args[j] not in (";", "+", "\\;"):
                    j += 1
                found.append(classify(args[i + 1:j], cwd, d))
                i = j
            i += 1
        return combine(found, "find") if found else None
    if prog in ("make", "gmake", "mingw32-make", "just"):
        f, here = _build_file(prog, args, cwd)
        text = _read(f) if f is not None and f.is_file() else None
        if not text:
            return None
        text = _RECIPE_PREFIX.sub(r"\1", _COMMENT_LINE.sub("", text))
        return _via(classify_text(text, True, str(here), d), prog)
    if _is_python(prog) and args:
        if args[0] == "-c" and len(args) > 1:
            return _via(_python_text(args[1]), "python")
        if not args[0].startswith("-"):
            return _script(args[0], cwd, d, "python")
        return None
    if prog == "npm":
        return _npm_script(args, cwd, d)
    # A script run by its path: judged as its interpreter would run it, and run through
    # that interpreter under the broker, which can only spawn a program.
    head = rest[0]
    if _base(head) in ("gates.sh", "build-slot.sh"):
        return classify(["bash"] + list(rest), cwd, depth)
    if Path(head).suffix.lower() in _SCRIPT_KINDS or (
            ("/" in head or "\\" in head) and not Path(head).suffix):
        p = _resolve(head, cwd)
        text = _read(p) if p is not None else None
        if text is None:
            return None
        kind = _script_kind(p, text)
        interp = {"bash": ["bash"], "python": ["python"],
                  "powershell": ["powershell", "-NoProfile", "-File"]}[kind]
        v = classify(interp + list(rest), cwd, depth)
        if v.heavy and v.via:
            v.run_with = _RUN_WITH[kind]
        return v if (v.heavy or v.gate or v.wrapped) else None
    return None


def classify(argv, cwd=None, _depth=0) -> Verdict:
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
                v = classify(inner, cwd, _depth)
                v.wrapped = True
                v.inner = inner
                return v
        v = _classify_plain(rest)
        if v.heavy:
            if v.kind == "cargo":
                v.cargo_argv = list(rest)
            return v
        w = _wrapped(prog, args, rest, cwd, _depth)
        return w if w is not None else v
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
