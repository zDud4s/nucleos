#!/usr/bin/env python3
"""Production hook for NucleOS sessions — launched by the daemon or opened by hand.

Registered under seven events (`autopilot.rs`'s `wire_classifier_hook`), told apart by the
payload's own `hook_event_name` rather than by anything the invocation says, because the same
command is wired to all seven. `Stop`, `SubagentStop`, `UserPromptSubmit` and `SessionStart` (stdout JSON such as
`hookSpecificOutput.additionalContext` is printed verbatim) forward to the
hooks a pinned workflow bundle declares (`bundle_hooks`), and fail open; the last two also
carry the claims context of the session (`claims_context`); the three tool events
are what follows:

A run (NUCLEOS_RUN_ID present) gets the full safety gate on `PreToolUse`: allows emit the
recognized hookSpecificOutput approval contract, while denials and pending approvals emit the
empirically proven legacy block contract. Just before an allow, a shell command first waits
for the daemon's admission to run (`admit_heavy`), the one leg here that fails open.

An interactive session gets one narrow question instead: is this a git operation the
VCS queue performs? If so it is queued and this call is refused; otherwise this hook
says nothing at all. **It can refuse but it can never approve, with one exception**:
a call to one of the four `mcp__nucleos__task_*` tools is allowed, only to stamp the session id
into its input (see `interactive_session`); every other tool, MCP server or near-miss name gets
no decision at all. That asymmetry is what
makes it safe to speak here, where the old version had to stay silent. Registered
repo-wide, an allow would auto-approve a person's own tools; a refusal grants nothing.
This branch may also add context (`additionalContext`) telling the agent that a vcs request
it was refused for has settled, once per request; so does the claims context (`claims_exchange`:
a rate-limited, fail-open registration of the session with the daemon, 2 s in total with token
discovery included, and the notices it answers); that carries no permission decision either,
and it is silent on every error.

That silence was a real hole rather than a conservative default. The queue exists to
order git operations between sessions, and the sessions doing most of the work are the
ones a person opens in an editor. Measured: an editor session asked to `git merge master`
merged, with no hook, no proposal and no row.

`PostToolUse`/`PostToolUseFailure` report the OUTCOME of a call this hook already judged, and
never block anything: by the time either fires the tool has already run (or already failed), so
a refusal here would be a barrier with nothing left to stop — an expensive warning, not a
protection. `report_outcome` is therefore silent on every failure of its own: an unreachable
daemon, a missing token, a payload it cannot use. Never `deny()`, never `approve()`. They also
carry the claims notices, as context only.

All malformed-input, configuration, and daemon errors still fail closed ON THE PreToolUse PATH —
the one path where failing open would matter.
"""

import fnmatch
import json
import os
import re
import shlex
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request

# Statuses after which a vcs request will not change again.
SETTLED_STATUSES = (
    "succeeded", "failed", "blocked", "rejected", "cancelled", "interrupted", "escalated",
)
PENDING_DIR_ENV = "NUCLEOS_VCS_PENDING_DIR"
# An id still unsettled this long is dropped, and the file is not re-checked more often than
# the second figure, so a burst of tool calls costs one check rather than one per call.
PENDING_TTL_SECONDS = 30 * 60
PENDING_RECHECK_SECONDS = 15
SESSION_ID_SHAPE = re.compile(r"[A-Za-z0-9_-]{1,128}")

# Task claims (spec 2026-10-22). An interactive session tells the daemon it is alive so that the
# claims it holds stay alive, and hears back what happened to them. A run never does this: it is a
# holder by its own id, which the daemon reads off its key.
CLAIMS_DIR_ENV = "NUCLEOS_CLAIMS_DIR"
# Switched off by ANY non-empty value ("1", "0" and "off" alike); only unset or empty leaves it on.
CLAIMS_OFF_ENV = "NUCLEOS_CLAIMS_OFF"
# Never more than this IN TOTAL: token discovery (at most 1 s) and the HTTP call share it. The
# registration is its OWN request, apart from the git path (30 s, fails closed). A claim must never
# be able to slow a tool call or stop one.
CLAIMS_TIMEOUT_SECONDS = 2
# The token costs a git call and a process spawn when it is not in the environment, and this hook
# runs before and after every tool call. The lease is 30 minutes, so one registration every 20
# seconds keeps it far from lapsing and a tool call longer than that is caught by its PostToolUse.
CLAIMS_INTERVAL_SECONDS = 20
CLAIMS_ERROR_BACKOFF_SECONDS = 60
CLAIMS_UNREGISTERED_BACKOFF_SECONDS = 300

# A deliberately over-broad local filter, and it must never be the authority. Its only job is
# to keep the fast path free: this hook runs in front of every tool call in every editor
# session, and asking the daemon each time would spend git subprocesses on keystrokes. What
# passes here is asked properly; what does not was never a queue operation under any spelling.
#
# `branch` is in the list for `branch -d` and lets `branch --list` through to the daemon, which
# declines it. That is the right way round: over-including costs one HTTP call, under-including
# costs the guarantee. `pull` is a fetch plus a merge into the checked-out branch, and the daemon
# refuses a direct one; a filter that never asked it let every editor session pull unqueued.
QUEUE_SUBCOMMANDS = ("merge", "push", "tag", "fetch", "branch", "rebase", "pull")

# Where one command word can end and another begin, in sh or in PowerShell. Normalized to spaces
# before the scan below, so `cd x&&git push` splits with no spaces in it anywhere.
SEGMENT_SEPARATORS = "\n\r;|&()`{}"

# Global flags that swallow the token after them. Everything else starting with `-` is either a
# valueless flag or a `--flag=value`, and skipping one token is right for both.
GIT_FLAGS_WITH_VALUES = ("-C", "-c", "--git-dir", "--work-tree", "--namespace", "--exec-path")

DEFAULT_DAEMON_URL = "http://127.0.0.1:8791"

# One admission poll waits this long for the daemon's answer. The daemon holds a poll for at
# most 25 seconds, so this is what decides a poll that went quiet, not the daemon.
ADMIT_POLL_TIMEOUT_SECONDS = 40
# The total time `admit_heavy` may spend waiting. Kept below `autopilot::HOOK_TIMEOUT_SECONDS`
# (3600, the timeout written onto the tool-event hook entries), so the hook returns and lets the
# command run before the CLI would kill it.
ADMIT_HOOK_BUDGET_SECONDS = 3300


def governed_git_verb(command: str) -> str:
    """The queue verb this command would run, or `""`.

    Scans the WHOLE command, not its first two words. The first version of this filter read
    `tokens[0]` and `tokens[1]` and so governed a command only when it literally began `git
    <verb>` — which `cd repo\\ngit push`, `true && git push`, and any leading `echo` walked
    straight past into silence. That is the one direction this filter must not fail in, and it
    failed in it: the file's own comment above says under-including costs the guarantee, while
    the code under it under-included.

    Deliberately over-broad, as that comment asks. `echo "run git push later"` matches and is
    refused, and a quoted mention is the price of not needing a shell parser to be sure. The cost
    of a false positive is one refusal a person can reword; the cost of a false negative is the
    queue's whole promise, silently.
    """
    # The words as the shell would hand them to git, not as they were typed. bash drops the
    # backslash of `pu\sh` and PowerShell the backtick of `pu`sh`; both run `push`, and the raw
    # text matched neither. Every reading is scanned and any one governing the command is enough:
    # stripping an escape character also mangles a Windows path or a bash command substitution,
    # which the untouched reading still sees.
    unescaped_bash = re.sub(r"\\(.)", r"\1", command, flags=re.DOTALL)
    for variant in (command, command.replace("`", ""), unescaped_bash,
                    unescaped_bash.replace("`", "")):
        verb = _verb_in_command(variant)
        if verb:
            return verb
    return ""


# The project owner's override of the task-claims land gate (`nucleos-core --land` reads it). It is
# for a person at their own terminal, like NUCLEOS_ALLOW_DIRECT_GIT, but unlike that one an agent
# must not reach it at all: a claimed task is only closed by its owner or by green.
CLAIMED_LAND_OVERRIDE = "nucleos_allow_claimed_land"


# The forms that SET a variable, on text already stripped and lower-cased: `NAME=`, `NAME =`,
# `env:NAME` (also `$env:NAME` and `Set-Item env:NAME`), `export NAME`, `env NAME`, `set NAME`,
# `-e NAME`. A bare mention (`rg NAME`, `git commit -m "... NAME ..."`, a doc) is not one.
_CLAIMED_LAND_SETTING = re.compile(
    r"(?<![a-z0-9_])(?:(?:env:|export\s+|env\s+|set\s+|-e\s+)" + CLAIMED_LAND_OVERRIDE
    + r"|" + CLAIMED_LAND_OVERRIDE + r"\s*=)"
)


def claimed_land_override(payload: dict) -> bool:
    """Whether this tool call SETS the owner's land override, however it is spelled.

    A speed bump, not the guard: a shell has endless ways to build a name (`n=NUCLEOS_ALLOW_CLAIMED;
    export ${n}_LAND=1`) that no text filter catches. The real refusal lives in `nucleos-core
    --land`, which is what honours or ignores the variable. This only stops the plain attempts.

    Over-broad like `governed_git_verb` on spelling: quotes, backslashes, backticks and `+ $ { }`
    are dropped before looking and the match ignores case, so `NUCLEOS_ALLOW_"CLAIMED_LAND"=1` and
    `$env:nucleos_allow_claimed_land` are both caught. Narrow on form: only a setting is refused
    (see `_CLAIMED_LAND_SETTING`), so searching for the name or writing it in a message is not.
    """
    if payload.get("tool_name") not in ("Bash", "PowerShell"):
        return False
    command = (payload.get("tool_input") or {}).get("command")
    if not isinstance(command, str):
        return False
    stripped = re.sub(r"[\\`'\"+${}]", "", command).lower()
    return _CLAIMED_LAND_SETTING.search(stripped) is not None


def _verb_in_command(command: str) -> str:
    """One reading of `governed_git_verb`: separators, then quote-aware and plain tokens."""
    normalized = command
    for separator in SEGMENT_SEPARATORS:
        normalized = normalized.replace(separator, " ")
    # Backslashes become slashes first: posix shlex would eat them, and the only thing read out of
    # a path here is its last component.
    flattened = normalized.replace("\\", "/")
    # Two readings, and either one governing the command is enough. The quote-aware one keeps
    # `-C "C:/My Repo"` as ONE token; plain whitespace splitting cut it in two and read the second
    # half as the verb, so the push walked past the queue. But shlex also swallows a quoted
    # `sh -c "<command>"` into a single word, which the plain reading still sees. Over-broad on
    # purpose.
    readings = [[token.strip("\"'") for token in flattened.split()]]
    try:
        readings.append(shlex.split(flattened, posix=True))
    except ValueError:
        pass  # an unbalanced quote: the plain reading above is all there is
    for tokens in readings:
        verb = _verb_in(tokens)
        if verb:
            return verb
    return ""


def _verb_in(tokens) -> str:
    """The first queue verb a git call in `tokens` would run, or `""`."""
    for index, token in enumerate(tokens):
        # `git`, `/usr/bin/git`, `C:\Program Files\Git\bin\git.exe` — the name it was invoked by
        # says nothing about what it does.
        name = token.replace("\\", "/").rsplit("/", 1)[-1].lower()
        if name not in ("git", "git.exe"):
            continue
        rest = index + 1
        while rest < len(tokens):
            candidate = tokens[rest]
            if candidate in GIT_FLAGS_WITH_VALUES:
                rest += 2
                continue
            if candidate.startswith("-"):
                rest += 1
                continue
            if candidate.lower() in QUEUE_SUBCOMMANDS:
                return candidate.lower()
            # A git call that is not a queue operation. Keep looking: a command may hold more
            # than one, and the second is as governed as the first.
            break
    return ""


def approve(reason: str = "autopilot: allowed", updated_input=None, context=None) -> None:
    """Allows the tool call, optionally rewriting its input.

    A rewrite (`updatedInput`) rides only on an allow, and a malformed one (anything but
    an object) is dropped so the command runs unfiltered (fail-open).
    """
    output = {
        "hookEventName": "PreToolUse",
        "permissionDecision": "allow",
        "permissionDecisionReason": reason,
    }
    if isinstance(updated_input, dict):
        output["updatedInput"] = updated_input
    if isinstance(context, str) and context:
        output["additionalContext"] = context
    print(json.dumps({"hookSpecificOutput": output}))
    sys.exit(0)


def no_opinion() -> None:
    sys.exit(0)


def deny(reason: str) -> None:
    # A block is a deliberate hook decision, not a script crash, so exit zero.
    print(json.dumps({"decision": "block", "reason": reason}))
    sys.exit(0)


def configured_target_dir(path):
    """`build.target-dir` out of one cargo config file, or None if it says nothing.

    Parsed rather than grepped. The config this exists for explains the setting in a
    comment that itself contains `CARGO_TARGET_DIR = "..."`, so a regex over the file
    finds the documentation before it finds the setting. Every failure -- no file, no
    `tomllib`, malformed TOML -- is None: this only ever ADDS places to look, so a
    reader that gives up leaves the old behaviour exactly as it was.
    """
    try:
        import tomllib

        with open(path, "rb") as handle:
            configured = tomllib.load(handle).get("build", {}).get("target-dir")
        return configured if isinstance(configured, str) and configured else None
    except Exception:
        return None


def cargo_target_dirs(main_root, environ=None, configs=None):
    """Every directory cargo might have built into, in cargo's own precedence order.

    `CARGO_TARGET_DIR` beats `build.target-dir` in a config file, which beats
    `<root>/target`. A LIST and not one resolved answer, because this is looking for a
    file that exists rather than reproducing cargo's decision: a binary left in an older
    location still prints the token, and this must not refuse because it went looking in
    only the newest place.
    """
    environ = os.environ if environ is None else environ
    if configs is None:
        cargo_home = environ.get("CARGO_HOME") or os.path.expanduser("~/.cargo")
        configs = [
            os.path.join(cargo_home, "config.toml"),
            os.path.join(main_root, ".cargo", "config.toml"),
        ]

    roots = []
    from_env = environ.get("CARGO_TARGET_DIR")
    if from_env:
        roots.append(from_env)
    for config in configs:
        configured = configured_target_dir(config)
        if configured:
            roots.append(configured)
    roots.append(os.path.join(main_root, "target"))
    return roots


def daemon_binary_candidates(roots, os_name=None):
    """Every path the daemon binary might have, in the order `control_token` tries them.

    cargo names the binary `nucleos-core.exe` on Windows and `nucleos-core` everywhere else.
    Looking only for the `.exe` failed CLOSED off Windows: no candidate ever existed, so every
    queue operation an agent asked for was refused for a token that could not be read.

    The platform's own name comes first. On Windows that keeps the old list, in the old order, as
    the exact head of this one, so a machine that found its binary before finds the same one now,
    and the extension-less names are tried only after every `.exe` has missed. Off Windows the
    `.exe` names come last; where no such file exists they cost one `os.path.exists` each.
    """
    os_name = os.name if os_name is None else os_name
    if os_name == "nt":
        names = ("nucleos-core.exe", "nucleos-core")
    else:
        names = ("nucleos-core", "nucleos-core.exe")
    return [
        os.path.join(root, build, name)
        for name in names
        for root in roots
        for build in ("debug", "release")
    ]


def run_dir_candidates(main_root, os_name=None):
    """The daemon binary as `scripts/run-daemon.ps1` stages it: `<dev home>/run/`, where the dev
    home is `NUCLEOS_DEV_HOME`, else `<system drive>/nucleos-dev` on Windows.

    The daemon runs from that copy, not from a cargo target dir, so a machine that only ever
    starts it through the script may have no `nucleos-core` under any target dir at all -- and
    the guard refused every queue operation, `--land` included, for a token it never looked for
    where it was. This is the script's default `-RunDir`; a run dir moved elsewhere is not found.
    `<parent of root>/.nucleos-run/`, the default before 2026-10-19, is still looked at last, for
    a daemon staged there that has not been restaged since.
    """
    os_name = os.name if os_name is None else os_name
    names = ("nucleos-core.exe", "nucleos-core") if os_name == "nt" else (
        "nucleos-core", "nucleos-core.exe")
    home = os.environ.get("NUCLEOS_DEV_HOME") or (
        (os.environ.get("SystemDrive") or "C:") + "\\nucleos-dev" if os_name == "nt" else None)
    run_dirs = ([os.path.join(home, "run")] if home else []) + [
        os.path.join(os.path.dirname(main_root), ".nucleos-run")]
    return [os.path.join(run_dir, name) for run_dir in run_dirs for name in names]


def daemon_binaries(main_root):
    """Every path the daemon binary might have: the staged run copy first, then cargo's builds.

    The run copy goes first because it is the binary actually running; any of them prints the
    same token, since it is read from the credential store, not from the binary.
    """
    return run_dir_candidates(main_root) + daemon_binary_candidates(cargo_target_dirs(main_root))


def control_token(cwd: str) -> str:
    """The daemon's own token, read the way the desktop app reads it.

    An editor session inherits no NucleOS environment, so the token has to be fetched
    rather than found. It lives in the system credential store under the person's own account,
    which is exactly who is sitting here — this grants nothing the session did not
    already have, it only stops the session having to be told how.

    The binary is located from the repository rather than from PATH: a linked worktree
    has no `target/` of its own, but `--git-common-dir` names the main checkout from
    inside any of them.

    Where cargo PUT it is a second question, and assuming `<main_root>/target` is how this
    guard spent a day inert. On a machine whose worktrees share one build tree outside the
    repository there is no `target/` at all, so every merge, push, tag and fetch from every
    agent session was refused for a missing token -- a guard failing closed on its own
    configuration, saying nothing about why. `cargo_target_dirs` mirrors cargo's own
    precedence instead of guessing at one location.
    """
    token, why = read_control_token(cwd)
    if token:
        return token
    if why == "repo":
        deny("could not locate the repository to find the daemon binary - failing closed")
    deny(
        "this is a git operation the queue performs, and the daemon token could not be "
        "read to queue it - failing closed. Run it from a terminal if you meant to act "
        "as yourself rather than through an agent."
    )


def read_control_token(cwd: str, git_timeout=10, print_timeout=20, total_timeout=None):
    """`(token, why)`: the token or None, and why none (`"repo"` or `"token"`).

    Raises nothing and never denies, so a caller that must stay silent (the follow-up that
    reports a settled request) can use it; `control_token` turns the failure into its denials.
    `total_timeout`, when given, is one budget for git and every `--print-token` together; each
    call gets the smaller of its own timeout and what is left of it.
    """
    from_env = os.environ.get("NUCLEOS_DAEMON_TOKEN")
    if from_env:
        return from_env, None

    deadline = None if total_timeout is None else time.monotonic() + total_timeout

    def left(own):
        if deadline is None:
            return own
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            raise subprocess.TimeoutExpired("token discovery", total_timeout)
        return min(own, remaining)

    try:
        common = subprocess.run(
            ["git", "-C", cwd, "rev-parse", "--path-format=absolute", "--git-common-dir"],
            capture_output=True, text=True, timeout=left(git_timeout),
        )
        if common.returncode != 0:
            return None, "repo"
        main_root = os.path.dirname(common.stdout.strip())

        for binary in daemon_binaries(main_root):
            if os.path.exists(binary):
                printed = subprocess.run(
                    [binary, "--print-token"], capture_output=True, text=True,
                    timeout=left(print_timeout),
                )
                if printed.returncode == 0 and printed.stdout.strip():
                    return printed.stdout.strip(), None
    except Exception:
        return None, "token"
    return None, "token"


def pending_path(session_id):
    """Where this session's unsettled vcs request ids are kept, or None for an unsafe id."""
    try:
        if not isinstance(session_id, str) or not SESSION_ID_SHAPE.fullmatch(session_id):
            return None
        base = os.environ.get(PENDING_DIR_ENV) or os.path.join(
            tempfile.gettempdir(), "nucleos-vcs-pending"
        )
        return os.path.join(base, f"{session_id}.json")
    except Exception:
        return None


def _load_pending(path):
    """Reads the pending file as `(entries, checked)`.

    `entries` is a list of `{"id": N, "since": epoch}`; `checked` is when the file was last
    checked against the daemon (0 when never). The first version of the file was a plain list
    of ints, and a session may still hold one: those ids count as remembered when the file was
    last written.
    """
    with open(path, "r", encoding="utf-8") as handle:
        loaded = json.load(handle)
    try:
        fallback = os.path.getmtime(path)
    except Exception:
        fallback = time.time()
    checked = 0
    items = loaded
    if isinstance(loaded, dict):
        raw_checked = loaded.get("checked")
        if isinstance(raw_checked, (int, float)) and not isinstance(raw_checked, bool):
            checked = raw_checked
        items = loaded.get("ids")
    if not isinstance(items, list):
        return [], checked
    entries = []
    for item in items:
        if isinstance(item, bool):
            continue
        if isinstance(item, int):
            entries.append({"id": item, "since": fallback})
        elif isinstance(item, dict):
            rid, since = item.get("id"), item.get("since")
            if isinstance(rid, int) and not isinstance(rid, bool):
                if not isinstance(since, (int, float)) or isinstance(since, bool):
                    since = fallback
                entries.append({"id": rid, "since": since})
    return entries, checked


def _store_pending(path, entries, checked=0):
    os.makedirs(os.path.dirname(path), exist_ok=True)
    tmp = f"{path}.{os.getpid()}.tmp"
    with open(tmp, "w", encoding="utf-8") as handle:
        json.dump({"checked": checked, "ids": entries}, handle)
    os.replace(tmp, path)


def remember_pending(session_id, request_id):
    """Adds one request id to the session's pending list. Silent on every failure."""
    try:
        path = pending_path(session_id)
        if path is None or not isinstance(request_id, int) or isinstance(request_id, bool):
            return
        try:
            entries, checked = _load_pending(path)
        except Exception:
            entries, checked = [], 0
        if request_id not in [e["id"] for e in entries]:
            entries.append({"id": request_id, "since": time.time()})
        _store_pending(path, entries, checked)
    except Exception:
        pass


def notice_for(ticket):
    """One sentence for a settled ticket, mirroring the daemon's own wording."""
    try:
        rid = ticket.get("id")
        status = ticket.get("status")
        sha = ticket.get("result_sha") or "an unrecorded commit"
        reason = ticket.get("failure_reason") or "no reason recorded"
        head = f"vcs request #{rid} has settled: "
        if status == "succeeded":
            return (
                head + f"it succeeded and landed at {sha}. Re-read git status and git log "
                "before going on."
            )
        if status == "escalated":
            return (
                head + "the conflict went to a person. Do not resolve it by hand."
            )
        if status == "blocked":
            return (
                head + f"it was refused before it started ({reason}). Nothing changed. "
                "Clear the cause and run the command again."
            )
        if status in ("failed", "interrupted"):
            return (
                head + f"it {status} ({reason}). Check the repository state before "
                "going on."
            )
        if status in ("cancelled", "rejected"):
            return head + f"it was {status}. Nothing was performed."
        return head + f"{status}."
    except Exception:
        return ""


def settled_notice(payload, token=None):
    """Text telling the agent which pending vcs requests have settled, or `""`.

    Silent on every error and makes no network call when nothing is pending. A file checked
    less than `PENDING_RECHECK_SECONDS` ago is left alone, so a burst of tool calls costs one
    check; an id still unsettled after `PENDING_TTL_SECONDS` is dropped with one last notice.
    `token` is an already-read daemon token, so the governed path does not read it twice.
    """
    try:
        path = pending_path(payload.get("session_id"))
        if path is None or not os.path.exists(path):
            return ""
        entries, checked = _load_pending(path)
        if not entries:
            return ""
        now = time.time()
        if 0 <= now - checked < PENDING_RECHECK_SECONDS:
            return ""
        if not token:
            cwd = payload.get("cwd") or os.getcwd()
            token, _why = read_control_token(cwd, 2, 3)
        daemon_url = os.environ.get("NUCLEOS_DAEMON_URL") or DEFAULT_DAEMON_URL
        notices = []
        remaining = []
        for entry in entries:
            rid = entry["id"]
            keep = True
            if token:
                try:
                    request = urllib.request.Request(
                        f"{daemon_url}/vcs/requests/{rid}",
                        headers={"Authorization": f"Bearer {token}"},
                        method="GET",
                    )
                    with urllib.request.urlopen(request, timeout=2) as response:
                        ticket = json.loads(response.read())
                    if isinstance(ticket, dict) and ticket.get("status") in SETTLED_STATUSES:
                        text = notice_for(ticket)
                        if text:
                            notices.append(text)
                            keep = False
                except urllib.error.HTTPError as exc:
                    if exc.code == 404:
                        keep = False
                except Exception:
                    pass
            if keep and now - entry["since"] > PENDING_TTL_SECONDS:
                notices.append(
                    f"vcs request #{rid} has not settled after "
                    f"{PENDING_TTL_SECONDS // 60} minutes; it is still the queue's - do not "
                    "run it again."
                )
                keep = False
            if keep:
                remaining.append(entry)
        if remaining:
            _store_pending(path, remaining, now)
        else:
            os.remove(path)
        return "\n".join(notices)
    except Exception:
        return ""


def claims_off():
    return bool(os.environ.get(CLAIMS_OFF_ENV))


def claims_state_path(session_id):
    """Where this session's claims state is kept, or None for an unsafe id."""
    try:
        if not isinstance(session_id, str) or not SESSION_ID_SHAPE.fullmatch(session_id):
            return None
        base = os.environ.get(CLAIMS_DIR_ENV) or os.path.join(
            tempfile.gettempdir(), "nucleos-claims"
        )
        return os.path.join(base, f"{session_id}.json")
    except Exception:
        return None


def _claims_state(path):
    """The state file as a dict; `{}` when it is missing or unreadable."""
    try:
        with open(path, "r", encoding="utf-8") as handle:
            loaded = json.load(handle)
        return loaded if isinstance(loaded, dict) else {}
    except Exception:
        return {}


def _save_claims_state(path, state):
    """Writes the state through a temp file; silent on every error. Never holds the token."""
    try:
        # Like `_store_pending`: the directory does not exist the first time.
        os.makedirs(os.path.dirname(path), exist_ok=True)
        tmp = f"{path}.{os.getpid()}.tmp"
        with open(tmp, "w", encoding="utf-8") as handle:
            json.dump(state, handle)
        os.replace(tmp, path)
    except Exception:
        pass


def _number(value):
    if isinstance(value, (int, float)) and not isinstance(value, bool):
        return value
    return 0


def claims_exchange(payload, want_incidents=False, force=False, now=None):
    """Registers this session with the daemon; returns the daemon's answer or None. Silent on every error.

    None means "nothing to say": switched off, a governed run, an id or cwd it cannot use, the rate
    limit, a daemon that is down (retried after a minute), or a project that is not registered
    (answered, but asked again only after five). Nothing here can raise, deny or approve.

    One total budget of CLAIMS_TIMEOUT_SECONDS covers token discovery (at most 1 s of it) and the
    HTTP call. `force` skips the rate limit, but not the error backoff: only a previous exchange
    that succeeded lets it through, so a dead daemon is not retried on every forced call.
    """
    try:
        if claims_off() or os.environ.get("NUCLEOS_RUN_ID") is not None:
            return None
        if not isinstance(payload, dict):
            return None
        session_id = payload.get("session_id")
        cwd = payload.get("cwd")
        path = claims_state_path(session_id)
        if path is None or not isinstance(cwd, str) or not cwd.strip():
            return None
        now = time.time() if now is None else now
        state = _claims_state(path)
        if now < _number(state.get("next")) and not (force and state.get("ok") is True):
            return None

        started = time.monotonic()
        token, _why = read_control_token(cwd, 1, 1, total_timeout=1)
        if not token:
            state["next"] = now + CLAIMS_ERROR_BACKOFF_SECONDS
            state["ok"] = False
            _save_claims_state(path, state)
            return None
        daemon_url = os.environ.get("NUCLEOS_DAEMON_URL") or DEFAULT_DAEMON_URL
        request = urllib.request.Request(
            f"{daemon_url}/tasks/session",
            data=json.dumps(
                {"session_id": session_id, "cwd": cwd, "want_incidents": bool(want_incidents)}
            ).encode(),
            headers={
                "Authorization": f"Bearer {token}",
                "Content-Type": "application/json",
            },
            method="POST",
        )
        try:
            remaining = max(0.2, CLAIMS_TIMEOUT_SECONDS - (time.monotonic() - started))
            with urllib.request.urlopen(request, timeout=remaining) as response:
                answer = json.loads(response.read())
        except Exception:
            answer = None
        if not isinstance(answer, dict):
            state["next"] = now + CLAIMS_ERROR_BACKOFF_SECONDS
            state["ok"] = False
            _save_claims_state(path, state)
            return None
        state["ok"] = True
        if answer.get("registered") is not True:
            state["next"] = now + CLAIMS_UNREGISTERED_BACKOFF_SECONDS
        else:
            state["next"] = now + CLAIMS_INTERVAL_SECONDS
        _save_claims_state(path, state)
        return answer
    except Exception:
        return None


# Equals mcp_tools::TASK_TOOL_NAMES behind the server prefix; a test compares the two.
TASK_TOOLS = tuple(f"mcp__nucleos__task_{name}" for name in ("list", "claim", "release", "done"))
INCIDENTS_HEADER = (
    "Live incidents in this project. Before fixing anything outside your own task, claim it with "
    "mcp__nucleos__task_claim, and if you do not win, leave it:\n"
)


def claims_context(payload, want_incidents=False, force=False, always_list=False):
    """The text this session should read before its next step, or "". Silent on every error.

    Notices (a claim lost, an incident closed) come from the daemon already worded. Incidents are
    listed only when asked, and only when the list changed since this session last saw it (or
    `always_list`, after a compaction dropped it from the context).
    """
    try:
        answer = claims_exchange(payload, want_incidents=want_incidents, force=force)
        if not isinstance(answer, dict) or answer.get("registered") is not True:
            return ""
        notices = answer.get("notices")
        parts = [n for n in (notices if isinstance(notices, list) else []) if isinstance(n, str) and n.strip()]
        if want_incidents:
            raw = answer.get("incidents")
            incidents = [i for i in (raw if isinstance(raw, list) else []) if isinstance(i, str) and i.strip()]
            digest = "\n".join(incidents)
            path = claims_state_path(payload.get("session_id"))
            state = _claims_state(path) if path else {}
            if incidents and (always_list or digest != state.get("incidents")):
                parts.append(INCIDENTS_HEADER + "\n".join(f"- {line}" for line in incidents))
            if path and state.get("incidents") != digest:
                # Also stored when the list went empty, so that the same list coming back is news.
                state["incidents"] = digest
                _save_claims_state(path, state)
        return "\n\n".join(parts)
    except Exception:
        return ""


def tell(text: str, event: str = "PreToolUse") -> None:
    """Adds context to the next turn. Carries no permission decision, so it grants nothing."""
    print(
        json.dumps(
            {"hookSpecificOutput": {"hookEventName": event, "additionalContext": text}}
        )
    )
    sys.exit(0)


def read_stdin_once():
    """Reads and parses stdin exactly ONCE, returning `(payload, reason)`.

    stdin can be read only once, and `main` now has three branches that each need it — the
    outcome report, the interactive filter, and the governed-run gate — with three DIFFERENT
    reactions to a parse failure: always silent, silent only for a person's session, and `deny()`
    for a governed run. Reading here once and handing every branch the same pair lets each apply
    its OWN reaction instead of the reader picking one for all three.

    `reason` is `None` on success. On failure it is the exact message the pre-restructuring code
    used to `deny()` with, preserved so the run branch's `deny(reason)` reads byte-identical to
    what it printed before this function existed.
    """
    try:
        payload = json.load(sys.stdin)
    except Exception as exc:
        return None, f"failed to parse hook payload from stdin ({exc}) - failing closed"
    if not isinstance(payload, dict):
        return None, "hook payload from stdin is not a JSON object - failing closed"
    return payload, None


def report_outcome(payload: dict) -> None:
    """Reports what a tool call did, and never blocks anything.

    By the time `PostToolUse`/`PostToolUseFailure` fires the call has already run (or already
    failed) — there is nothing left here to approve or deny, only something to record. So this
    function is silent on EVERY failure of its own: an unreachable daemon, a missing token, a
    payload it cannot use. It never raises past its own boundary and never calls `deny()` or
    `approve()` — `main` returns straight after calling this, which leaves the process to exit 0
    with nothing printed, exactly like `no_opinion()` without the `sys.exit` neither of them needs
    here.

    `payload` is already a parsed dict — the caller determined the event from it, so by
    construction it always has `hook_event_name`. NUCLEOS_RUN_ID/NUCLEOS_DAEMON_URL/
    NUCLEOS_DAEMON_TOKEN are read fresh rather than trusted from the payload, exactly as the
    `PreToolUse` gate below reads them: the CLI's own payload never carries a run id at all, only
    the environment the daemon spawned this process with does.
    """
    try:
        run_id = int(os.environ.get("NUCLEOS_RUN_ID", ""))
        daemon_url = os.environ.get("NUCLEOS_DAEMON_URL")
        token = os.environ.get("NUCLEOS_DAEMON_TOKEN")
        if not daemon_url or not token:
            return

        body = json.dumps(
            {
                "run_id": run_id,
                "tool_name": payload.get("tool_name", ""),
                "tool_input": payload.get("tool_input", {}),
                "tool_response": payload.get("tool_response", {}),
                "event": payload.get("hook_event_name", ""),
                "tool_use_id": payload.get("tool_use_id"),
            }
        ).encode()
        request = urllib.request.Request(
            f"{daemon_url}/hooks/posttooluse",
            data=body,
            headers={
                "Authorization": f"Bearer {token}",
                "Content-Type": "application/json",
            },
            method="POST",
        )
        # Three seconds, not `PreToolUse`'s five: a slow outcome report must not delay the NEXT
        # turn, and unlike the gate above there is no verdict on the other end worth waiting for.
        with urllib.request.urlopen(request, timeout=3):
            pass
    except Exception:
        # Every failure is the same non-event: the ledger missed one entry. Nothing here is worth
        # acting on from a hook that cannot block anyway.
        pass


# The IDE verify hint (F2c-3). A recorded verify-box beat makes the daemon write this machine file
# (worktree -> project, gated tools with their allowed subcommands, gate entrypoints, expires_at);
# this hook only READS it, locally, and only ever adds context - it never refuses and never
# approves on this path. No daemon call and no token: the file already says whether the project
# has the switch on (a beat is recorded only then) and whether the box is live (expires_at).
HINTS_FILE_ENV = "NUCLEOS_VERIFY_HINTS_FILE"
HINTS_FILE = "ide-verify-hints.json"  # equals verify_box::HINTS_FILE
# Mirrors verify_guard::VALUE_FLAGS: global flags that swallow the next word.
HINT_VALUE_FLAGS = ("-C", "--manifest-path", "--config", "--color", "-Z", "--prefix", "--cwd")
VERIFY_HINT = (
    "this project verifies through the daemon: call mcp__nucleos__verify to run this "
    "worktree's build, test and gate commands in its queue and cache, and wait on it with "
    "mcp__nucleos__verify_status. Nothing was refused; this command runs as written."
)


def hints_path() -> str:
    override = os.environ.get(HINTS_FILE_ENV)
    if override:
        return override
    return os.path.join(os.path.expanduser("~"), ".nucleos", HINTS_FILE)


def _norm_path(path: str) -> str:
    plain = str(path).replace("\\", "/")
    if plain.startswith("//?/"):
        plain = plain[4:]
    plain = plain.rstrip("/")
    return plain.lower() if os.name == "nt" else plain


def _runs_verification(command: str, entry: dict) -> bool:
    """Whether `command` runs one of the entry's gated tools or gate entrypoints.

    Deliberately over-broad, like governed_git_verb: an over-match costs one sentence of context,
    never a decision. A gated tool with allowed subcommands needs a subcommand that is not one of
    them (a bare call or `--version` does not match); a tool with none matches any use.
    """
    tools = entry.get("tools")
    tools = {str(k).lower(): v for k, v in tools.items()} if isinstance(tools, dict) else {}
    entrypoints = entry.get("entrypoints")
    if not isinstance(entrypoints, list):
        entrypoints = []
    entrypoints = [e.replace("\\", "/") for e in entrypoints if isinstance(e, str)]
    segments = [command]
    for separator in SEGMENT_SEPARATORS:
        segments = [part for chunk in segments for part in chunk.split(separator)]
    for segment in segments:
        words = [w.strip("\"'") for w in segment.replace("\\", "/").split()]
        for index, word in enumerate(words):
            base = word.rsplit("/", 1)[-1].lower()
            if base.endswith(".exe"):
                base = base[:-4]
            if base in tools:
                allowed = tools[base] if isinstance(tools[base], list) else []
                if not allowed:
                    return True
                rest = iter(words[index + 1:])
                for follow in rest:
                    if follow.startswith("+"):
                        continue
                    if follow.startswith("-"):
                        if follow in HINT_VALUE_FLAGS:
                            next(rest, None)
                        continue
                    if follow not in allowed:
                        return True
                    break
            shown = word[2:] if word.startswith("./") else word
            for gate in entrypoints:
                if fnmatch.fnmatchcase(shown, gate) or shown == gate or shown.endswith("/" + gate):
                    return True
    return False


def verify_hint(payload: dict, now=None) -> str:
    """The hint text for a verification command in a worktree with a live box, else `""`.

    Total: any failure reading the file or its contents is `""`, so a broken hint can never
    cost a tool call.
    """
    try:
        command = (payload.get("tool_input") or {}).get("command")
        if payload.get("tool_name") not in ("Bash", "PowerShell") or not isinstance(command, str):
            return ""
        with open(hints_path(), encoding="utf-8") as handle:
            file = json.load(handle)
        worktrees = file["worktrees"]
        if not isinstance(worktrees, dict):
            return ""
        moment = time.time() if now is None else now
        cwd = _norm_path(payload.get("cwd") or os.getcwd())
        for key, entry in worktrees.items():
            if not isinstance(entry, dict):
                continue
            expires = entry.get("expires_at")
            if isinstance(expires, bool) or not isinstance(expires, (int, float)):
                continue
            if expires <= moment:
                continue
            root = _norm_path(key)
            if cwd != root and not cwd.startswith(root + "/"):
                continue
            if _runs_verification(command, entry):
                return VERIFY_HINT
        return ""
    except Exception:
        return ""


def interactive_session(payload: dict) -> None:
    """A session a person opened. Refuses queue operations; may add context. The one thing it ever
    approves is a call to one of the four `mcp__nucleos__task_*` tools, to stamp the session id.

    The non-git branch may also add the verify hint (context only, never a decision).
    """
    tool = payload.get("tool_name")
    session_id = payload.get("session_id")
    tool_input = payload.get("tool_input")
    stamping = (
        tool in TASK_TOOLS
        and isinstance(session_id, str)
        and SESSION_ID_SHAPE.fullmatch(session_id) is not None
        and isinstance(tool_input, dict)
    )
    # A task tool forces the registration: the daemon only believes a session id it has seen.
    claims = claims_context(payload, force=stamping)
    if stamping:
        # The ONE approval this hook ever gives, and the reason it is safe to give: the four task
        # tools only write claim rows in the daemon's own table (idempotent, no effect outside it).
        # The owner accepted this on 2026-10-22. The stamp overwrites whatever the model put in
        # `session_id`, so a session cannot speak for another. The residual trust is the server
        # NAME: `mcp__nucleos__` is whatever this session registered as `nucleos`, the same trust
        # `is_job_node_tool` places in it on the daemon side.
        approve(
            "claims: task tools only write claim rows",
            {**tool_input, "session_id": session_id},
            claims or None,
        )
    command = (payload.get("tool_input") or {}).get("command")
    if (
        payload.get("tool_name") not in ("Bash", "PowerShell")
        or not isinstance(command, str)
        or not governed_git_verb(command)
    ):
        notice = settled_notice(payload)
        hint = verify_hint(payload)
        text = "\n\n".join(t for t in (claims, notice, hint) if t)
        if text:
            tell(text)
        no_opinion()

    cwd = payload.get("cwd") or os.getcwd()
    daemon_url = os.environ.get("NUCLEOS_DAEMON_URL") or DEFAULT_DAEMON_URL
    body = json.dumps(
        {
            "tool_name": payload.get("tool_name", ""),
            "tool_input": payload.get("tool_input", {}),
            "cwd": cwd,
        }
    ).encode()
    token = control_token(cwd)
    request = urllib.request.Request(
        f"{daemon_url}/hooks/session-git-decision",
        data=body,
        headers={
            "Authorization": f"Bearer {token}",
            "Content-Type": "application/json",
        },
        method="POST",
    )
    try:
        with urllib.request.urlopen(request, timeout=30) as response:
            decision = json.loads(response.read())
    except Exception as exc:
        # Fails closed, and the wording matters: with the daemon down there is no queue to
        # order anything, so letting the command through is the one outcome that breaks the
        # guarantee silently.
        deny(f"daemon unreachable or errored ({exc}) - failing closed")

    if not isinstance(decision, dict):
        deny("daemon returned a non-object decision body - failing closed")
    request_id = decision.get("request_id")
    if (
        decision.get("settled") is False
        and isinstance(request_id, int)
        and not isinstance(request_id, bool)
    ):
        remember_pending(payload.get("session_id"), request_id)
    notice = "\n\n".join(t for t in (claims, settled_notice(payload, token)) if t)
    if decision.get("decision") == "allow":
        # Never re-emitted as an approval. The daemon means "not mine to govern", and the
        # session's ordinary permissions decide from here.
        if notice:
            tell(notice)
        no_opinion()
    reason = decision.get("reason", "this git operation belongs to the queue")
    deny(reason + ("\n\n" + notice if notice else ""))


# The events that are not about a tool call. Wired so a pinned workflow bundle can hang its own
# hooks on them (`core/src/workflows/hooks.rs`); this script only forwards them.
SESSION_EVENTS = ("Stop", "SubagentStop", "UserPromptSubmit", "SessionStart")

# Longer than the daemon's own fifty-second budget for running every hook of one event, so the
# daemon is what stops a slow hook and says so, rather than this giving up first.
BUNDLE_HOOKS_TIMEOUT = 55


def merged_stdout(outputs):
    """What to print for the hooks that produced output, in the order they ran.

    Claude Code reads a hook's stdout as ONE document: a JSON object when the whole of it parses
    as one, plain text otherwise. Two JSON objects printed one after the other parse as neither,
    so they cannot simply be concatenated. The rule: the first output that is a JSON object is
    printed alone (it is a decision or structured context, and a second one could only contradict
    it); when none is, the plain texts are joined, one per paragraph, as context.
    """
    for out in outputs:
        try:
            if isinstance(json.loads(out), dict):
                return out
        except ValueError:
            pass
    return "\n\n".join(out.strip() for out in outputs)


def with_context(outputs, event, extra):
    """`outputs` (bundle hook stdout) with `extra` added as context, still ONE document.

    A bundle that printed a JSON object keeps it, and `extra` goes into its
    `hookSpecificOutput.additionalContext` (Claude Code reads stdout as one document, so a second
    object beside it would parse as neither). Otherwise `extra` is one more plain-text paragraph.
    """
    if not extra:
        return outputs
    for index, out in enumerate(outputs):
        try:
            parsed = json.loads(out)
        except ValueError:
            continue
        if isinstance(parsed, dict):
            special = parsed.get("hookSpecificOutput")
            if not isinstance(special, dict):
                special = parsed["hookSpecificOutput"] = {}
            special.setdefault("hookEventName", event)
            current = special.get("additionalContext")
            special["additionalContext"] = f"{current}\n\n{extra}" if current else extra
            return outputs[:index] + [json.dumps(parsed)] + outputs[index + 1:]
    return outputs + [extra]


def bundle_hooks(payload: dict, extra: str = "") -> None:
    """Runs the hooks the project's pinned workflow bundles declare for this session event.

    The daemon runs them, from its library, and hands back what each did; this reproduces Claude
    Code's own reading of a hook. The first one that exited 2 blocks: its stderr is printed and
    this exits 2. Otherwise the stdout of those that printed something is printed (`merged_stdout`)
    and this exits 0. `extra` (the claims text of `main`) rides in that same document, and is
    printed even when there is no bundle, token or daemon behind the call.

    **Fails open, every way.** No token, no daemon, an error, an answer it cannot read, no project,
    no pins: exit 0 with nothing printed (but `extra`). These events are not a tool call this
    daemon governs; they are a workflow's own guards, and a guard that cannot be reached must not
    leave a session that can never stop, or a prompt that can never be sent.
    """
    results = []
    try:
        cwd = payload.get("cwd") or os.getcwd()
        token, _why = read_control_token(cwd, 2, 3)
        if token:
            daemon_url = os.environ.get("NUCLEOS_DAEMON_URL") or DEFAULT_DAEMON_URL
            request = urllib.request.Request(
                f"{daemon_url}/hooks/bundle",
                data=json.dumps(payload).encode(),
                headers={
                    "Authorization": f"Bearer {token}",
                    "Content-Type": "application/json",
                },
                method="POST",
            )
            with urllib.request.urlopen(request, timeout=BUNDLE_HOOKS_TIMEOUT) as response:
                answer = json.loads(response.read())
            found = answer.get("results") if isinstance(answer, dict) else None
            if isinstance(found, list):
                results = found
    except Exception:
        results = []

    outputs = []
    for result in results:
        if not isinstance(result, dict):
            continue
        if result.get("exit_code") == 2:
            sys.stderr.write(str(result.get("stderr") or ""))
            sys.exit(2)
        out = result.get("stdout")
        if isinstance(out, str) and out.strip():
            outputs.append(out)
    outputs = with_context(outputs, payload.get("hook_event_name"), extra)
    if outputs:
        print(merged_stdout(outputs))
    sys.exit(0)


def main() -> None:
    # Read exactly ONCE — stdin cannot be read twice — and keep the parse error rather than
    # acting on it: which of the three branches below owns this call is decided next, and each
    # reacts to a parse failure in its OWN way.
    payload, parse_error = read_stdin_once()

    # `hook_event_name` is the CLI's own field, not something either the interactive filter or
    # the `PreToolUse` gate below has ever needed to read — until now. This branch runs whatever
    # `NUCLEOS_RUN_ID` says, because a `PostToolUse` outcome fires for the SAME conversation a
    # run's `PreToolUse` calls fire in, and it never blocks: never `deny()`, never `approve()`.
    #
    # An outer JSON parse failure (`payload is None`) leaves `event` unreadable, so a call that
    # was genuinely `PostToolUse`/`PostToolUseFailure` but arrived unparseable falls through to
    # the branches below instead of landing here — see `read_stdin_once`'s docstring. In practice
    # this is unreachable: the CLI's own hook payloads are always well-formed JSON, and this
    # ambiguity only exists for input that should never occur at all.
    event = payload.get("hook_event_name") if payload is not None else None
    if event in ("PostToolUse", "PostToolUseFailure"):
        report_outcome(payload)
        if os.environ.get("NUCLEOS_RUN_ID") is None:
            text = claims_context(payload)
            if text:
                tell(text, event)
        return

    # Before the run branch, which would otherwise send a `Stop` to the tool gate: these events
    # carry no tool call, and they fail open whoever's session fired them.
    if event in SESSION_EVENTS:
        extra = ""
        if event in ("UserPromptSubmit", "SessionStart") and os.environ.get("NUCLEOS_RUN_ID") is None:
            # Forced: a prompt is rare next to tool calls, and a list that is a minute stale is not
            # worth the rate limit. `always_list` after a compaction, which drops the context.
            extra = claims_context(
                payload, want_incidents=True, force=True, always_list=(event == "SessionStart"),
            )
        bundle_hooks(payload, extra)

    # Refused for a person's session and for a governed run alike, before either is asked
    # anything: the override belongs to the project's owner, not to an agent.
    if payload is not None and claimed_land_override(payload):
        deny(
            "this variable is for the project's owner at their own terminal, not for an agent. "
            "A branch that touches a claimed task waits for the task to close; task_list shows "
            "who holds it."
        )

    run_id_raw = os.environ.get("NUCLEOS_RUN_ID")
    if run_id_raw is None:
        # Silent on a parse failure, exactly as `read_payload(silent_on_error=True)` used to be:
        # for a person's session, an unreadable payload means only that we cannot tell whether
        # this was git — and refusing every tool call in someone's editor, over a payload the
        # editor itself wrote, would take the session down to protect a guarantee never at risk.
        if payload is None:
            no_opinion()
        interactive_session(payload)

    # For a run, an unreadable payload means the gate cannot see what it is governing and must
    # stop everything — `deny()`, exactly as the un-restructured code did.
    if payload is None:
        deny(parse_error)

    daemon_url = os.environ.get("NUCLEOS_DAEMON_URL")
    token = os.environ.get("NUCLEOS_DAEMON_TOKEN")
    if not daemon_url or not token:
        deny(
            "NUCLEOS_DAEMON_URL/NUCLEOS_DAEMON_TOKEN not set for autopilot run "
            "- failing closed"
        )

    try:
        # run_id == runs.id; the core validates it against in-flight runs before acting.
        run_id = int(run_id_raw)
    except (TypeError, ValueError):
        deny("NUCLEOS_RUN_ID is not an integer - failing closed")

    body = json.dumps(
        {
            "run_id": run_id,
            "tool_name": payload.get("tool_name", ""),
            "tool_input": payload.get("tool_input", {}),
        }
    ).encode()
    request = urllib.request.Request(
        f"{daemon_url}/hooks/pretooluse-decision",
        data=body,
        headers={
            "Authorization": f"Bearer {token}",
            "Content-Type": "application/json",
        },
        method="POST",
    )
    try:
        with urllib.request.urlopen(request, timeout=5) as response:
            decision = json.loads(response.read())
    except Exception as exc:
        deny(f"daemon unreachable or errored ({exc}) - failing closed")

    if not isinstance(decision, dict):
        deny("daemon returned a non-object decision body - failing closed")

    rewrite = decision.get("updated_input")
    verdict = decision.get("decision")
    if verdict == "asking":
        decision = wait_for_answer(daemon_url, token, run_id)
        verdict = decision.get("decision")
    if verdict in ("deny", "pending_approval"):
        deny(decision.get("reason", ""))
    if verdict != "allow":
        deny(f"daemon returned an unrecognized decision {verdict!r} - failing closed")
    admit_heavy(daemon_url, token, run_id, payload)
    approve(decision.get("reason", "autopilot: allowed"), rewrite)


def wait_for_answer(daemon_url, token, run_id):
    """Waits for the person to answer for a tool call the daemon is holding.

    The gate above answers in five seconds because that is all the time a tool call
    can spare. A call somebody has to say yes to cannot be answered in five seconds,
    so the daemon says `asking` and the waiting happens here instead -- which keeps
    the ordinary call as fast as it was and confines the long timeout to the one
    case that earned it. A daemon that is simply down still fails at the first call,
    in five seconds, exactly as before.

    Fifty seconds against the daemon's own forty-five, so the daemon is what decides
    a question nobody answered rather than this timing out first and saying something
    less useful. Every failure here is a refusal: a tool call that cannot be allowed
    must never be allowed by default.
    """
    body = json.dumps({"run_id": run_id}).encode()
    request = urllib.request.Request(
        f"{daemon_url}/hooks/ask-wait",
        data=body,
        headers={
            "Authorization": f"Bearer {token}",
            "Content-Type": "application/json",
        },
        method="POST",
    )
    try:
        with urllib.request.urlopen(request, timeout=50) as response:
            answered = json.loads(response.read())
    except Exception as exc:
        deny(f"nobody could be asked about this ({exc}) - failing closed")
    if not isinstance(answered, dict):
        deny("daemon returned a non-object answer body - failing closed")
    return answered


def admit_heavy(daemon_url, token, run_id, payload) -> None:
    """Waits until the daemon admits a shell command to run, and returns when it may.

    The one fail-open leg on the PreToolUse path. The gate has already decided this call is
    allowed; admission only ORDERS heavy work between runs so two builds do not start together.
    An unreachable daemon, a bad body, an answer other than `waiting`, or the hook budget running
    out all mean the same thing: run the command. Failing closed here would turn a scheduling
    nicety into a refusal the gate never made. Never prints, never denies, never rewrites.
    """
    try:
        tool_name = payload.get("tool_name")
        tool_use_id = payload.get("tool_use_id")
        if tool_name not in ("Bash", "PowerShell"):
            return
        if not isinstance(tool_use_id, str) or not tool_use_id:
            return
        body = json.dumps(
            {
                "run_id": run_id,
                "tool_use_id": tool_use_id,
                "tool_name": tool_name,
                "tool_input": payload.get("tool_input", {}),
            }
        ).encode()
        deadline = time.monotonic() + ADMIT_HOOK_BUDGET_SECONDS
        while True:
            request = urllib.request.Request(
                f"{daemon_url}/hooks/admit",
                data=body,
                headers={
                    "Authorization": f"Bearer {token}",
                    "Content-Type": "application/json",
                },
                method="POST",
            )
            with urllib.request.urlopen(request, timeout=ADMIT_POLL_TIMEOUT_SECONDS) as response:
                answer = json.loads(response.read())
            if not isinstance(answer, dict) or answer.get("admission") != "waiting":
                return
            if time.monotonic() > deadline:
                return
    except Exception:
        return


if __name__ == "__main__":
    main()
