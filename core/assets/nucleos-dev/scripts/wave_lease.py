#!/usr/bin/env python3
"""The controller's side of a wave's slot lease.

A wave's workers are local processes, so they hold the daemon's slots exactly as its own
work does (invariant II). This asks the daemon for them, degrades to one worker whenever it
cannot have them, keeps the lease alive from a background renewer, and always gives the
wave back. The daemon's side is `core/src/wave.rs`.

Usage:
  python <bundle_dir>/scripts/wave_lease.py open --cwd . --units 3 --workers 2
  python <bundle_dir>/scripts/wave_lease.py status --wave-id 12   # exit 0: held; 3: open no more units
  python <bundle_dir>/scripts/wave_lease.py close --wave-id 12    # always exit 0

`open` always answers, and always with a worker count: degrading is not an error. The
renewer is `_renew`, which `open` starts detached and `close` stops -- a Claude Code session
runs no code between tool calls, so it cannot be a step of the skill.
"""
from __future__ import annotations

import argparse
import http.client
import importlib.util
import json
import os
import signal
import subprocess
import sys
import time
import urllib.error
import urllib.request
from dataclasses import asdict, dataclass, field
from datetime import datetime, timezone
from pathlib import Path

import yaml

import nucleos_paths

# `STATE_DIR` is None until a caller (a test) pins it; otherwise the lease files live in
# `<state_dir>/wave/leases/`, found through `.nucleos/workflow/project.json`.
STATE_DIR: Path | None = None
DEFAULT_DAEMON_URL = "http://127.0.0.1:8791"
# `core/src/wave.rs` MAX_WORKERS: the daemon answers 400 to a wave of more.
DAEMON_MAX_WORKERS = 4
TIMEOUT_SECONDS = 5
EXIT_HELD = 0
EXIT_STOP = 3


def models_path() -> Path:
    return nucleos_paths.state_dir() / "models.yaml"


def lease_dir() -> Path:
    return STATE_DIR if STATE_DIR is not None else nucleos_paths.state_dir() / "wave" / "leases"


class LimitsError(ValueError):
    """`parallel_execute` in `<state_dir>/models.yaml` is missing or not usable."""


@dataclass(frozen=True)
class Limits:
    max_workers: int
    max_units: int
    max_wave_seconds: int


def read_limits(path: Path | None = None) -> Limits:
    """The three ceilings this client obeys, from `parallel_execute` -- the first reader they have."""
    try:
        path = path or models_path()
        block = (yaml.safe_load(path.read_text(encoding="utf-8")) or {})["parallel_execute"]
        values = {key: block[key] for key in ("max_workers", "max_units", "max_wave_seconds")}
    except (OSError, yaml.YAMLError, KeyError, TypeError, nucleos_paths.ProjectNotFound) as error:
        raise LimitsError(f"parallel_execute is unreadable in {path}: {error!r}") from error
    for key, value in values.items():
        if not isinstance(value, int) or isinstance(value, bool) or value < 1:
            raise LimitsError(f"parallel_execute.{key} must be an integer >= 1, not {value!r}")
    return Limits(**values)


def requested_workers(units: int, decision_workers: int, limits: Limits) -> int:
    """How many slots to ask for: never more than the units, the speed decision's `workers`
    (spec §3.3 names this controller as its consumer), the config, or the daemon allows."""
    return max(1, min(units, decision_workers, limits.max_workers, DAEMON_MAX_WORKERS))


# Every way the daemon can fail to answer: refused, reset, timed out, or not HTTP at all.
NO_ANSWER = (OSError, http.client.HTTPException)

# The closed vocabulary of `degraded` reasons the `record` tool accepts, and the four this client
# can put in it. Named once and used everywhere, so a typo is a failing test and not a record the
# daemon refuses.
DEGRADED_VOCABULARY = (
    "task_not_decomposable", "lane_violation", "slots_exhausted", "no_capacity_authority",
    "wave_budget_exhausted", "effort_lever_unavailable", "team_serial", "wave_disabled",
)
NOT_DECOMPOSABLE = "task_not_decomposable"
NO_AUTHORITY = "no_capacity_authority"
EXHAUSTED = "slots_exhausted"
BUDGET = "wave_budget_exhausted"
REASONS = (NOT_DECOMPOSABLE, NO_AUTHORITY, EXHAUSTED, BUDGET)


@dataclass
class Opening:
    """What `open` tells the wave engine: how many workers to run, and why not more."""

    workers: int
    requested: int
    granted: int = 0
    wave_id: int | None = None
    lease_seconds: int | None = None
    degraded: list[str] = field(default_factory=list)
    reason: str | None = None


def daemon_url() -> str:
    return os.environ.get("NUCLEOS_DAEMON_URL") or DEFAULT_DAEMON_URL


def call(method: str, path: str, token: str, body: dict | None = None) -> tuple[int, bytes]:
    """`(status, body)` for any answer the daemon gave. Raises one of NO_ANSWER when there was none."""
    request = urllib.request.Request(
        daemon_url() + path,
        data=None if body is None else json.dumps(body).encode(),
        method=method,
        headers={"Authorization": f"Bearer {token}", "Content-Type": "application/json"},
    )
    try:
        with urllib.request.urlopen(request, timeout=TIMEOUT_SECONDS) as response:
            return response.status, response.read()
    except urllib.error.HTTPError as error:
        return error.code, error.read()


def request_wave(cwd: str, units: int, decision_workers: int, limits: Limits,
                 token: str | None) -> Opening:
    """The contract's table, row by row. Anything the daemon did not grant is one worker.

    Invariant II: a wave never assumes capacity nobody granted, so a status the table does not
    name (a 500, say) is a refusal and not a grant.
    """
    if units > limits.max_units:
        return Opening(1, 1, degraded=[NOT_DECOMPOSABLE],
                       reason=f"{units} units is more than max_units {limits.max_units}")
    requested = requested_workers(units, decision_workers, limits)
    if requested <= 1:
        # A wave of one is today's sequential run, which holds no slot; asking for one would not
        # be "normal" any more.
        return Opening(1, 1)
    if token is None:
        return Opening(1, requested, degraded=[NO_AUTHORITY], reason="no daemon token could be read")
    try:
        status, body = call("POST", "/concurrency/waves", token, {"cwd": cwd, "workers": requested})
    except NO_ANSWER as error:
        return Opening(1, requested, degraded=[NO_AUTHORITY],
                       reason=f"the daemon did not answer: {error!r}")
    if status != 200:
        return Opening(1, requested, degraded=[NO_AUTHORITY],
                       reason=f"the daemon refused the wave ({status}): "
                              f"{body.decode(errors='replace')[:200]}")
    try:
        answer = json.loads(body)
        granted, wave_id = int(answer["granted"]), answer["wave_id"]
        lease_seconds = int(answer["lease_seconds"])
    except (ValueError, KeyError, TypeError) as error:
        # Whatever it did grant lapses on its own within LEASE_SECONDS: nothing renews it.
        return Opening(1, requested, degraded=[NO_AUTHORITY],
                       reason=f"the daemon's grant is unreadable: {error!r}")
    if wave_id is None or granted < 1:
        return Opening(1, requested, degraded=[EXHAUSTED], reason=answer.get("reason"))
    return Opening(
        workers=min(units, decision_workers, limits.max_workers, granted),
        requested=requested,
        granted=granted,
        wave_id=int(wave_id),
        lease_seconds=lease_seconds,
        degraded=[EXHAUSTED] if granted < requested else [],
        reason=answer.get("reason"),
    )


def renew(wave_id: int, token: str) -> str:
    """`renewed`, `lapsed` or `no_answer`.

    `lapsed` is 410 and every other refusal: 404 (the daemon holds nothing for the wave), and a
    401/403 (the token no longer reaches the route, so the lease cannot be kept). A 5xx or no
    answer is silence -- the caller decides when silence has lasted a lease.
    """
    try:
        status, _ = call("POST", f"/concurrency/waves/{wave_id}/renew", token)
    except NO_ANSWER:
        return "no_answer"
    if status == 204:
        return "renewed"
    return "no_answer" if status >= 500 else "lapsed"


def release(wave_id: int, token: str) -> bool:
    """True when the daemon holds nothing for the wave any more (204, or 404). Never raises:
    a release that did not arrive is a lease that lapses by itself within LEASE_SECONDS."""
    try:
        status, _ = call("DELETE", f"/concurrency/waves/{wave_id}", token)
    except NO_ANSWER:
        return False
    return status in (204, 404)


def control_token(cwd: str) -> str | None:
    """The daemon's own token, read the way the project's installed hook reads it.

    The daemon writes `.claude/hooks/ask_daemon.py` into every rostered project; this loads
    that file from the main checkout and calls its `read_control_token(cwd)`, which returns
    `(token, why)` and never denies or exits -- this script's stdout is JSON and its exit code
    is the engine's signal, so the hook's denying `control_token` is not used. The
    `NUCLEOS_DAEMON_TOKEN` environment variable wins. A project without the hook has no daemon
    to ask; any failure at all answers None, never a crash.
    """
    from_env = os.environ.get("NUCLEOS_DAEMON_TOKEN")
    if from_env:
        return from_env
    try:
        common = subprocess.run(
            ["git", "-C", cwd, "rev-parse", "--path-format=absolute", "--git-common-dir"],
            capture_output=True, text=True, timeout=10,
        )
        if common.returncode != 0:
            return None
        main_root = os.path.dirname(common.stdout.strip())
        hook = Path(main_root) / ".claude" / "hooks" / "ask_daemon.py"
        if not hook.is_file():
            return None
        spec = importlib.util.spec_from_file_location("ask_daemon_paths", hook)
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        token, _why = module.read_control_token(cwd)
    except Exception:  # a hook that changed shape, or no git, is no token, never a crash
        return None
    return token or None


# What each ending of a wave adds to the GREEN record's `degraded` (REASONS is above).
DEGRADED_FOR = {"held": [], "closing": [], "closed": [], "lapsed": [EXHAUSTED],
                "renewer_gone": [EXHAUSTED], "deadline": [BUDGET]}


def stamp(at: float) -> str:
    return datetime.fromtimestamp(at, timezone.utc).isoformat(timespec="seconds").replace("+00:00", "Z")


def state_path(wave_id: int) -> Path:
    return lease_dir() / f"{wave_id}.json"


def read_state(path: Path) -> dict | None:
    try:
        return json.loads(path.read_text(encoding="utf-8"))
    except (OSError, ValueError):
        return None


def write_state(path: Path, state: dict) -> None:
    """Atomic, with two writers on one file: `open`/`close` and the renewer.

    Each writer has its own scratch name, so neither renames the other's half-written file.
    On Windows `os.replace` onto a file another process is reading fails with PermissionError
    for a moment, so it retries briefly before giving up.
    """
    path.parent.mkdir(parents=True, exist_ok=True)
    scratch = path.with_name(f"{path.stem}.{os.getpid()}.tmp")
    scratch.write_text(json.dumps(state), encoding="utf-8")
    for attempt in range(10):
        try:
            os.replace(scratch, path)
            return
        except PermissionError:
            time.sleep(0.05 * (attempt + 1))
    os.replace(scratch, path)


def update_state(path: Path, **changes) -> None:
    """Best effort: a record the renewer could not write never stops it renewing."""
    try:
        state = read_state(path)
        if state is not None:
            write_state(path, {**state, **changes})
    except OSError:
        pass


def pid_alive(pid: int | None) -> bool:
    """Whether a process is running -- without touching it.

    Never `os.kill(pid, 0)` on Windows: there signal 0 is CTRL_C_EVENT, sent with GenerateConsoleCtrlEvent: not a probe, and it can interrupt a whole console group.
    """
    if not pid:
        return False
    if os.name == "nt":
        import ctypes

        kernel32 = ctypes.windll.kernel32
        handle = kernel32.OpenProcess(0x1000, False, pid)  # PROCESS_QUERY_LIMITED_INFORMATION
        if not handle:
            return False
        try:
            code = ctypes.c_ulong()
            return bool(kernel32.GetExitCodeProcess(handle, ctypes.byref(code))) and code.value == 259
        finally:
            kernel32.CloseHandle(handle)
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return False
    except PermissionError:
        return True
    return True


def renew_loop(wave_id: int, token: str, lease_seconds: int, deadline: float, path: Path, *,
               now=time.time, sleep=time.sleep, renew_call=renew) -> str:
    """Keep the lease until the wave closes, lapses, or reaches its deadline; say which.

    The deadline is `max_wave_seconds` from the open: a controller that died without `close`
    leaves a renewer nobody stops, and this is what bounds the slots it holds.
    """
    interval = max(1, lease_seconds // 4)
    last_renewed = now()
    while True:
        sleep(interval)
        state = read_state(path)
        if state is None or state.get("status") != "held":
            return "closed"
        if now() >= deadline:
            # Nothing renews from here, so the slots go when the last renewal's lease runs out.
            update_state(path, status="deadline", unguarded_since=stamp(last_renewed + lease_seconds))
            return "deadline"
        answer = renew_call(wave_id, token)
        if answer == "renewed":
            last_renewed = now()
            update_state(path, renewed_at=stamp(last_renewed))
        elif answer == "lapsed":
            update_state(path, status="lapsed", unguarded_since=stamp(now()))
            return "lapsed"
        elif now() - last_renewed >= lease_seconds - interval:
            # The next try would land after the lease ran out, and the daemon's sweep takes the
            # slots at `last_renewed + lease_seconds` whatever this loop does next.
            update_state(path, status="lapsed",
                         unguarded_since=stamp(last_renewed + lease_seconds))
            return "lapsed"


def renewer_argv(wave_id: int, lease_seconds: int, deadline: float, path: Path) -> list[str]:
    # No token here: argv is what every process listing shows. It travels in the environment.
    return [sys.executable, str(Path(__file__).resolve()), "_renew", "--wave-id", str(wave_id),
            "--lease-seconds", str(lease_seconds), "--deadline", str(deadline),
            "--state-file", str(path)]


def spawn_renewer(wave_id: int, token: str, lease_seconds: int, deadline: float, path: Path) -> int:
    """Start the renewer detached from whoever ran `open`, and return its pid."""
    options = dict(stdin=subprocess.DEVNULL, env={**os.environ, "NUCLEOS_DAEMON_TOKEN": token},
                   close_fds=True)
    argv = renewer_argv(wave_id, lease_seconds, deadline, path)
    with open(path.with_suffix(".log"), "ab") as log:
        options.update(stdout=log, stderr=log)
        if os.name != "nt":
            return subprocess.Popen(argv, start_new_session=True, **options).pid
        # CREATE_NO_WINDOW, not DETACHED_PROCESS: a console-less process hands every console child
        # (git, cmd) a new VISIBLE window; a hidden console is inherited instead.
        detached = subprocess.CREATE_NO_WINDOW | subprocess.CREATE_NEW_PROCESS_GROUP
        try:
            # Out of the caller's job object too: a tool call's job may kill its children on close.
            return subprocess.Popen(argv, creationflags=detached | subprocess.CREATE_BREAKAWAY_FROM_JOB,
                                    **options).pid
        except OSError:
            return subprocess.Popen(argv, creationflags=detached, **options).pid


def open_wave(cwd: str, units: int, decision_workers: int, models: Path | None = None,
              spawn=spawn_renewer) -> dict:
    try:
        limits = read_limits(models)
    except LimitsError as error:
        # Not a degradation reason: nothing was refused, the config is broken. The skill gates on
        # `enabled` before ever calling `open` (desvio 11).
        return {**asdict(Opening(1, 1)), "config_error": str(error)}
    token = control_token(cwd)
    opening = request_wave(cwd, units, decision_workers, limits, token)
    if opening.wave_id is None:
        return asdict(opening)
    path = state_path(opening.wave_id)
    started = time.time()
    deadline = started + limits.max_wave_seconds
    try:
        write_state(path, {"wave_id": opening.wave_id, "status": "held", "renewer_pid": None,
                           "lease_seconds": opening.lease_seconds, "renewed_at": stamp(started),
                           "deadline_at": stamp(deadline), "unguarded_since": None})
        pid = spawn(opening.wave_id, token, opening.lease_seconds, deadline, path)
    except OSError as error:
        # Unrenewed, the lease would lapse under running workers; give it back now instead.
        release(opening.wave_id, token)
        try:
            path.unlink(missing_ok=True)
        except OSError:
            pass
        return asdict(Opening(1, opening.requested, degraded=[NO_AUTHORITY],
                              reason=f"the renewer could not start: {error!r}"))
    update_state(path, renewer_pid=pid)
    return {**asdict(opening), "state_file": str(path)}


def wave_status(wave_id: int) -> tuple[int, dict]:
    state = read_state(state_path(wave_id))
    if state is None:
        return EXIT_STOP, {"wave_id": wave_id, "status": "closed", "degraded": []}
    status = state.get("status", "closed")
    if status == "held" and not pid_alive(state.get("renewer_pid")):
        status = "renewer_gone"
    body = {**state, "status": status, "degraded": DEGRADED_FOR.get(status, ["slots_exhausted"])}
    return (EXIT_HELD if status == "held" else EXIT_STOP), body


def close_wave(wave_id: int, cwd: str) -> dict:
    """Stop the renewer and give the wave back. Always answers; never an error."""
    _, current = wave_status(wave_id)
    path = state_path(wave_id)
    update_state(path, status="closing")
    # Only a renewer the file still calls `held` (and `wave_status` found alive) is stopped. Any
    # other ending means it already exited, and its pid may belong to another process by now.
    # If the kill misses -- a venv's python.exe is a launcher whose child is the real
    # interpreter -- the renewer reads `closing` (or no file) on its next tick and exits.
    if current["status"] == "held":
        try:
            os.kill(current["renewer_pid"], signal.SIGTERM)  # TerminateProcess on Windows
        except OSError:
            pass
    token = control_token(cwd)
    released = release(wave_id, token) if token else False
    for leftover in (path, path.with_suffix(".log")):
        try:
            leftover.unlink(missing_ok=True)
        except OSError:
            pass  # gitignored and harmless; `status` on a closed wave reads its last ending
    return {"wave_id": wave_id, "released": released, "ended_as": current["status"],
            "unguarded_since": current.get("unguarded_since"), "degraded": current["degraded"]}


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    verbs = parser.add_subparsers(dest="verb", required=True)
    opening = verbs.add_parser("open")
    opening.add_argument("--cwd", required=True)
    opening.add_argument("--units", type=int, required=True)
    opening.add_argument("--workers", type=int, required=True,
                         help="the speed decision's workers (resolve_speed.py)")
    for name in ("status", "close"):
        verb = verbs.add_parser(name)
        verb.add_argument("--wave-id", type=int, required=True)
        verb.add_argument("--cwd", default=".")
    renewer = verbs.add_parser("_renew")
    renewer.add_argument("--wave-id", type=int, required=True)
    renewer.add_argument("--lease-seconds", type=int, required=True)
    renewer.add_argument("--deadline", type=float, required=True)
    renewer.add_argument("--state-file", type=Path, required=True)
    args = parser.parse_args(argv)

    if args.verb == "open":
        print(json.dumps(open_wave(os.path.abspath(args.cwd), args.units, args.workers)))
        return 0
    if args.verb == "status":
        code, body = wave_status(args.wave_id)
        print(json.dumps(body))
        return code
    if args.verb == "close":
        print(json.dumps(close_wave(args.wave_id, os.path.abspath(args.cwd))))
        return 0
    renew_loop(args.wave_id, os.environ.get("NUCLEOS_DAEMON_TOKEN", ""), args.lease_seconds,
               args.deadline, args.state_file)
    return 0


if __name__ == "__main__":
    sys.exit(main())
