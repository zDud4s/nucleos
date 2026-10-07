"""Live, read-only view of the heavy-command broker (scripts/heavy.py).

    python scripts/heavy_watch.py [--interval 2]

One block per running job, numbered #1, #2...: its phases (prepare -> compile -> test)
with the current one marked, cargo's own progress as the broker reads it from the job's
output (units compiled out of the total, then the test binary and tests run), the target
slot it leased and the rustc processes it spawned. Queued jobs show how long the same
command took before, from the broker's log. The slot list names jobs by the same number;
rustc outside any broker job is listed apart. Never reaps, never writes: it reads the state
files `heavy.py status` reads. Ctrl+C to leave.
"""
from __future__ import annotations

import argparse
import json
import os
import shutil
import statistics
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import heavy  # noqa: E402

try:
    import psutil
except ImportError:  # the broker view still works without the process panels
    psutil = None

RESET, DIM, BOLD = "\x1b[0m", "\x1b[2m", "\x1b[1m"
GREEN, YELLOW, RED, CYAN = "\x1b[32m", "\x1b[33m", "\x1b[31m", "\x1b[36m"
# Prefixes a job's rustc line with its rank, so `fit` can drop the least useful ones first
# when the frame is taller than the terminal; it never reaches the screen.
RUSTC_MARK = "\x1f"
# `cargo clippy` compiles through clippy-driver, not rustc.
COMPILERS = {"rustc.exe", "clippy-driver.exe", "rustc", "clippy-driver"}
# One colour per job number, reused wherever that job is named (its block, the slot list).
JOB_COLOURS = ["\x1b[36m", "\x1b[35m", "\x1b[34m", "\x1b[33m"]


def short_wt(path) -> str:
    """The worktree's name. A held record carries the directory the command ran from (often
    `<worktree>/core`), so walk up to the checkout root: the slot list names the root."""
    if not path:
        return "-"
    p = Path(str(path))
    for c in (p, *p.parents) if p.is_absolute() else ():
        if (c / ".git").exists():
            return c.name
    return p.name or str(path)


def short_argv(rec: dict, width: int = 60) -> str:
    argv = " ".join(str(a) for a in (rec.get("argv") or []))
    argv = argv.replace("C:\\Users\\PC Multimedia\\AppData\\Local\\Programs\\Python\\Python311\\", "")
    return argv if len(argv) <= width else argv[: width - 1] + "…"


def age_s(path: Path, rec: dict) -> int:
    try:
        if isinstance(rec.get("acquired"), (int, float)):
            return max(0, int(time.time() - rec["acquired"]))
        return int(time.time() - path.stat().st_mtime)
    except OSError:
        return 0


def fmt_age(s: float) -> str:
    s = int(s)
    if s < 60:
        return f"{s}s"
    if s < 3600:
        return f"{s // 60}m{s % 60:02d}"
    if s < 86400:
        return f"{s // 3600}h{s % 3600 // 60:02d}"
    return f"{s // 86400}d"


def bar(used: int, total: int) -> str:
    cells = "".join(
        (RED if used >= total else YELLOW) + "█" + RESET if i < used else DIM + "░" + RESET
        for i in range(total))
    return f"[{cells}] {used}/{total}"


def meter(done: int, total: int, width: int = 24) -> str:
    frac = min(1.0, done / total) if total else 0.0
    filled = int(frac * width)
    return (f"[{CYAN}{'█' * filled}{RESET}{DIM}{'░' * (width - filled)}{RESET}] "
            f"{int(frac * 100):>3}%")


def colour_of(n: int) -> str:
    return JOB_COLOURS[(n - 1) % len(JOB_COLOURS)]


def tag(n: int) -> str:
    return f"{colour_of(n)}{BOLD}#{n}{RESET}"


# ------------------------------------------------------------------ history

# The broker log only grows; re-parse it when its size changes, not every frame.
_history_cache: dict = {"size": -1, "rows": []}


def history() -> list[dict]:
    try:
        size = (heavy.state_dir() / "log.jsonl").stat().st_size
    except OSError:
        return []
    if size != _history_cache["size"]:
        _history_cache["size"], _history_cache["rows"] = size, heavy._read_log(None)
    return _history_cache["rows"]


def estimate(rec: dict) -> str:
    """How long the same command took before: median and spread of the last runs in the
    same worktree when there are at least two (an incremental build there is the closest
    match), else of the last runs anywhere. Runs that never started (exit 75) are left out.
    The spread is shown because it is wide: a full rebuild and an incremental one share an
    argv."""
    key = [str(a) for a in (rec.get("argv") or [])]
    rows = [r for r in history() if r.get("argv") == key
            and r.get("exit") != heavy.EXIT_QUEUE_TIMEOUT
            and isinstance(r.get("run_s"), (int, float)) and r["run_s"] > 0]
    wt = heavy._real(rec.get("worktree") or "")
    here = [r for r in rows if heavy._real(r.get("worktree") or "") == wt][-5:]
    runs, where = (here, "this worktree") if len(here) >= 2 else (rows[-15:], "any worktree")
    vals = sorted(float(r["run_s"]) for r in runs)
    if not vals:
        return f"{DIM}unknown (never ran){RESET}"
    med = statistics.median(vals)
    if len(vals) >= 4:
        lo, _, hi = statistics.quantiles(vals, n=4)
    else:
        lo, hi = vals[0], vals[-1]
    spread = f"{fmt_age(lo)}–{fmt_age(hi)}, " if hi - lo >= 5 else ""
    return f"~{fmt_age(med)}  {DIM}({spread}{where}, n={len(vals)}){RESET}"


# ------------------------------------------------------------------ sessions

# session id -> (checked at, title); a title changes rarely, a transcript can be 100+ MB.
_titles: dict[str, tuple[float, str]] = {}
_TITLE_TYPES = ("custom-title", "ai-title")


def _claude_projects() -> Path:
    base = os.environ.get("CLAUDE_CONFIG_DIR")
    return (Path(base) if base else Path.home() / ".claude") / "projects"


def _title_in(lines: list[bytes]) -> str | None:
    """The newest title among transcript lines: a name given with /rename beats the one
    Claude Code generates, which is what the VS Code session list shows otherwise."""
    found: dict[str, str] = {}
    for raw in lines:
        if b"-title" not in raw:
            continue
        try:
            rec = json.loads(raw)
        except ValueError:
            continue
        kind = rec.get("type")
        if kind in _TITLE_TYPES:
            title = rec.get("customTitle") or rec.get("aiTitle") or rec.get("title")
            if isinstance(title, str) and title.strip():
                found[kind] = title.strip()
    return next((found[k] for k in _TITLE_TYPES if k in found), None)


def _first_prompt(head: list[bytes]) -> str | None:
    for raw in head:
        try:
            rec = json.loads(raw)
        except ValueError:
            continue
        if rec.get("type") != "user":
            continue
        content = (rec.get("message") or {}).get("content")
        if isinstance(content, list):
            content = next((c.get("text") for c in content
                            if isinstance(c, dict) and c.get("type") == "text"), None)
        if isinstance(content, str) and content.strip() and not content.startswith("<"):
            return content.strip().splitlines()[0]
    return None


def session_title(sid: str) -> str | None:
    hit = _titles.get(sid)
    if hit and time.time() - hit[0] < 30:
        return hit[1] or None
    title = None
    for path in _claude_projects().glob(f"*/{sid}.jsonl"):
        try:
            with open(path, "rb") as f:
                size = f.seek(0, 2)
                f.seek(max(0, size - 2**21))
                title = _title_in(f.read().splitlines())
                if not title:
                    f.seek(0)
                    head = f.read(2**18).splitlines()
                    title = _title_in(head) or _first_prompt(head)
        except OSError:
            continue
        if title:
            break
    _titles[sid] = (time.time(), title or "")
    return title


def session_label(rec: dict, width: int = 56) -> str:
    """Who asked for this job: the Claude Code session's title and short id, marked when a
    subagent of it ran the command; or where it came from when no session did."""
    sid = rec.get("session")
    agent = rec.get("agent")
    if not sid:
        if agent:
            return f"{DIM}agent {agent}, session unknown (started before the broker recorded it){RESET}"
        return f"{DIM}no Claude session (terminal, daemon gate or CI){RESET}"
    title = session_title(str(sid)) or "untitled"
    if len(title) > width:
        title = title[: width - 1] + "…"
    sub = f"  {YELLOW}subagent {str(agent)[:8]}{RESET}" if agent and agent != "main" else ""
    return f"{BOLD}{title}{RESET}  {DIM}{str(sid)[:8]}{RESET}{sub}"


# ------------------------------------------------------------------ phases

def read_progress(pid) -> dict:
    """What the broker read from this job's own output (heavy.py `_write_progress`).
    Empty for a job that is not cargo, or that started under a broker without it."""
    try:
        return json.loads((heavy.state_dir() / "progress" / str(pid)).read_text(encoding="utf-8"))
    except (OSError, ValueError):
        return {}


def phases_of(argv: list) -> list[str]:
    args = [str(a) for a in argv]
    if not args or not Path(args[0]).stem.lower().startswith("cargo"):
        return ["run"]
    sub = next((a for a in args[1:] if not a.startswith(("-", "+"))), "")
    if sub in ("test", "t", "bench") and "--no-run" not in args:
        return ["prepare", "compile", "test"]
    return ["prepare", "compile"]


def current_phase(phases: list[str], prog: dict) -> str:
    if phases == ["run"]:
        return "run"
    if prog.get("binary") and "test" in phases:
        return "test"
    if prog.get("units_total"):
        return "compile"
    return "prepare"


def phase_line(phases: list[str], now: str) -> str:
    i = phases.index(now)
    parts = []
    for j, p in enumerate(phases):
        if j < i:
            parts.append(f"{DIM}✓ {p}{RESET}")
        elif j == i:
            parts.append(f"{BOLD}{YELLOW}▶ {p}{RESET}")
        else:
            parts.append(f"{DIM}· {p}{RESET}")
    return f" {DIM}→{RESET} ".join(parts) + f"   {DIM}{i + 1}/{len(phases)}{RESET}"


def field(pad: str, label: str, value: str) -> str:
    """One `label  value` row: the label is the dim column, the value reads at full strength."""
    return f"{pad}{DIM}{label:<9}{RESET}{value}"


# ------------------------------------------------------------------ processes

def rustc_info(p, primed: dict) -> dict | None:
    """Crate, uptime, cpu and memory of one rustc compiling a crate; None for anything
    else, cargo's `rustc -vV` probes included."""
    try:
        if p.name().lower() not in COMPILERS:
            return None
        cmd = p.cmdline()
        if "--crate-name" not in cmd:
            return None
        if p.pid not in primed:
            primed[p.pid] = p
            p.cpu_percent(None)
            cpu = None
        else:
            cpu = primed[p.pid].cpu_percent(None) / (psutil.cpu_count() or 1)
        out = cmd[cmd.index("--out-dir") + 1] if "--out-dir" in cmd else ""
        return {
            "crate": cmd[cmd.index("--crate-name") + 1] + (" (test)" if "--test" in cmd else "")
                     + (" [clippy]" if "clippy" in p.name().lower() else ""),
            "up": time.time() - p.create_time(), "cpu": cpu,
            "mb": p.memory_info().rss // 2**20,
            "target": Path(out).parent.parent.name if out else "?",
        }
    except psutil.Error:
        return None


def rustc_line(info: dict, indent: str, with_target: bool = False) -> str:
    cpu = "   …" if info["cpu"] is None else f"{info['cpu']:4.1f}%"
    target = f"  {CYAN}{info['target']}{RESET}" if with_target else ""
    return (f"{indent}{info['crate']:<32} {fmt_age(info['up']):>5}  "
            f"{cpu} {DIM}cpu{RESET}  {info['mb']:>5} {DIM}MB{RESET}{target}")


def job_tree(pid, primed: dict) -> tuple[list[dict], set[int]]:
    """The rustc this job spawned (longest-running first), and every pid in its tree."""
    if psutil is None:
        return [], set()
    try:
        kids = psutil.Process(int(pid)).children(recursive=True)
    except (psutil.Error, ValueError, TypeError):
        return [], set()
    infos = [i for i in (rustc_info(k, primed) for k in kids) if i]
    return sorted(infos, key=lambda i: -i["up"]), {k.pid for k in kids}


def stray_rustc(owned: set[int], primed: dict) -> list[dict]:
    if psutil is None:
        return []
    out = []
    for p in psutil.process_iter(["name"]):
        if p.pid in owned or (p.info["name"] or "").lower() not in COMPILERS:
            continue
        info = rustc_info(p, primed)
        if info:
            out.append(info)
    return out


# ------------------------------------------------------------------ frame

def job_block(n: int, f: Path, r: dict, slot: int | None, primed: dict,
              owned: set[int]) -> list[str]:
    pad = f"  {colour_of(n)}│{RESET} "
    slot_txt = f"pool-{slot}" if slot else "none"
    lines = [f"  {tag(n)} {BOLD}{short_wt(r.get('worktree'))}{RESET}",
             field(pad, "command", short_argv(r, 72)),
             field(pad, "session", session_label(r)),
             field(pad, "running", f"{fmt_age(age_s(f, r))}   {DIM}usually{RESET} {estimate(r)}"),
             field(pad, "slot", f"{slot_txt}   {DIM}weight {r.get('weight')}{RESET}")]

    phases = phases_of(r.get("argv") or [])
    prog = read_progress(r.get("pid"))
    now = current_phase(phases, prog)
    lines.append(field(pad, "phase", phase_line(phases, now)))

    if now == "compile":
        done, total = prog["units_done"], prog["units_total"]
        lines.append(field(pad, "progress", f"{meter(done, total)}  {done}/{total} {DIM}units{RESET}"))
    elif now == "test":
        total, done = prog.get("tests_total"), prog.get("tests_done") or 0
        if total:
            # A test that re-runs its own binary adds its child's `test ... ok` line.
            done = min(done, total)
        what = f"{DIM}binary {prog.get('binaries')}:{RESET} {prog.get('binary')}"
        if total:
            lines.append(field(pad, "progress", f"{meter(done, total)}  {done}/{total} {DIM}tests{RESET}"))
            lines.append(field(pad, "", what))
        else:
            lines.append(field(pad, "progress", f"{what} {DIM}starting…{RESET}"))

    infos, tree = job_tree(r.get("pid"), primed)
    owned |= tree
    for i, info in enumerate(infos):
        lines.append(f"{RUSTC_MARK}{i}{RUSTC_MARK}"
                     + field(pad, "rustc" if i == 0 else "", rustc_line(info, "")))
    return lines


def frame(primed: dict) -> str:
    d = heavy.state_dir()
    held = heavy._entries(d / "held", reap=False)
    used = sum(int(r.get("weight") or 1) for _, r in held)
    head = (f"{BOLD}heavy broker{RESET}  {time.strftime('%H:%M:%S')}   "
            f"capacity {bar(used, heavy.capacity())}")
    if psutil is not None:
        vm, sw = psutil.virtual_memory(), psutil.swap_memory()
        colour = RED if vm.available < 2 * 2**30 else (YELLOW if vm.available < 4 * 2**30 else GREEN)
        head += (f"   RAM {colour}{vm.available / 2**30:.1f} GB free{RESET}"
                 f"{DIM} of {vm.total / 2**30:.0f} · swap {sw.used / 2**30:.1f}{RESET}")
    lines = [head, f"{DIM}{d}{RESET}", ""]

    slots: dict[int, dict] = {}
    try:
        for k in range(1, heavy.target_slots() + 1):
            slots[k] = heavy._read_slot(d, k)
    except Exception:  # a view must not die on one unreadable file
        pass
    # A slot names its holder by the broker's pid, the same pid as the held record.
    held = sorted(held, key=lambda fr: -age_s(*fr))
    job_of = {r.get("pid"): n for n, (_, r) in enumerate(held, 1)}
    slot_of = {rec.get("pid"): k for k, rec in slots.items() if heavy._slot_busy(rec)}

    lines.append(f"{BOLD}RUNNING ({len(held)}){RESET}")
    owned: set[int] = set()
    for n, (f, r) in enumerate(held, 1):
        lines.extend(job_block(n, f, r, slot_of.get(r.get("pid")), primed, owned))
        lines.append("")
    if not held:
        lines += [f"  {DIM}(nothing){RESET}", ""]

    queue = heavy._queue_order(heavy._entries(d / "queue", reap=False), d)
    lines.append(f"{BOLD}QUEUED ({len(queue)}){RESET}")
    for i, (f, r) in enumerate(queue, 1):
        pad = f"  {YELLOW}│{RESET} "
        lines += [
            f"  {YELLOW}{BOLD}{i}.{RESET} {BOLD}{short_wt(r.get('worktree'))}{RESET}",
            field(pad, "command", short_argv(r, 72)),
            field(pad, "session", session_label(r)),
            field(pad, "waiting", f"{fmt_age(age_s(f, r))}   {DIM}priority "
                  f"{heavy._int_prio(r.get('prio'))}, weight {r.get('weight')}{RESET}"),
            field(pad, "takes", estimate(r)),
        ]
        if i < len(queue):
            lines.append("")
    if not queue:
        lines.append(f"  {DIM}(empty){RESET}")
    lines.append("")

    lines.append(f"{BOLD}TARGET SLOTS{RESET}")
    for k, rec in slots.items():
        if heavy._slot_busy(rec):
            n = job_of.get(rec.get("pid"))
            who = tag(n) if n else f"{YELLOW}?{RESET}"
            state = f"{who} {short_wt(rec.get('worktree'))}"
        else:
            state = f"{DIM}free  (last: {short_wt(rec.get('last_worktree'))}){RESET}"
        lines.append(f"  pool-{k}  {state}")
    lines.append("")

    stale = []
    wt = d / "wt"
    for lock in sorted(wt.glob("*.lock")) if wt.is_dir() else []:
        rec = heavy._read_lock(lock)
        if rec and not heavy.is_alive(rec.get("pid"), rec.get("ctime")):
            stale.append(rec)
    if stale:
        lines.append(f"{BOLD}ORPHAN LOCKS ({len(stale)}){RESET} "
                     f"{DIM}dead pid; the broker clears them itself{RESET}")
        for r in stale:
            lines.append(f"  {RED}✗{RESET} {short_wt(r.get('worktree')):<28} {DIM}{short_argv(r)}{RESET}")
        lines.append("")

    strays = stray_rustc(owned, primed)
    if strays:
        lines.append(f"{BOLD}RUSTC OUTSIDE THE BROKER ({len(strays)}){RESET} "
                     f"{DIM}cargo started without heavy.py (IDE, tauri dev, NUCLEOS_HEAVY=0){RESET}")
        lines.extend(rustc_line(i, "  ", with_target=True) for i in strays)
        lines.append("")
    if psutil is None:
        lines.append(f"{DIM}(pip install psutil to see rustc and RAM){RESET}")
    lines.append(f"{DIM}Ctrl+C to quit{RESET}")
    return "\n".join(lines)


def fit(text: str, rows: int) -> list[str]:
    """The frame cut to the terminal's height, so it never scrolls: drop the rustc lines of
    each job from the last one up, then cut the tail and say how much was left out."""
    lines = text.split("\n")

    def keep(limit: int) -> list[str]:
        out = []
        for line in lines:
            if line.startswith(RUSTC_MARK):
                rank, _, rest = line[1:].partition(RUSTC_MARK)
                if int(rank) < limit:
                    out.append(rest)
            else:
                out.append(line)
        return out

    for limit in (4, 2, 1, 0):
        out = keep(limit)
        if len(out) <= rows:
            break
    hidden = sum(1 for line in lines if line.startswith(RUSTC_MARK)) - sum(
        1 for line in lines if line.startswith(RUSTC_MARK)
        and int(line[1:].partition(RUSTC_MARK)[0]) < limit)
    if hidden:
        out[-1] += f"   {DIM}(+{hidden} rustc not shown){RESET}"
    if len(out) > rows:
        cut = len(out) - rows + 1
        out = out[:rows - 1] + [f"{YELLOW}… {cut} more lines: enlarge the terminal{RESET}"]
    return out


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--interval", type=float, default=2.0)
    args = ap.parse_args()
    os.system("")  # turns on ANSI escapes in a Windows console
    primed: dict = {}
    # Alternate screen (no scrollback to push frames into, as htop and less do), cursor
    # hidden, line wrap off so a long line costs one row; each frame is drawn over the last
    # from the top-left instead of clearing the screen, which is what made the VS Code
    # terminal grow a scrollback and stop following.
    sys.stdout.write("\x1b[?1049h\x1b[?25l\x1b[?7l")
    try:
        while True:
            size = shutil.get_terminal_size((120, 40))
            out = fit(frame(primed), max(5, size.lines))
            sys.stdout.write("\x1b[H" + "\x1b[K\n".join(out) + "\x1b[K\x1b[J")
            sys.stdout.flush()
            time.sleep(args.interval)
    except KeyboardInterrupt:
        return 0
    finally:
        sys.stdout.write("\x1b[?7h\x1b[?25h\x1b[?1049l")
        sys.stdout.flush()


if __name__ == "__main__":
    raise SystemExit(main())
