#!/usr/bin/env python3
"""Cost model and shadow admission for the machine-wide build broker.

Standard library only, and fail-open: heavy.py reaches this module through a guarded import,
and nothing here may raise into the broker. Admission is only OBSERVED (shadow mode): the
static capacity rule in heavy.py stays the one that decides.
"""

from __future__ import annotations

import json
import math
import os
import statistics
import time
from pathlib import Path

COST_VERSION = 1

# (tool, sub) -> (cores, mem_mb)
DEFAULTS: dict[tuple[str, str], tuple[float, float]] = {
    ("cargo", "test"): (2, 3072),
    ("cargo", "build"): (2, 3072),
    ("cargo", "nextest"): (5, 3072),
    ("cargo", "check"): (1.5, 2048),
    ("cargo", "clippy"): (1.5, 2048),
    ("npm", "test"): (8, 2048),
    ("npx", "vitest"): (4, 1536),
    ("npm", "exec:vitest"): (4, 1536),
    ("npx", "tsc"): (1.5, 1024),
    ("npm", "exec:tsc"): (1.5, 1024),
    ("go", "*"): (1, 512),
}

VALUE_FLAGS = frozenset(
    "-p --package --manifest-path --target-dir -j --jobs --features -F --config -Z "
    "--color --prefix -C --profile".split()
)
_SCRIPT_EXT = (".sh", ".py", ".ps1", ".mjs", ".js")
_PKG_TOOLS = ("cargo", "npm", "npx", "go")


# ----------------------------------------------------------------------------- class key


def _tool(argv: list[str]) -> str:
    name = Path(str(argv[0])).name.lower() if argv else "-"
    for ext in (".exe", ".cmd", ".bat"):
        if name.endswith(ext):
            name = name[: -len(ext)]
            break
    # One interpreter, one class: `python3` (macOS, Linux, a CI runner) and `python3.11` cost the
    # same as this machine's `python`, and would otherwise split one history across three keys.
    if name.startswith("python3"):
        return "python"
    return name


def _positionals(args: list[str]) -> tuple[list[str], dict[str, list[str]]]:
    """Positional words of `args`, and the values of every VALUE_FLAGS flag, by flag."""
    pos: list[str] = []
    vals: dict[str, list[str]] = {}
    i = 0
    while i < len(args):
        a = str(args[i])
        if a.startswith("+") and not pos:
            pass
        elif a.startswith("-") and a != "-":
            if a in VALUE_FLAGS:
                if i + 1 < len(args):
                    vals.setdefault(a, []).append(str(args[i + 1]))
                i += 1
            elif "=" in a and a.split("=", 1)[0] in VALUE_FLAGS:
                vals.setdefault(a.split("=", 1)[0], []).append(a.split("=", 1)[1])
        else:
            pos.append(a)
        i += 1
    return pos, vals


def _clean(path: str) -> str:
    s = str(path).replace("\\", "/")
    while len(s) > 1 and s.endswith("/"):
        s = s[:-1]
    return s or "-"


def class_key(argv: list[str], filtered: bool = False, compiled: bool = True) -> str:
    tool = _tool(argv)
    pos, vals = _positionals([str(a) for a in argv[1:]])
    scope = "-"
    if tool in _PKG_TOOLS:
        sub = pos[0] if pos else "-"
        if tool in ("npm", "npx") and sub in ("exec", "run"):
            rest = [w for w in pos[1:] if w != "--"]
            sub = "exec:" + rest[0] if rest else "exec:-"
        if tool == "cargo":
            pkgs = sorted(vals.get("-p", []) + vals.get("--package", []))
            if "--workspace" in argv or "--all" in argv:  # --all: cargo's older spelling
                scope = "ws"
            elif pkgs:
                scope = "+".join(pkgs)
        elif tool == "npm":
            if vals.get("--prefix"):
                scope = _clean(vals["--prefix"][0])
        elif tool == "go":
            if vals.get("-C"):
                scope = _clean(vals["-C"][0])
    else:
        first = pos[0] if pos else ""
        base = first.replace("\\", "/").rsplit("/", 1)[-1]
        sub = base if base.lower().endswith(_SCRIPT_EXT) else "-"
    f = "f" if filtered else "a"
    c = "n" if (tool == "cargo" and not compiled) else "c"
    return f"{tool}|{sub}|{scope}|{f}|{c}"


def default_cost(cls: str, weight: int = 1) -> tuple[float, float]:
    parts = str(cls).split("|")
    tool = parts[0] if parts else ""
    sub = parts[1] if len(parts) > 1 else "-"
    for key in ((tool, sub), (tool, "*")):
        if key in DEFAULTS:
            c, m = DEFAULTS[key]
            return (float(c), float(m))
    w = max(1, int(weight or 1))
    return (2.0 * w, 2048.0 * w)


# ----------------------------------------------------------------------------- measurement


def _p90(values: list[float]) -> float:
    s = sorted(values)
    return s[max(0, math.ceil(0.9 * len(s)) - 1)]


def cores_p90(trace: list[tuple[float, float]], window_s: float = 2.0) -> float | None:
    try:
        if not trace:
            return None
        t_a, cpu_a = trace[0]
        rates: list[float] = []
        for t, cpu in trace[1:]:
            if t - t_a >= window_s:
                rates.append((cpu - cpu_a) / (t - t_a))
                t_a, cpu_a = t, cpu
        if not rates:
            return None
        return round(_p90(rates), 2)
    except Exception:
        return None


def job_peak_mem_mb(job) -> float | None:
    if os.name != "nt" or not job:
        return None
    try:
        import ctypes
        from ctypes import wintypes

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

        k32 = ctypes.WinDLL("kernel32", use_last_error=True)
        k32.QueryInformationJobObject.argtypes = [
            wintypes.HANDLE, ctypes.c_int, ctypes.c_void_p, wintypes.DWORD, ctypes.c_void_p]
        k32.QueryInformationJobObject.restype = wintypes.BOOL
        info = EXT()
        if not k32.QueryInformationJobObject(job, 9, ctypes.byref(info), ctypes.sizeof(info), None):
            return None
        peak = info.PeakJobMemoryUsed
        if not peak:
            return None
        return round(peak / 2**20, 1)
    except Exception:
        return None


def measure_fields(stats: dict, run_s: float, job) -> dict:
    try:
        cpu_s = (stats or {}).get("cpu_s")
        avg = None
        if cpu_s and run_s and cpu_s > 0 and run_s > 0:
            avg = round(cpu_s / run_s, 2)
        return {
            "peak_mem_mb": job_peak_mem_mb(job),
            "cores_avg": avg,
            "cores_p90": cores_p90((stats or {}).get("cpu_trace") or []),
        }
    except Exception:
        return {"peak_mem_mb": None, "cores_avg": None, "cores_p90": None}


# ----------------------------------------------------------------------------- cost model


def _ts_epoch(ts) -> float | None:
    from datetime import datetime

    try:
        return datetime.strptime(str(ts), "%Y-%m-%dT%H:%M:%S%z").timestamp()
    except Exception:
        return None


def _num(v) -> bool:
    return isinstance(v, (int, float)) and not isinstance(v, bool)


def build_costs(rows: list[dict], now: float | None = None, n: int = 20,
                min_run_s: float = 20.0, days: float = 30.0, min_n: int = 3) -> dict:
    now = time.time() if now is None else now
    groups: dict[str, list[dict]] = {}
    for r in rows:
        if not isinstance(r, dict) or not isinstance(r.get("cls"), str):
            continue
        run_s = r.get("run_s")
        if not _num(run_s) or run_s < min_run_s:
            continue
        t = _ts_epoch(r.get("ts"))
        if t is None or now - t > days * 86400:
            continue
        groups.setdefault(r["cls"], []).append(r)
    classes: dict[str, dict] = {}
    for cls, grp in groups.items():
        grp = grp[-n:]
        if len(grp) < min_n:
            continue
        d_cores, d_mem = default_cost(cls)
        cs = [r["cores_p90"] for r in grp if _num(r.get("cores_p90"))]
        ms = [r["peak_mem_mb"] for r in grp if _num(r.get("peak_mem_mb"))]
        classes[cls] = {
            "cores": _p90(cs) if cs else d_cores,
            "mem_mb": _p90(ms) if ms else d_mem,
            "n": len(grp),
        }
    return {"v": COST_VERSION, "built": time.strftime("%Y-%m-%dT%H:%M:%S%z"), "classes": classes}


def load_costs(directory: Path) -> dict:
    try:
        data = json.loads((Path(directory) / "costs.json").read_text(encoding="utf-8"))
        return data if isinstance(data, dict) else {}
    except Exception:
        return {}


def rebuild_costs(directory: Path, max_age_s: float = 600.0, tail_bytes: int = 4_000_000) -> None:
    try:
        d = Path(directory)
        target = d / "costs.json"
        try:
            if time.time() - target.stat().st_mtime < max_age_s:
                return
        except OSError:
            pass
        log = d / "log.jsonl"
        if not log.exists():
            return
        with open(log, "rb") as fh:
            size = fh.seek(0, os.SEEK_END)
            seeked = size > tail_bytes
            fh.seek(max(0, size - tail_bytes))
            raw = fh.read()
        lines = raw.decode("utf-8", errors="replace").splitlines()
        if seeked and lines:
            lines = lines[1:]
        rows = []
        for line in lines:
            try:
                rows.append(json.loads(line))
            except ValueError:
                continue
        costs = build_costs(rows)
        tmp = d / "costs.json.tmp"
        tmp.write_text(json.dumps(costs), encoding="utf-8")
        os.replace(tmp, target)
    except Exception:
        return


# ----------------------------------------------------------------------------- light lane
#
# A class that has proven small (a few hundred MB, seconds of CPU) may start while the token
# budget is full. It uses its own model, `light.json`, so `costs.json` keeps its format: an old
# broker reads the same state dir and simply ignores the file.

LIGHT_VERSION = 1
LIGHT_MEM_MB = 1536   # p90 peak memory
LIGHT_RUN_S = 180     # p90 run time
LIGHT_CORES = 3       # p90 of the per-run average cores
LIGHT_MIN_N = 5       # runs needed before a class is trusted
LIGHT_MAX_DEFAULT = 2
LIGHT_MARGIN_MB_DEFAULT = 1024


def light_enabled() -> bool:
    return os.environ.get("NUCLEOS_HEAVY_LIGHT", "").strip() != "0"


def light_max() -> int:
    """Light runs allowed at once (NUCLEOS_HEAVY_LIGHT_MAX); an unreadable value is the default."""
    try:
        return max(0, int(os.environ.get("NUCLEOS_HEAVY_LIGHT_MAX", "").strip()))
    except ValueError:
        return LIGHT_MAX_DEFAULT


def light_margin_mb() -> int:
    """Memory that must stay available on top of a light run's own p90."""
    return int(_env_pos("NUCLEOS_HEAVY_LIGHT_MARGIN_MB", LIGHT_MARGIN_MB_DEFAULT))


def _is_light(n, mem, run, cores, min_n: int = LIGHT_MIN_N) -> bool:
    return (_num(n) and _num(mem) and _num(run) and _num(cores)
            and n >= min_n and mem <= LIGHT_MEM_MB and run <= LIGHT_RUN_S
            and cores <= LIGHT_CORES)


def build_light(rows: list[dict], now: float | None = None, n: int = 20,
                min_n: int = LIGHT_MIN_N, days: float = 30.0) -> dict:
    """Per class, over its last `n` complete rows of any duration: p90 memory, run time and
    average cores, and whether they are small enough to run beside a full budget."""
    now = time.time() if now is None else now
    groups: dict[str, list[dict]] = {}
    for r in rows:
        if not isinstance(r, dict) or not isinstance(r.get("cls"), str):
            continue
        if not (_num(r.get("peak_mem_mb")) and _num(r.get("cores_avg"))
                and _num(r.get("run_s"))):
            continue
        t = _ts_epoch(r.get("ts"))
        if t is None or now - t > days * 86400:
            continue
        groups.setdefault(r["cls"], []).append(r)
    classes: dict[str, dict] = {}
    for cls, grp in groups.items():
        grp = grp[-n:]
        mem = _p90([r["peak_mem_mb"] for r in grp])
        run = _p90([r["run_s"] for r in grp])
        cores = _p90([r["cores_avg"] for r in grp])
        classes[cls] = {"n": len(grp), "mem_mb": mem, "run_s": run, "cores": cores,
                        "light": _is_light(len(grp), mem, run, cores, min_n)}
    return {"v": LIGHT_VERSION, "built": time.strftime("%Y-%m-%dT%H:%M:%S%z"), "classes": classes}


def load_light(directory: Path) -> dict:
    try:
        data = json.loads((Path(directory) / "light.json").read_text(encoding="utf-8"))
        return data if isinstance(data, dict) else {}
    except Exception:
        return {}


def rebuild_light(directory: Path, max_age_s: float = 600.0, tail_bytes: int = 4_000_000) -> None:
    """Rewrite light.json from the log's tail, at most every `max_age_s`. Never raises, and
    never touches costs.json."""
    try:
        d = Path(directory)
        target = d / "light.json"
        try:
            if time.time() - target.stat().st_mtime < max_age_s:
                return
        except OSError:
            pass
        log = d / "log.jsonl"
        if not log.exists():
            return
        with open(log, "rb") as fh:
            size = fh.seek(0, os.SEEK_END)
            seeked = size > tail_bytes
            fh.seek(max(0, size - tail_bytes))
            raw = fh.read()
        lines = raw.decode("utf-8", errors="replace").splitlines()
        if seeked and lines:
            lines = lines[1:]
        rows = []
        for line in lines:
            try:
                rows.append(json.loads(line))
            except ValueError:
                continue
        tmp = d / "light.json.tmp"
        tmp.write_text(json.dumps(build_light(rows)), encoding="utf-8")
        os.replace(tmp, target)
    except Exception:
        return


def light_entry(light: dict, cls: str) -> dict | None:
    """The class's entry when its own numbers say light. The stored flag is not trusted: a
    hand-edited or older file must not admit a class the thresholds would refuse."""
    try:
        e = ((light or {}).get("classes") or {}).get(cls)
        if isinstance(e, dict) and _is_light(
                e.get("n"), e.get("mem_mb"), e.get("run_s"), e.get("cores")):
            return e
    except Exception:
        pass
    return None


def light_fits(entry: dict, p: dict, margin: float | None = None) -> tuple[bool, dict]:
    """Room for one light run now: its memory plus a margin is available, and the CPUs
    already busy leave it its cores with one to spare."""
    margin = light_margin_mb() if margin is None else margin
    avail = float(p.get("avail_mb") or 0)
    busy = p.get("busy_cores")
    need_mem = float(entry["mem_mb"]) + margin
    ok = avail >= need_mem
    if ok and _num(busy):
        ok = busy + float(entry["cores"]) <= float(p.get("logical") or 1) - 1
    return ok, {
        "avail_mb": round(avail, 1), "need_mem_mb": round(need_mem, 1),
        "busy_cores": round(busy, 1) if _num(busy) else None, "cores": entry.get("cores"),
    }


def predict(costs: dict, cls: str, weight: int = 1) -> tuple[float, float]:
    try:
        e = (costs or {}).get("classes", {}).get(cls)
        if isinstance(e, dict) and _num(e.get("cores")) and _num(e.get("mem_mb")):
            return (e["cores"], e["mem_mb"])
        # A workspace run builds every member, so an unseen one is predicted as the largest
        # known class of the same tool, subcommand and compiled flag (measured 2026-10-22: the
        # first `cargo check --workspace` got the static 2 GB and peaked at 6 GB).
        tool, sub, scope, _, comp = (str(cls).split("|") + [""] * 5)[:5]
        if scope == "ws":
            same = []
            for k, v in (costs or {}).get("classes", {}).items():
                kp = str(k).split("|")
                if (len(kp) == 5 and (kp[0], kp[1], kp[4]) == (tool, sub, comp)
                        and isinstance(v, dict) and _num(v.get("cores"))
                        and _num(v.get("mem_mb"))):
                    same.append(v)
            if same:
                return (max(v["cores"] for v in same), max(v["mem_mb"] for v in same))
    except Exception:
        pass
    return default_cost(cls, weight)


# ----------------------------------------------------------------------------- admission


def _env_pos(name: str, default: float) -> float:
    try:
        v = float(os.environ.get(name, ""))
        return v if v > 0 else default
    except ValueError:
        return default


def admission_mode() -> str:
    if os.environ.get("NUCLEOS_HEAVY_CAPACITY", "").strip():
        return "static"
    if os.environ.get("NUCLEOS_HEAVY_ADMISSION", "").strip().lower() == "static":
        return "static"
    return "shadow"


_K32 = None
_PREV_TIMES: tuple[float, int, int] | None = None  # (monotonic, idle, total)
# Wait between a process's first two samples; the probes need >= 0.2 s between them.
PRIME_S = 0.25


def _win_probe(logical: int) -> dict:
    global _K32, _PREV_TIMES
    import ctypes
    from ctypes import wintypes

    if _K32 is None:
        _K32 = ctypes.WinDLL("kernel32", use_last_error=True)
    k32 = _K32

    class MEMSTAT(ctypes.Structure):
        _fields_ = [
            ("dwLength", wintypes.DWORD),
            ("dwMemoryLoad", wintypes.DWORD),
            ("ullTotalPhys", ctypes.c_uint64),
            ("ullAvailPhys", ctypes.c_uint64),
            ("ullTotalPageFile", ctypes.c_uint64),
            ("ullAvailPageFile", ctypes.c_uint64),
            ("ullTotalVirtual", ctypes.c_uint64),
            ("ullAvailVirtual", ctypes.c_uint64),
            ("ullAvailExtendedVirtual", ctypes.c_uint64),
        ]

    ms = MEMSTAT()
    ms.dwLength = ctypes.sizeof(MEMSTAT)
    k32.GlobalMemoryStatusEx.argtypes = [ctypes.c_void_p]
    k32.GlobalMemoryStatusEx.restype = wintypes.BOOL
    if not k32.GlobalMemoryStatusEx(ctypes.byref(ms)):
        raise OSError("GlobalMemoryStatusEx failed")

    idle, kern, user = (wintypes.FILETIME(), wintypes.FILETIME(), wintypes.FILETIME())
    k32.GetSystemTimes.argtypes = [ctypes.c_void_p] * 3
    k32.GetSystemTimes.restype = wintypes.BOOL
    busy = None
    if k32.GetSystemTimes(ctypes.byref(idle), ctypes.byref(kern), ctypes.byref(user)):
        def ft(x) -> int:
            return (x.dwHighDateTime << 32) | x.dwLowDateTime

        i, total = ft(idle), ft(kern) + ft(user)  # kernel time includes idle
        now = time.monotonic()
        prev = _PREV_TIMES
        _PREV_TIMES = (now, i, total)
        if prev is not None and now - prev[0] >= 0.2 and total > prev[2]:
            dt, di = total - prev[2], i - prev[1]
            busy = round(logical * (dt - di) / dt, 2)
    return {
        "logical": logical,
        "total_mb": ms.ullTotalPhys / 2**20,
        "avail_mb": ms.ullAvailPhys / 2**20,
        "busy_cores": busy,
    }


_PREV_STAT: tuple[float, int, int] | None = None


def _posix_probe(logical: int) -> dict:
    global _PREV_STAT
    mem: dict[str, float] = {}
    for line in Path("/proc/meminfo").read_text().splitlines():
        k, _, v = line.partition(":")
        if k in ("MemTotal", "MemAvailable"):
            mem[k] = float(v.split()[0]) / 1024.0
    busy = None
    try:
        f = Path("/proc/stat").read_text().splitlines()[0].split()[1:]
        vals = [int(x) for x in f]
        idle = vals[3] + (vals[4] if len(vals) > 4 else 0)
        total = sum(vals[:8])
        now = time.monotonic()
        prev = _PREV_STAT
        _PREV_STAT = (now, idle, total)
        if prev is not None and now - prev[0] >= 0.2 and total > prev[2]:
            busy = round(logical * ((total - prev[2]) - (idle - prev[1])) / (total - prev[2]), 2)
    except Exception:
        busy = None
    return {
        "logical": logical,
        "total_mb": mem["MemTotal"],
        "avail_mb": mem["MemAvailable"],
        "busy_cores": busy,
    }


def probe() -> dict:
    fake = os.environ.get("NUCLEOS_HEAVY_FAKE_PROBE")
    if fake:
        return json.loads(fake)
    logical = os.cpu_count() or 1
    if os.name == "nt":
        return _win_probe(logical)
    return _posix_probe(logical)


def budget(p: dict) -> tuple[float, float, float]:
    total = float(p.get("total_mb") or 0)
    reserve_cpu = _env_pos("NUCLEOS_HEAVY_RESERVE_CPU", 2.0)
    reserve_mem = _env_pos("NUCLEOS_HEAVY_RESERVE_MEM_MB", max(3072.0, 0.2 * total))
    return (float(p.get("logical") or 1) - reserve_cpu, total - reserve_mem, reserve_mem)


def fits(held_recs: list[dict], head_rec: dict, p: dict, costs: dict) -> tuple[bool, dict]:
    def pred_of(r: dict) -> tuple[float, float]:
        if _num(r.get("pred_cores")) and _num(r.get("pred_mem_mb")):
            return (r["pred_cores"], r["pred_mem_mb"])
        return predict(costs, r.get("cls") or "", int(r.get("weight") or 1))

    b_cpu, b_mem, reserve_mem = budget(p)
    total = float(p.get("total_mb") or 0)
    avail = float(p.get("avail_mb") or 0)
    busy = p.get("busy_cores")
    preds = [pred_of(r) for r in held_recs]
    sum_c = sum(c for c, _ in preds)
    sum_m = sum(m for _, m in preds)
    head_c, head_m = pred_of(head_rec)
    ext_c = max(0.0, busy - sum_c) if _num(busy) else 0.0
    ext_m = max(0.0, (total - avail) - sum_m)
    need_c = sum_c + head_c + ext_c
    need_m = sum_m + head_m + ext_m
    if not held_recs:
        ok, rule = True, "alone"
    elif avail < reserve_mem:
        ok, rule = False, "mem_guard"
    else:
        ok, rule = (need_c <= b_cpu and need_m <= b_mem), "budget"
    detail = {
        "rule": rule,
        "need_cpu": round(need_c, 1),
        "b_cpu": round(b_cpu, 1),
        "need_mem_mb": round(need_m, 1),
        "b_mem_mb": round(b_mem, 1),
        "busy_cores": round(busy, 1) if _num(busy) else None,
        "avail_mb": round(avail, 1),
    }
    return ok, detail


# ----------------------------------------------------------------------------- jsonl


def append_jsonl(path: Path, row: dict) -> None:
    try:
        path = Path(path)
        path.parent.mkdir(parents=True, exist_ok=True)
        data = (json.dumps(row) + "\n").encode("utf-8")
        fd = os.open(str(path), os.O_WRONLY | os.O_APPEND | os.O_CREAT | getattr(os, "O_BINARY", 0), 0o666)
        try:
            os.write(fd, data)
        finally:
            os.close(fd)
    except Exception:
        return


def read_jsonl(path: Path, since: str | None = None) -> list[dict]:
    rows: list[dict] = []
    try:
        path = Path(path)
        if not path.exists():
            return rows
        for line in path.read_text(encoding="utf-8").splitlines():
            try:
                row = json.loads(line)
            except ValueError:
                continue
            if not isinstance(row, dict):
                continue
            if since and str(row.get("ts", ""))[:10] < since:
                continue
            rows.append(row)
    except Exception:
        pass
    return rows


# ----------------------------------------------------------------------------- shadow


class Shadow:
    """Watches one queued request's admission and records where the cost model would differ."""

    def __init__(self, rec: dict, directory) -> None:
        self.p = None
        self.rec = rec
        self.directory = directory
        self.costs: dict = {}
        self.t_head: float | None = None
        self.head_polls = 0
        self.disagree_polls = 0
        self.first_disagreement = None
        self.shadow_fit_at: float | None = None
        self.static_fit_at: float | None = None
        self.primed = False
        try:
            self.costs = load_costs(Path(directory))
        except Exception:
            self.costs = {}

    def tick(self) -> None:
        try:
            self.p = probe()
            if self.p.get("busy_cores") is None and not self.primed:
                # busy_cores is a difference of two samples, and a fresh process has only
                # one: without a second, the first fits() counted no outside CPU at all.
                self.primed = True
                time.sleep(PRIME_S)
                self.p = probe()
        except Exception:
            self.p = None

    def observe(self, held_recs: list[dict], static_fit: bool) -> None:
        try:
            if self.p is None:
                return
            now = time.monotonic()
            if self.t_head is None:
                self.t_head = now
            self.head_polls += 1
            ok, d = fits(held_recs, self.rec, self.p, self.costs)
            if ok and self.shadow_fit_at is None:
                self.shadow_fit_at = now
            if static_fit and self.static_fit_at is None:
                self.static_fit_at = now
            if bool(ok) != bool(static_fit):
                self.disagree_polls += 1
                if self.first_disagreement is None:
                    self.first_disagreement = dict(d, static=bool(static_fit), shadow=bool(ok))
        except Exception:
            return

    def finish(self, admitted: bool) -> None:
        try:
            if self.head_polls == 0:
                return
            t0 = self.t_head or 0.0

            def wait(at):
                return None if at is None else round(at - t0, 3)

            rec = self.rec
            append_jsonl(Path(self.directory) / "admission.jsonl", {
                "v": 1,
                "ts": time.strftime("%Y-%m-%dT%H:%M:%S%z"),
                "pid": os.getpid(),
                "cls": rec.get("cls"),
                "weight": rec.get("weight"),
                "pred_cores": rec.get("pred_cores"),
                "pred_mem_mb": rec.get("pred_mem_mb"),
                "admitted": admitted,
                "head_polls": self.head_polls,
                "disagree_polls": self.disagree_polls,
                "agree": self.disagree_polls == 0,
                "static_wait_s": wait(self.static_fit_at),
                "shadow_wait_s": wait(self.shadow_fit_at),
                "first_disagreement": self.first_disagreement,
            })
        except Exception:
            return


# ----------------------------------------------------------------------------- report


def _median_err(pairs: list[tuple[float, float]]) -> float | None:
    return statistics.median(abs(a - b) for a, b in pairs) if pairs else None


def report_lines(rows: list[dict], adm_rows: list[dict]) -> list[str]:
    out = ["admission (shadow)"]
    adm = [r for r in adm_rows if isinstance(r, dict)]
    if not adm:
        out.append("  no shadow decisions")
    else:
        agree = sum(1 for r in adm if r.get("agree"))
        out.append(f"  decisions: {len(adm)}  agreement: {100 * agree / len(adm):.0f}%")
        per: dict[str, list[int]] = {}
        for r in adm:
            c = per.setdefault(str(r.get("cls")), [0, 0])
            c[1] += 1
            if not r.get("agree"):
                c[0] += 1
        for cls in sorted(per):
            if per[cls][0]:
                out.append(f"  {cls}: {per[cls][0]}/{per[cls][1]} differ")
    out.append("cost classes (prediction error)")
    by: dict[str, list[dict]] = {}
    for r in rows:
        if isinstance(r, dict) and isinstance(r.get("cls"), str):
            by.setdefault(r["cls"], []).append(r)
    if not by:
        out.append("  no measured runs")
    for cls in sorted(by):
        grp = by[cls]
        cs = [r["cores_p90"] for r in grp if _num(r.get("cores_p90"))]
        ms = [r["peak_mem_mb"] for r in grp if _num(r.get("peak_mem_mb"))]
        ec = _median_err([(r["pred_cores"], r["cores_p90"]) for r in grp
                          if _num(r.get("pred_cores")) and _num(r.get("cores_p90"))])
        em = _median_err([(r["pred_mem_mb"], r["peak_mem_mb"]) for r in grp
                          if _num(r.get("pred_mem_mb")) and _num(r.get("peak_mem_mb"))])

        def f(v, spec):
            return "-" if v is None else format(v, spec)

        out.append(
            f"  {cls}: n={len(grp)} cores_p90={f(_p90(cs) if cs else None, '.1f')} "
            f"err_cores={f(ec, '.1f')} mem_mb={f(_p90(ms) if ms else None, '.0f')} "
            f"err_mem_mb={f(em, '.0f')}"
        )
    return out
