#!/usr/bin/env python3
"""Heavy-command broker: runs one build/test command under a per-machine admission
scheme and appends one telemetry line per run.

    heavy.py [--prio 0|1|2] [--agent ID] [--kind auto|cargo|node|go] [--] argv...
    heavy.py status
    heavy.py report [--since YYYY-MM-DD]
    heavy.py hold-worktree [--prio N] [--] argv...
    heavy.py warm [--cwd PATH] [-- argv...]

Admission is a machine token queue: every request has a weight (heavy_classify) and a
priority; the head of the queue (priority, then arrival) runs once the held weights plus
its own fit the capacity (`NUCLEOS_HEAVY_CAPACITY`, default 4). Underneath it sits the
runtime: process liveness (pid + creation time), the directory mutex, running the child
inside a Windows Job object (so a hard-killed broker takes its child with it),
`NUCLEOS_HEAVY=0`, and fail-open. A bug in the broker must run the command, never block it.

Standard library only. Never `os.kill(pid, 0)` on Windows: CPython maps it to
TerminateProcess.
"""

from __future__ import annotations

import hashlib
import json
import os
import re
import shutil
import subprocess
import sys
import statistics
import threading
import time
from pathlib import Path

LOG_VERSION = 1
MUTEX_STALE_S = 60.0
MUTEX_WAIT_S = 30.0
STILL_ACTIVE = 259
DEFAULT_CAPACITY = 4
AGENT_WAIT_MAX_S = 540.0
CALLER_WAIT_MAX_S = 3600.0
EXIT_QUEUE_TIMEOUT = 75

SUBCOMMANDS = ("status", "report", "hold-worktree", "warm")
WARM_PRIO = 3
WARM_ARGV = ["cargo", "test", "-p", "nucleos-core", "--no-run"]


# --------------------------------------------------------------------------- liveness


def _win_proc_identity(pid: int):
    import ctypes
    from ctypes import wintypes

    k32 = ctypes.WinDLL("kernel32", use_last_error=True)
    k32.OpenProcess.argtypes = [wintypes.DWORD, wintypes.BOOL, wintypes.DWORD]
    k32.OpenProcess.restype = wintypes.HANDLE
    k32.GetExitCodeProcess.argtypes = [wintypes.HANDLE, ctypes.POINTER(wintypes.DWORD)]
    k32.GetExitCodeProcess.restype = wintypes.BOOL
    k32.GetProcessTimes.argtypes = [wintypes.HANDLE] + [ctypes.POINTER(wintypes.FILETIME)] * 4
    k32.GetProcessTimes.restype = wintypes.BOOL
    k32.CloseHandle.argtypes = [wintypes.HANDLE]

    PROCESS_QUERY_LIMITED_INFORMATION = 0x1000
    handle = k32.OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, False, pid)
    if not handle:
        # Access denied still means the process exists; anything else means it is gone.
        return (ctypes.get_last_error() == 5), None
    try:
        code = wintypes.DWORD()
        if not k32.GetExitCodeProcess(handle, ctypes.byref(code)):
            return False, None
        if code.value != STILL_ACTIVE:
            return False, None
        c, e, k, u = (wintypes.FILETIME() for _ in range(4))
        ctime = None
        if k32.GetProcessTimes(handle, ctypes.byref(c), ctypes.byref(e),
                               ctypes.byref(k), ctypes.byref(u)):
            ctime = (c.dwHighDateTime << 32) | c.dwLowDateTime
        return True, ctime
    finally:
        k32.CloseHandle(handle)


def proc_identity(pid: int):
    """(alive, creation_time). creation_time is None where it cannot be read."""
    try:
        pid = int(pid)
        if pid <= 0:
            return False, None
        if os.name == "nt":
            return _win_proc_identity(pid)
        try:
            raw = Path(f"/proc/{pid}/stat").read_text()
            # Field 22 (starttime), counted after the parenthesised command name.
            fields = raw[raw.rindex(")") + 2:].split()
            if fields[0] == "Z":
                return False, None
            return True, int(fields[19])
        except (OSError, ValueError, IndexError):
            pass
        try:
            os.kill(pid, 0)
            return True, None
        except ProcessLookupError:
            return False, None
        except PermissionError:
            return True, None
    except Exception:
        return False, None


def is_alive(pid, ctime=None) -> bool:
    """Alive, and (when both creation times are known) the same process, not a reused pid."""
    alive, now = proc_identity(pid)
    if not alive:
        return False
    if ctime is not None and now is not None and ctime != now:
        return False
    return True


# ----------------------------------------------------------------------------- mutex


class Mutex:
    """`mkdir $DIR/.mutex` plus an `owner` file. Stale when older than 60 s or the owner
    is dead; a stale one is removed and the acquisition retried."""

    def __init__(self, state_dir: Path, timeout: float = MUTEX_WAIT_S):
        self.path = Path(state_dir) / ".mutex"
        self.timeout = timeout
        self.held = False

    def _stale(self) -> bool:
        try:
            if time.time() - self.path.stat().st_mtime > MUTEX_STALE_S:
                return True
        except OSError:
            return False  # vanished meanwhile: the retry will take it
        try:
            owner = json.loads((self.path / "owner").read_text())
            return not is_alive(owner.get("pid"), owner.get("ctime"))
        except (OSError, ValueError):
            # No readable owner yet: its creator may be between mkdir and the write.
            try:
                return time.time() - self.path.stat().st_mtime > 5.0
            except OSError:
                return False

    def acquire(self) -> None:
        deadline = time.time() + self.timeout
        self.path.parent.mkdir(parents=True, exist_ok=True)
        while True:
            try:
                self.path.mkdir()
                break
            except (FileExistsError, PermissionError):
                # Windows answers a mkdir on a directory whose delete is still pending
                # with "access denied", not "exists".
                if self._stale():
                    shutil.rmtree(self.path, ignore_errors=True)
                    continue
                if time.time() > deadline:
                    raise TimeoutError("broker mutex busy")
                time.sleep(0.05)
        self.held = True
        _, ctime = proc_identity(os.getpid())
        try:
            (self.path / "owner").write_text(json.dumps({"pid": os.getpid(), "ctime": ctime}))
        except OSError:
            pass

    def release(self) -> None:
        if self.held:
            self.held = False
            try:
                (self.path / "owner").unlink()
            except OSError:
                pass
            for _ in range(50):
                try:
                    self.path.rmdir()
                    return
                except FileNotFoundError:
                    return
                except OSError:
                    time.sleep(0.02)
            shutil.rmtree(self.path, ignore_errors=True)

    def __enter__(self):
        self.acquire()
        return self

    def __exit__(self, *exc):
        self.release()
        return False


# ------------------------------------------------------------------------ Job object


def make_kill_on_close_job():
    """A Windows Job object that kills its processes when the last handle closes.
    Returns the handle (keep it open for the broker's life) or None."""
    if os.name != "nt":
        return None
    import ctypes
    from ctypes import wintypes

    k32 = ctypes.WinDLL("kernel32", use_last_error=True)

    class BASIC(ctypes.Structure):
        _fields_ = [
            ("PerProcessUserTimeLimit", ctypes.c_int64),
            ("PerJobUserTimeLimit", ctypes.c_int64),
            ("LimitFlags", wintypes.DWORD),
            ("MinimumWorkingSetSize", ctypes.c_size_t),
            ("MaximumWorkingSetSize", ctypes.c_size_t),
            ("ActiveProcessLimit", wintypes.DWORD),
            ("Affinity", ctypes.c_size_t),
            ("PriorityClass", wintypes.DWORD),
            ("SchedulingClass", wintypes.DWORD),
        ]

    class IO(ctypes.Structure):
        _fields_ = [(n, ctypes.c_uint64) for n in (
            "ReadOperationCount", "WriteOperationCount", "OtherOperationCount",
            "ReadTransferCount", "WriteTransferCount", "OtherTransferCount")]

    class EXT(ctypes.Structure):
        _fields_ = [
            ("BasicLimitInformation", BASIC),
            ("IoInfo", IO),
            ("ProcessMemoryLimit", ctypes.c_size_t),
            ("JobMemoryLimit", ctypes.c_size_t),
            ("PeakProcessMemoryUsed", ctypes.c_size_t),
            ("PeakJobMemoryUsed", ctypes.c_size_t),
        ]

    k32.CreateJobObjectW.argtypes = [ctypes.c_void_p, wintypes.LPCWSTR]
    k32.CreateJobObjectW.restype = wintypes.HANDLE
    k32.SetInformationJobObject.argtypes = [
        wintypes.HANDLE, ctypes.c_int, ctypes.c_void_p, wintypes.DWORD]
    k32.SetInformationJobObject.restype = wintypes.BOOL

    job = k32.CreateJobObjectW(None, None)
    if not job:
        return None
    info = EXT()
    info.BasicLimitInformation.LimitFlags = 0x2000  # JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
    JobObjectExtendedLimitInformation = 9
    if not k32.SetInformationJobObject(
        job, JobObjectExtendedLimitInformation, ctypes.byref(info), ctypes.sizeof(info)
    ):
        return None
    return job


def assign_to_job(job, proc) -> bool:
    if job is None or os.name != "nt":
        return False
    import ctypes
    from ctypes import wintypes

    k32 = ctypes.WinDLL("kernel32", use_last_error=True)
    k32.AssignProcessToJobObject.argtypes = [wintypes.HANDLE, wintypes.HANDLE]
    k32.AssignProcessToJobObject.restype = wintypes.BOOL
    return bool(k32.AssignProcessToJobObject(job, int(proc._handle)))


# ----------------------------------------------------------------------------- queue


def _env_float(name: str, default: float) -> float:
    try:
        v = float(os.environ.get(name, ""))
        return v if v > 0 else default
    except ValueError:
        return default


def capacity() -> int:
    try:
        return max(1, int(os.environ.get("NUCLEOS_HEAVY_CAPACITY", DEFAULT_CAPACITY)))
    except ValueError:
        return DEFAULT_CAPACITY


def wait_max_s(opt, agent) -> float:
    """--wait-max > NUCLEOS_HEAVY_WAIT_MAX > 540 s with an agent, 3600 s without."""
    for raw in (opt, os.environ.get("NUCLEOS_HEAVY_WAIT_MAX")):
        try:
            if raw not in (None, "") and float(raw) > 0:
                return float(raw)
        except ValueError:
            pass
    return AGENT_WAIT_MAX_S if agent else CALLER_WAIT_MAX_S


def _entries(directory: Path, reap: bool = True) -> list[tuple[Path, dict]]:
    """Records in a state subdirectory. A dead owner (pid gone, or reused: other creation
    time) is removed when `reap`, and never returned."""
    out = []
    if not directory.is_dir():
        return out
    for f in directory.iterdir():
        try:
            rec = json.loads(f.read_text(encoding="utf-8"))
            pid = rec["pid"]
        except (OSError, ValueError, KeyError, TypeError):
            continue
        if not is_alive(pid, rec.get("ctime")):
            if reap:
                try:
                    f.unlink()
                except OSError:
                    pass
            continue
        out.append((f, rec))
    return out


def _epoch_ns(path: Path) -> int:
    try:
        return int(path.name.split("-")[1])
    except (IndexError, ValueError):
        return 0


def _wkey(path: Path) -> tuple[int, int]:
    """(priority, arrival) encoded in a `<prio>-<epoch_ns>-<pid>` file name."""
    try:
        prio = int(path.name.split("-")[0])
    except (IndexError, ValueError):
        prio = 1
    return prio, _epoch_ns(path)


def _effective_prios(state: Path, items: list[tuple[Path, dict]]) -> dict[str, int]:
    """Queue-file name -> effective priority: the entry's own, lifted to the best priority
    of every waiter of any worktree lock its pid holds (priority inheritance)."""
    own = {f.name: _int_prio(r.get("prio")) for f, r in items}
    wt = state / "wt"
    if not wt.is_dir():
        return own
    best: dict[int, int] = {}
    for lock in wt.glob("*.lock"):
        try:
            pid = int(json.loads(lock.read_text(encoding="utf-8"))["pid"])
        except (OSError, ValueError, KeyError, TypeError):
            continue
        wdir = wt / (lock.name[: -len(".lock")] + ".waiters")
        for f, _ in _entries(wdir, reap=False):
            best[pid] = min(best.get(pid, 9), _wkey(f)[0])
    for f, r in items:
        try:
            lifted = best.get(int(r.get("pid")))
        except (TypeError, ValueError):
            lifted = None
        if lifted is not None:
            own[f.name] = min(own[f.name], lifted)
    return own


def _queue_order(items: list[tuple[Path, dict]], state: Path | None = None
                 ) -> list[tuple[Path, dict]]:
    eff = _effective_prios(state, items) if state is not None else {}
    return sorted(items, key=lambda it: (
        eff.get(it[0].name, _int_prio(it[1].get("prio"))), _epoch_ns(it[0])))


def acquire_token(directory: Path, rec: dict, wait_cap: float,
                  arrival_ns: int | None = None) -> tuple[bool, float]:
    """Queue, wait for the head position and room, then hold. Returns (admitted, waited_s).
    Not admitted means the wait cap expired; the queue entry is gone either way (when
    admitted it has become the holder file)."""
    queue_dir, held_dir = directory / "queue", directory / "held"
    me = os.getpid()
    poll = _env_float("NUCLEOS_HEAVY_POLL_S", 0.5)
    t0 = time.time()
    qfile = queue_dir / f"{rec['prio']}-{arrival_ns or time.time_ns()}-{me}"
    queued = False
    last_report = t0
    try:
        with Mutex(directory):
            queue_dir.mkdir(parents=True, exist_ok=True)
            held_dir.mkdir(parents=True, exist_ok=True)
            qfile.write_text(json.dumps(rec), encoding="utf-8")
            queued = True
        while True:
            position = 0
            with Mutex(directory):
                order = _queue_order(_entries(queue_dir), directory)
                held = sum(int(r.get("weight") or 1) for _, r in _entries(held_dir))
                names = [f.name for f, _ in order]
                position = names.index(qfile.name) + 1 if qfile.name in names else 0
                if position == 0:
                    # Our own entry vanished (state wiped): put it back and carry on.
                    qfile.parent.mkdir(parents=True, exist_ok=True)
                    held_dir.mkdir(parents=True, exist_ok=True)
                    qfile.write_text(json.dumps(rec), encoding="utf-8")
                elif position == 1 and held + rec["weight"] <= capacity():
                    (held_dir / str(me)).write_text(json.dumps(rec), encoding="utf-8")
                    qfile.unlink()
                    queued = False
                    return True, time.time() - t0
            now = time.time()
            if now - t0 >= wait_cap:
                sys.stderr.write(
                    f"heavy: in queue for {int(now - t0)}s (position {position}); nothing was "
                    "compiled - try again, re-run with the Bash tool timeout set to 600000\n")
                return False, now - t0
            if now - last_report >= 30:
                last_report = now
                sys.stderr.write(
                    f"heavy: waiting {int(now - t0)}s, position {position} in the queue\n")
            time.sleep(poll)
    finally:
        if queued:
            try:
                qfile.unlink()
            except OSError:
                pass


def release_token(directory: Path) -> None:
    try:
        (directory / "held" / str(os.getpid())).unlink()
    except OSError:
        pass


# ------------------------------------------------------------------- worktree lock


def worktree_root() -> str:
    """`git rev-parse --show-toplevel` of the cwd; the cwd itself when git cannot say."""
    try:
        out = subprocess.run(
            ["git", "rev-parse", "--show-toplevel"], capture_output=True, text=True, timeout=20)
        top = out.stdout.strip()
        if out.returncode == 0 and top:
            return os.path.normpath(top)
    except Exception:
        pass
    return os.getcwd()


def worktree_hash(path: str) -> str:
    norm = os.path.normcase(os.path.normpath(path)).lower().replace("\\", "/")
    return hashlib.sha256(norm.encode("utf-8")).hexdigest()[:16]


def _lock_paths(directory: Path, h: str) -> tuple[Path, Path]:
    wt = directory / "wt"
    return wt / f"{h}.lock", wt / f"{h}.waiters"


def _read_lock(lock: Path) -> dict | None:
    """The live holder's record, or None (a dead holder's file is removed)."""
    try:
        rec = json.loads(lock.read_text(encoding="utf-8"))
        pid = rec["pid"]
    except (OSError, ValueError, KeyError, TypeError):
        return None
    if not is_alive(pid, rec.get("ctime")):
        try:
            lock.unlink()
        except OSError:
            pass
        return None
    return rec


def acquire_lock(directory: Path, h: str, rec: dict, wait_cap: float
                 ) -> tuple[bool, float, int]:
    """Take the worktree lock, waiting in `<hash>.waiters` by (priority, arrival). Holds no
    token while it waits. Returns (acquired, waited_s, arrival_ns)."""
    lock, wdir = _lock_paths(directory, h)
    me = os.getpid()
    poll = _env_float("NUCLEOS_HEAVY_POLL_S", 0.5)
    t0 = time.time()
    arrival = time.time_ns()
    wfile = wdir / f"{rec['prio']}-{arrival}-{me}"
    mine = (int(rec["prio"]), arrival)
    queued = False
    last_report = t0
    try:
        while True:
            with Mutex(directory):
                wdir.mkdir(parents=True, exist_ok=True)
                holder = _read_lock(lock)
                if holder is not None and holder.get("pid") == me:
                    # Handed over by the previous holder on its release.
                    lock.write_text(json.dumps(rec), encoding="utf-8")
                    return True, time.time() - t0, arrival
                if holder is None:
                    ahead = [f for f, _ in _entries(wdir)
                             if f.name != wfile.name and _wkey(f) < mine]
                    if not ahead:
                        lock.write_text(json.dumps(rec), encoding="utf-8")
                        return True, time.time() - t0, arrival
                if not queued or not wfile.exists():
                    wfile.write_text(json.dumps(rec), encoding="utf-8")
                    queued = True
            now = time.time()
            if now - t0 >= wait_cap:
                sys.stderr.write(
                    f"heavy: waited {int(now - t0)}s for the worktree lock; nothing was "
                    "compiled - try again, re-run with the Bash tool timeout set to 600000\n")
                return False, now - t0, arrival
            if now - last_report >= 30:
                last_report = now
                sys.stderr.write(f"heavy: waiting {int(now - t0)}s for the worktree lock\n")
            time.sleep(poll)
    finally:
        if queued:
            try:
                wfile.unlink()
            except OSError:
                pass


def release_lock(directory: Path, h: str):
    """Release, handing the lock straight to the first waiter. Returns (pid, ctime,
    will_ask_token) of the successor, or None."""
    lock, wdir = _lock_paths(directory, h)
    with Mutex(directory):
        try:
            cur = json.loads(lock.read_text(encoding="utf-8"))
        except (OSError, ValueError):
            return None
        if cur.get("pid") != os.getpid():
            return None
        try:
            lock.unlink()
        except OSError:
            pass
        waiting = sorted(_entries(wdir), key=lambda it: _wkey(it[0]))
        if not waiting:
            return None
        f, wrec = waiting[0]
        wrec = dict(wrec, start=time.strftime("%Y-%m-%dT%H:%M:%S%z"))
        lock.write_text(json.dumps(wrec), encoding="utf-8")
        try:
            f.unlink()
        except OSError:
            pass
        return wrec["pid"], wrec.get("ctime"), not wrec.get("hold")


def await_successor(directory: Path, succ, limit: float = 2.0) -> None:
    """Keep the token until the lock's new owner is in the machine queue (or holds a token),
    so a request that was already queued cannot slip in between the two."""
    if not succ or not succ[2]:
        return
    pid, ctime, _ = succ
    end = time.time() + limit
    queue_dir = directory / "queue"
    while time.time() < end:
        try:
            if any(p.name.endswith(f"-{pid}") for p in queue_dir.iterdir()):
                return
        except OSError:
            pass
        if (directory / "held" / str(pid)).exists() or not is_alive(pid, ctime):
            return
        time.sleep(0.02)


# ------------------------------------------------------------------------------ run


def state_dir() -> Path:
    override = os.environ.get("NUCLEOS_HEAVY_DIR")
    if override:
        return Path(override)
    return Path.home() / ".nucleos" / "heavy"


_COMPILING = re.compile(rb"^(?:\x1b\[[0-9;]*m)*\s*(?:\x1b\[[0-9;]*m)*Compiling ")


def _new_stats() -> dict:
    return {"compiled": False, "test_failed": False, "compile_error": False}


def _pump(src, dst, stats: dict) -> None:
    try:
        for line in iter(src.readline, b""):
            if _COMPILING.match(line):
                stats["compiled"] = True
            if b"test result: FAILED" in line:
                stats["test_failed"] = True
            if b"could not compile" in line:
                stats["compile_error"] = True
            try:
                dst.write(line)
                dst.flush()
            except Exception:
                pass
    finally:
        try:
            src.close()
        except Exception:
            pass


def run_child(argv: list[str], env: dict | None, job, low: bool = False
              ) -> tuple[int, dict]:
    """Run argv, streaming stdout/stderr through; return (exit, stats). `low` runs it at
    below-normal CPU priority (Windows) with no console window of its own."""
    stats = _new_stats()
    # CreateProcess finds only `.exe` without an extension, so `npm`/`npx` (`.cmd` shims)
    # failed with WinError 2: resolve argv[0] through PATH and PATHEXT the way a shell would.
    if argv and not os.path.dirname(argv[0]):
        found = shutil.which(argv[0], path=(env or os.environ).get("PATH"))
        if found:
            argv = [found] + list(argv[1:])
    flags = 0
    if low and os.name == "nt":
        flags = 0x00004000 | 0x08000000  # BELOW_NORMAL_PRIORITY_CLASS | CREATE_NO_WINDOW
    proc = subprocess.Popen(
        argv, env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE, creationflags=flags
    )
    try:
        assign_to_job(job, proc)
    except Exception:
        pass
    out = getattr(sys.stdout, "buffer", sys.stdout)
    err = getattr(sys.stderr, "buffer", sys.stderr)
    threads = [
        threading.Thread(target=_pump, args=(proc.stdout, out, stats), daemon=True),
        threading.Thread(target=_pump, args=(proc.stderr, err, stats), daemon=True),
    ]
    for t in threads:
        t.start()
    code = proc.wait()
    for t in threads:
        t.join(timeout=10)
    return code, stats


# ------------------------------------------------------------------ cargo fingerprint

FP_SUBCOMMANDS = ("test", "check", "build", "clippy", "doc", "bench", "run")
# Cargo options that consume the next token, so it is never mistaken for a positional filter.
_VALUE_FLAGS = {
    "-p", "--package", "--test", "--bench", "--example", "--bin", "--features", "-F",
    "--manifest-path", "--target", "--target-dir", "--profile", "--exclude", "-j", "--jobs",
    "--color", "--message-format", "--config", "-Z", "--lockfile-path", "--timings",
}


def _real(path) -> str:
    return os.path.normcase(os.path.realpath(str(path)))


def _is_cargo(argv: list[str]) -> bool:
    base = os.path.basename(argv[0]).lower() if argv else ""
    return os.path.splitext(base)[0] == "cargo"


def _cargo_args(argv: list[str]) -> list[str]:
    """Cargo's own tokens: everything between the program and a `--`."""
    out = []
    for a in argv[1:]:
        if a == "--":
            break
        out.append(a)
    return out


def _flag_values(args: list[str], *names: str) -> list[str]:
    vals = []
    for i, a in enumerate(args):
        name, eq, val = a.partition("=")
        if name in names:
            if eq:
                vals.append(val)
            elif i + 1 < len(args):
                vals.append(args[i + 1])
    return vals


def _fp_argv(argv: list[str]) -> list[str]:
    """The argv part of the fingerprint: the subcommand, flags and their values, without a
    positional test filter or anything after `--`."""
    out, i, args, seen_sub = [], 0, _cargo_args(argv), False
    while i < len(args):
        a = args[i]
        if a.startswith("-"):
            out.append(a)
            if a in _VALUE_FLAGS and i + 1 < len(args):
                out.append(args[i + 1])
                i += 1
        elif not seen_sub:
            seen_sub = True
            out.append(a)
        # else: a positional filter, dropped
        i += 1
    return out


def resolve_crate(argv: list[str], root: str, cwd: str) -> Path | None:
    """The crate directory a cargo command builds, or None when it cannot be told."""
    found = resolve_crate_how(argv, root, cwd)
    return found[0] if found else None


def resolve_crate_how(argv: list[str], root: str, cwd: str):
    """(crate dir, named) or None. `named` is False when the crate is only the root default
    (no `-p`, no manifest, not inside src-tauri): a guess, not something the caller said."""
    if not _is_cargo(argv):
        return None
    args = _cargo_args(argv)
    sub = next((a for a in args if not a.startswith("-") and not a.startswith("+")), "")
    if sub not in FP_SUBCOMMANDS:
        return None
    rootp = Path(root)
    manifests = _flag_values(args, "--manifest-path")
    if manifests:
        m = Path(manifests[-1])
        m = m if m.is_absolute() else Path(cwd) / m
        return (m.parent, True) if m.parent.is_dir() else None
    tauri = rootp / "shell" / "src-tauri"
    rc, rt = _real(cwd), _real(tauri)
    if rc == rt or rc.startswith(rt + os.sep):
        return tauri, True
    if "--workspace" in args or "--all" in args:
        return None
    pkgs = _flag_values(args, "-p", "--package")
    if any(p != "nucleos-core" for p in pkgs):
        return None
    core = rootp / "core"
    return (core, bool(pkgs)) if core.is_dir() else None


def _config_target_dir(path: Path) -> str | None:
    try:
        text = path.read_text(encoding="utf-8")
    except OSError:
        return None
    m = re.search(r'^\s*target-dir\s*=\s*"([^"]*)"', text, re.M)
    if not m:
        return None
    val = Path(m.group(1))
    base = path.parent.parent  # the directory that holds `.cargo`
    return str(val if val.is_absolute() else base / val)


def _td_root() -> Path:
    """Where the per-worktree target dirs live: NUCLEOS_HEAVY_TD_ROOT, else the parent of the
    main checkout (where `--land` looks for `.cargo-target-<name>`)."""
    env = os.environ.get("NUCLEOS_HEAVY_TD_ROOT")
    if env:
        return Path(env)
    here = Path(__file__).resolve().parents[1]
    try:
        out = subprocess.run(["git", "rev-parse", "--path-format=absolute", "--git-common-dir"],
                             cwd=str(here), capture_output=True, text=True, timeout=20)
        common = out.stdout.strip()
        if out.returncode == 0 and common:
            return Path(common).resolve().parent.parent
    except Exception:
        pass
    return here.parent


def injected_target_dir(argv: list[str], root: str) -> str | None:
    """The target dir the broker gives a cargo child that named none, or None."""
    if os.environ.get("NUCLEOS_HEAVY_TARGET") == "0" or not _is_cargo(argv):
        return None
    if os.environ.get("CARGO_TARGET_DIR") or os.environ.get("CARGO_BUILD_TARGET_DIR"):
        return None
    if _flag_values(_cargo_args(argv), "--target-dir"):
        return None
    slug = ""
    try:
        out = subprocess.run(["git", "rev-parse", "--abbrev-ref", "HEAD"], cwd=root,
                             capture_output=True, text=True, timeout=20)
        if out.returncode == 0:
            slug = out.stdout.strip().replace("/", "-")
    except Exception:
        pass
    if slug in ("", "HEAD", "test", "gates", "gate"):
        slug = os.path.basename(os.path.normpath(root))
    return str(_td_root() / f".cargo-target-{slug}")


def resolve_target_dir(argv: list[str], crate: Path, root: str, cwd: str,
                       injected: str | None = None) -> str:
    env = os.environ
    found = env.get("CARGO_TARGET_DIR") or env.get("CARGO_BUILD_TARGET_DIR")
    if not found:
        flag = _flag_values(_cargo_args(argv), "--target-dir")
        found = flag[-1] if flag else None
    if not found:
        found = injected
    if not found:
        home = Path(env.get("CARGO_HOME") or (Path.home() / ".cargo"))
        for cfg in (crate / ".cargo" / "config.toml", Path(root) / ".cargo" / "config.toml",
                    home / "config.toml"):
            found = _config_target_dir(cfg)
            if found:
                break
    if not found:
        workspace = Path(root) if _real(crate) == _real(Path(root) / "core") else crate
        found = str(workspace / "target")
    p = Path(found)
    return _real(p if p.is_absolute() else Path(cwd) / p)


def _git(root: str, *args: str) -> bytes:
    out = subprocess.run(["git", "-c", "core.quotepath=off", *args], cwd=root,
                         capture_output=True, timeout=120)
    if out.returncode != 0:
        raise RuntimeError(f"git {args[0]} failed")
    return out.stdout


def compute_fingerprint(argv: list[str], crate: Path, root: str, target_dir: str) -> str:
    rel = os.path.relpath(_real(crate), _real(root)).replace("\\", "/")
    if rel.startswith(".."):
        raise ValueError("crate outside the worktree")
    paths = [rel, "Cargo.toml", "Cargo.lock", "rust-toolchain.toml", ".cargo", "scripts"]
    h = hashlib.sha256()
    h.update(_git(root, "rev-parse", "HEAD").strip())
    h.update(b"\0diff\0")
    h.update(_git(root, "diff", "HEAD", "--binary", "--no-ext-diff", "--", *paths))
    h.update(b"\0untracked\0")
    names = _git(root, "ls-files", "-z", "--others", "--exclude-standard", "--", *paths)
    for name in sorted(n for n in names.split(b"\0") if n):
        h.update(name + b"\0")
        try:
            h.update(Path(root, name.decode("utf-8", "surrogateescape")).read_bytes())
        except OSError:
            pass
        h.update(b"\0")
    h.update(b"\0argv\0" + json.dumps(_fp_argv(argv)).encode("utf-8"))
    envs = {k: v for k, v in os.environ.items()
            if k == "RUSTFLAGS" or k.startswith(("CARGO_PROFILE_", "CARGO_FEATURES"))}
    h.update(b"\0env\0" + json.dumps(envs, sort_keys=True).encode("utf-8"))
    h.update(b"\0target\0" + target_dir.encode("utf-8"))
    return h.hexdigest()


def _td_path(directory: Path, target_dir: str) -> Path:
    key = hashlib.sha256(target_dir.encode("utf-8")).hexdigest()[:24]
    return directory / "td" / (key + ".json")


def registry_hit(directory: Path, target_dir: str, fingerprint: str, worktree: str) -> bool:
    """The target dir was last written by this worktree at exactly this fingerprint."""
    try:
        rec = json.loads(_td_path(directory, target_dir).read_text(encoding="utf-8"))
    except (OSError, ValueError):
        return False
    return (rec.get("fingerprint") == fingerprint
            and _real(rec.get("worktree", "")) == _real(worktree))


def registry_write(directory: Path, target_dir: str, fingerprint: str, worktree: str) -> None:
    path = _td_path(directory, target_dir)
    path.parent.mkdir(parents=True, exist_ok=True)
    tmp = path.with_name(f"{path.name}.{os.getpid()}.tmp")
    tmp.write_text(json.dumps({"fingerprint": fingerprint, "worktree": worktree,
                               "target_dir": target_dir}), encoding="utf-8")
    os.replace(tmp, path)


def registry_forget(directory: Path, target_dir: str) -> None:
    try:
        _td_path(directory, target_dir).unlink()
    except OSError:
        pass


def append_log(row: dict, directory: Path) -> None:
    try:
        directory.mkdir(parents=True, exist_ok=True)
        with open(directory / "log.jsonl", "a", encoding="utf-8") as f:
            f.write(json.dumps(row) + "\n")
    except Exception as exc:
        sys.stderr.write(f"heavy: could not write the log: {exc}\n")


def _int_prio(value, default=1) -> int:
    try:
        return max(0, min(WARM_PRIO, int(value)))
    except (TypeError, ValueError):
        return default


def parse_args(args: list[str]):
    """Options, then an optional `--`, then argv. A missing `--` is accepted: the first
    token that is not a known option starts the argv (PowerShell 5.1 can swallow it)."""
    opts = {"prio": None, "agent": None, "kind": "auto", "wait-max": None}
    i = 0
    while i < len(args):
        a = args[i]
        if a == "--":
            i += 1
            break
        name, eq, val = a.partition("=")
        if name in ("--prio", "--agent", "--kind", "--wait-max"):
            if eq:
                opts[name[2:]] = val
                i += 1
            elif i + 1 < len(args):
                opts[name[2:]] = args[i + 1]
                i += 2
            else:
                i += 1
            continue
        break
    return opts, args[i:]


def broker_run(args: list[str], held: bool = False) -> int:
    opts, argv = parse_args(args)
    if not argv:
        sys.stderr.write("heavy: no command given\n")
        return 2
    env = None
    warm = os.environ.get("NUCLEOS_HEAVY_WARM") == "1"

    if os.environ.get("NUCLEOS_HEAVY") == "0":
        code, _ = run_child(argv, env, None)
        return code

    row: dict = {}
    job = None
    holding = False
    lock_hash = None
    directory = None
    nested = bool(os.environ.get("NUCLEOS_HEAVY_TOKEN"))
    in_held = bool(os.environ.get("NUCLEOS_HEAVY_HELD"))
    t_start = time.time()
    fp = None  # (crate dir, target dir, fingerprint) once computed
    hit = False
    root = None
    try:
        if held:
            # hold-worktree: the session's priority is --prio, default 0 (never the env's).
            prio = _int_prio(opts["prio"], 0)
        else:
            prio = _int_prio(opts["prio"] or os.environ.get("NUCLEOS_HEAVY_PRIO"), 1)
        weight, kind = 1, opts["kind"] or "auto"
        try:
            sys.path.insert(0, str(Path(__file__).resolve().parents[1] / ".ai" / "scripts"))
            import heavy_classify  # type: ignore

            verdict = heavy_classify.classify(argv)
            if verdict.weight:
                weight = verdict.weight
            if kind == "auto":
                kind = verdict.kind or "auto"
        except Exception:
            pass  # fail-open: unclassified, admitted anyway
        if opts["kind"] == "cargo":
            weight = max(int(weight), 2)  # an explicit cargo request is a cargo-weight one
        weight = max(1, min(int(weight), capacity()))
        agent = opts["agent"] or os.environ.get("NUCLEOS_HEAVY_AGENT")
        directory = state_dir()
        directory.mkdir(parents=True, exist_ok=True)
        wait_token = wait_lock = 0.0
        cap = wait_max_s(opts["wait-max"], opts["agent"])
        arrival = None
        _, ctime = proc_identity(os.getpid())
        rec = {
            "pid": os.getpid(), "ctime": ctime, "weight": weight, "prio": prio,
            "agent": agent, "worktree": os.getcwd(), "argv": argv,
        }
        # Fixed order: worktree lock -> token. A cargo run takes the lock of its worktree
        # (waiting without holding a token); a session already inside a held worktree or
        # under a token holder does not retake it.
        # Only an explicit `--kind cargo` asks for the lock: a kind the classifier inferred does
        # not (test_heavy_queue.py runs classified cargo commands in one worktree, side by
        # side, and expects them to share the machine budget rather than serialise).
        crate, named = None, False
        inject = None
        if kind in ("cargo", "auto") and _is_cargo(argv):
            try:
                inject = injected_target_dir(argv, root or worktree_root())
            except Exception:
                inject = None
        if not nested and not in_held and not held and kind in ("cargo", "auto"):
            # A cargo build/test of a crate we can name shares that worktree's target dir
            # state, so it is serialised by the worktree lock as well.
            try:
                root = worktree_root()
                found = resolve_crate_how(argv, root, os.getcwd())
                crate, named = found if found else (None, False)
            except Exception:
                root = crate = None
        # The lock is for a crate the caller NAMED; a bare `cargo test` at the root only gets
        # a fingerprint (it is still a guess, and test_heavy_queue.py runs such commands
        # side by side expecting them to share the machine budget).
        if (held or opts["kind"] == "cargo" or (crate is not None and named))                 and not nested and not in_held:
            root = root or worktree_root()
            lock_hash = worktree_hash(root)
            lrec = dict(rec, worktree=root, start=time.strftime("%Y-%m-%dT%H:%M:%S%z"),
                        hold=held)
            got, wait_lock, arrival = acquire_lock(directory, lock_hash, lrec, cap)
            if not got:
                lock_hash = None
                _log_timeout(directory, agent, prio, kind, weight, wait_lock, argv)
                return EXIT_QUEUE_TIMEOUT
        if crate is not None and not nested and not held:
            # Order: lock -> fingerprint -> token. Fail-open: no fingerprint, no hit.
            try:
                tdir = resolve_target_dir(argv, crate, root, os.getcwd(), inject)
                fp = (crate, tdir, compute_fingerprint(argv, crate, root, tdir))
                hit = registry_hit(directory, tdir, fp[2], root)
            except Exception as exc:
                fp = None
                sys.stderr.write(f"heavy: no fingerprint: {exc}\n")
        if hit:
            weight = 0
            # A weight-0 run never visits the queue, so it reaps dead entries itself.
            try:
                with Mutex(directory):
                    _entries(directory / "queue")
                    _entries(directory / "held")
            except Exception:
                pass
            env = dict(os.environ)
            env.pop("NUCLEOS_HEAVY_TOKEN", None)
        elif held:
            env = dict(os.environ)
            env.pop("NUCLEOS_HEAVY_TOKEN", None)
            env["NUCLEOS_HEAVY_HELD"] = str(os.getpid())
            env["NUCLEOS_HEAVY_PRIO"] = str(prio)
        elif nested:
            # A call made under a broker that already holds a token takes no second one
            # (it would wait on its own parent). The token covers ONE nesting level: the
            # child below does not inherit it, so a chain that keeps re-entering the
            # broker is bounded by the wait cap instead of recursing for ever.
            env = dict(os.environ)
            env.pop("NUCLEOS_HEAVY_TOKEN", None)
        else:
            admitted, wait_token = acquire_token(directory, rec, cap, arrival)
            if not admitted:
                _release_all(directory, lock_hash, False)
                lock_hash = None
                _log_timeout(directory, agent, prio, kind, weight, wait_lock + wait_token, argv)
                return EXIT_QUEUE_TIMEOUT
            holding = True
            env = dict(os.environ)
            env["NUCLEOS_HEAVY_TOKEN"] = str(os.getpid())
        if inject and env is not None:
            env["CARGO_TARGET_DIR"] = inject
        eff_td = None
        if kind in ("cargo", "auto") and _is_cargo(argv):
            eff_td = (inject or os.environ.get("CARGO_TARGET_DIR")
                      or os.environ.get("CARGO_BUILD_TARGET_DIR")
                      or next(iter(_flag_values(_cargo_args(argv), "--target-dir")[-1:]), None))
            if fp is not None:
                eff_td = fp[1]
        job = make_kill_on_close_job()
        row = {
            "v": LOG_VERSION, "ts": time.strftime("%Y-%m-%dT%H:%M:%S%z"),
            "worktree": os.getcwd(), "agent": agent,
            "prio": prio, "eff_prio": prio, "kind": kind, "weight": weight,
            "wait_lock_s": round(wait_lock, 3), "wait_token_s": round(wait_token, 3),
        }
        if warm:
            row["warm"] = True
        if eff_td:
            row["target_dir"] = eff_td
    except Exception as exc:
        _release_all(directory, lock_hash, holding)
        code, stats = run_child(argv, env, None)
        append_log({
            "v": LOG_VERSION, "ts": time.strftime("%Y-%m-%dT%H:%M:%S%z"),
            "worktree": os.getcwd(), "agent": opts["agent"], "prio": None, "eff_prio": None,
            "kind": opts["kind"], "weight": None, "wait_lock_s": 0.0, "wait_token_s": 0.0,
            "run_s": round(time.time() - t_start, 3), "compiled": stats["compiled"],
            "fp_hit": False, "fp_miss": False, "exit": code,
            "argv0": argv[0], "broker_error": f"{type(exc).__name__}: {exc}",
        }, _safe_state_dir())
        return code

    t_run = time.time()
    try:
        code, stats = run_child(argv, env, job, low=warm)
    except OSError as exc:
        sys.stderr.write(f"heavy: {argv[0]}: {exc}\n")
        code, stats = 127, _new_stats()
        row["broker_error"] = f"spawn: {exc}"
    except BaseException:
        _release_all(directory, lock_hash, holding)
        raise
    row.update({
        "run_s": round(time.time() - t_run, 3), "compiled": stats["compiled"],
        "fp_hit": hit, "fp_miss": hit and stats["compiled"], "exit": code,
        "argv0": argv[0],
    })
    # Register and log BEFORE releasing: the lock's next owner reads the registry the moment
    # it holds the lock, and must see what this run just built.
    if fp is not None:
        _record_fingerprint(directory, fp, root, code, stats)
    append_log(row, directory)
    _release_all(directory, lock_hash, holding)
    return code


def _record_fingerprint(directory: Path, fp, root: str, code: int, stats: dict) -> None:
    """Register the fingerprint after a run that left a usable target dir: exit 0, or a
    test-only failure. Any other failure may have half-rewritten it, so the entry goes."""
    try:
        _, tdir, fingerprint = fp
        test_only = stats["test_failed"] and not stats["compile_error"]
        if code == 0 or test_only:
            registry_write(directory, tdir, fingerprint, root)
        else:
            registry_forget(directory, tdir)
    except Exception as exc:
        sys.stderr.write(f"heavy: could not record the fingerprint: {exc}\n")


def _log_timeout(directory, agent, prio, kind, weight, waited, argv) -> None:
    """A run that gave up waiting still leaves a row, so `report` counts the exit 75."""
    append_log({
        "v": LOG_VERSION, "ts": time.strftime("%Y-%m-%dT%H:%M:%S%z"),
        "worktree": os.getcwd(), "agent": agent, "prio": prio, "eff_prio": prio,
        "kind": kind, "weight": weight, "wait_lock_s": round(waited, 3), "wait_token_s": 0.0,
        "run_s": 0.0, "compiled": False, "fp_hit": False, "fp_miss": False,
        "exit": EXIT_QUEUE_TIMEOUT, "argv0": argv[0],
    }, directory)


def _release_all(directory, lock_hash, holding: bool) -> None:
    """Hand the worktree lock on, keep the token until its new owner is queued, then drop
    the token. Never raises: release must not turn a finished run into a failure."""
    try:
        succ = release_lock(directory, lock_hash) if lock_hash else None
        if holding:
            await_successor(directory, succ)
    except Exception as exc:
        sys.stderr.write(f"heavy: could not release the worktree lock: {exc}\n")
    finally:
        if holding:
            release_token(directory)


def _safe_state_dir() -> Path:
    try:
        return state_dir()
    except Exception:
        return Path(".")


# ------------------------------------------------------------------ other subcommands


def _read_log(since: str | None) -> list[dict]:
    rows = []
    path = state_dir() / "log.jsonl"
    if not path.exists():
        return rows
    for line in path.read_text(encoding="utf-8").splitlines():
        try:
            row = json.loads(line)
        except ValueError:
            continue
        if since and str(row.get("ts", ""))[:10] < since:
            continue
        rows.append(row)
    return rows


def _age(rec_path: Path) -> str:
    try:
        return f"{int(time.time() - rec_path.stat().st_mtime)}s"
    except OSError:
        return "?"


def _line(rec: dict, extra: str) -> str:
    argv = " ".join(str(a) for a in (rec.get("argv") or []))
    return (f"  {extra} pid={rec.get('pid')} weight={rec.get('weight')} "
            f"agent={rec.get('agent')} worktree={rec.get('worktree')} argv={argv}")


def cmd_status() -> int:
    d = state_dir()
    print(f"state: {d}  capacity: {capacity()}")
    order = _queue_order(_entries(d / "queue", reap=False), d)
    print(f"queue: {len(order)}")
    for f, rec in order:
        print(_line(rec, f"prio={_int_prio(rec.get('prio'))} age={_age(f)}"))
    held = _entries(d / "held", reap=False)
    print(f"held: {len(held)}  weight: {sum(int(r.get('weight') or 1) for _, r in held)}")
    for f, rec in held:
        print(_line(rec, f"age={_age(f)}"))
    wt = d / "wt"
    locks = sorted(wt.glob("*.lock")) if wt.is_dir() else []
    print(f"worktree locks: {len(locks)}")
    for lock in locks:
        try:
            rec = json.loads(lock.read_text(encoding="utf-8"))
        except (OSError, ValueError):
            continue
        wdir = wt / (lock.name[: -len(".lock")] + ".waiters")
        waiting = sorted(_entries(wdir, reap=False), key=lambda it: _wkey(it[0]))
        print(_line(rec, f"lock={lock.name[: -len('.lock')]} age={_age(lock)} "
                         f"waiters={len(waiting)}"))
        for f, wrec in waiting:
            print(_line(wrec, f"  waiting prio={_wkey(f)[0]} age={_age(f)}"))
    return 0


def _warm_marker(directory: Path, root: str) -> Path:
    return directory / "warm" / (worktree_hash(root) + ".json")


def _warm_running(marker: Path) -> bool:
    try:
        rec = json.loads(marker.read_text(encoding="utf-8"))
        return is_alive(rec["pid"], rec.get("ctime"))
    except (OSError, ValueError, KeyError, TypeError):
        return False


def _is_warm(argv: list[str], root: str, cwd: str, directory: Path) -> bool:
    """The registry already holds this worktree at exactly the fingerprint a run of argv
    would have. Fail-open: any doubt means not warm."""
    try:
        found = resolve_crate_how(argv, root, cwd)
        if not found:
            return False
        crate = found[0]
        inject = injected_target_dir(argv, root)
        tdir = resolve_target_dir(argv, crate, root, cwd, inject)
        return registry_hit(directory, tdir, compute_fingerprint(argv, crate, root, tdir), root)
    except Exception:
        return False


def cmd_warm(args: list[str]) -> int:
    """Pre-build a worktree at idle priority: a detached broker queued at prio 3."""
    cwd, argv, i = os.getcwd(), [], 0
    while i < len(args):
        a = args[i]
        if a == "--":
            argv = args[i + 1:]
            break
        name, eq, val = a.partition("=")
        if name == "--cwd":
            if eq:
                cwd, i = val, i + 1
            elif i + 1 < len(args):
                cwd, i = args[i + 1], i + 2
            else:
                i += 1
            continue
        i += 1
    argv = argv or list(WARM_ARGV)
    if os.environ.get("NUCLEOS_HEAVY") == "0":
        print("heavy: broker off, nothing warmed")
        return 0
    try:
        os.chdir(cwd)
    except OSError as exc:
        sys.stderr.write(f"heavy: warm: {cwd}: {exc}\n")
        return 2
    cwd = os.getcwd()
    root = worktree_root()
    directory = state_dir()
    directory.mkdir(parents=True, exist_ok=True)
    marker = _warm_marker(directory, root)
    with Mutex(directory):
        if _warm_running(marker):
            print(f"heavy: already warming {root}")
            return 0
        if _is_warm(argv, root, cwd, directory):
            print(f"heavy: already warm {root}")
            return 0
        env = dict(os.environ, NUCLEOS_HEAVY_WARM="1")
        cmd = [sys.executable, str(Path(__file__).resolve()), "--prio", str(WARM_PRIO),
               "--kind", "cargo", "--agent", "warm", "--wait-max", "86400", "--", *argv]
        kw: dict = dict(env=env, cwd=cwd, stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
                        stderr=subprocess.DEVNULL, close_fds=True)
        if os.name == "nt":
            base = 0x00000200 | 0x08000000  # CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW
            try:
                proc = subprocess.Popen(cmd, creationflags=base | 0x01000000, **kw)  # breakaway
            except OSError:
                proc = subprocess.Popen(cmd, creationflags=base, **kw)
        else:
            proc = subprocess.Popen(cmd, start_new_session=True, **kw)
        _, ctime = proc_identity(proc.pid)
        marker.parent.mkdir(parents=True, exist_ok=True)
        marker.write_text(json.dumps({"pid": proc.pid, "ctime": ctime, "worktree": root}),
                          encoding="utf-8")
    print(f"heavy: warming {root} in the background (prio {WARM_PRIO})")
    return 0


def _median(values: list[float]) -> float:
    return statistics.median(values) if values else 0.0


def _num(value) -> float:
    try:
        return float(value)
    except (TypeError, ValueError):
        return 0.0


def cmd_report(args: list[str]) -> int:
    since = None
    if "--since" in args and args.index("--since") + 1 < len(args):
        since = args[args.index("--since") + 1]
    rows = _read_log(since)
    print(f"runs: {len(rows)}" + (f"  since: {since}" if since else ""))

    def agent_class(r: dict) -> str:
        agent = r.get("agent")
        if not agent:
            return "none"
        return "main" if agent == "main" else "subagent"

    def counts(title: str, keys: list[str]) -> None:
        print(title)
        tally: dict[str, int] = {}
        for k in keys:
            tally[k] = tally.get(k, 0) + 1
        for k in sorted(tally):
            print(f"  {k}: {tally[k]}")

    counts("by agent", [agent_class(r) for r in rows])
    counts("by kind", [str(r.get("kind")) for r in rows])
    print("outcomes")
    print(f"  exit 75: {sum(1 for r in rows if r.get('exit') == EXIT_QUEUE_TIMEOUT)}")
    print(f"  broker_error: {sum(1 for r in rows if r.get('broker_error'))}")
    print(f"  fp_hit: {sum(1 for r in rows if r.get('fp_hit'))}")
    print(f"  fp_miss: {sum(1 for r in rows if r.get('fp_miss'))}")
    print(f"  warm: {sum(1 for r in rows if r.get('warm'))}")
    print("compiled per worktree")
    per: dict[str, int] = {}
    for r in rows:
        wt = str(r.get("worktree"))
        per[wt] = per.get(wt, 0) + (1 if r.get("compiled") else 0)
    for wt in sorted(per):
        print(f"  {wt}: {per[wt]}")
    print("median")
    for field in ("wait_lock_s", "wait_token_s", "run_s"):
        print(f"  {field}: {_median([_num(r.get(field)) for r in rows]):.1f}")
    return 0


def main(argv: list[str]) -> int:
    try:
        if argv and argv[0] == "status":
            return cmd_status()
        if argv and argv[0] == "report":
            return cmd_report(argv[1:])
        if argv and argv[0] == "warm":
            return cmd_warm(argv[1:])
        if argv and argv[0] == "hold-worktree":
            return broker_run(argv[1:], held=True)
        return broker_run(argv)
    except KeyboardInterrupt:
        return 130


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
