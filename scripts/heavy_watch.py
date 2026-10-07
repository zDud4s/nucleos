"""Live, read-only view of the heavy-command broker (scripts/heavy.py).

    python scripts/heavy_watch.py [--interval 2]

Shows the capacity in use, who holds a place, who waits, the target-dir slots and every
running rustc with its CPU share. Never reaps, never writes: it reads the same state files
`heavy.py status` reads. Ctrl+C to leave.
"""
from __future__ import annotations

import argparse
import os
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import heavy  # noqa: E402

try:
    import psutil
except ImportError:  # the broker view still works without the rustc panel
    psutil = None

RESET, DIM, BOLD = "\x1b[0m", "\x1b[2m", "\x1b[1m"
GREEN, YELLOW, RED, CYAN = "\x1b[32m", "\x1b[33m", "\x1b[31m", "\x1b[36m"


def short_wt(path) -> str:
    return Path(str(path or "-")).name or str(path)


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


def fmt_age(s: int) -> str:
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


def rustc_rows(primed: dict) -> list[str]:
    if psutil is None:
        return [f"  {DIM}(pip install psutil para ver o rustc){RESET}"]
    rows = []
    for p in psutil.process_iter(["pid", "name", "cmdline", "create_time", "memory_info"]):
        if (p.info["name"] or "").lower() != "rustc.exe":
            continue
        cmd = p.info["cmdline"] or []
        if "--crate-name" not in cmd:
            continue
        crate = cmd[cmd.index("--crate-name") + 1]
        out = cmd[cmd.index("--out-dir") + 1] if "--out-dir" in cmd else "?"
        target = Path(out).parent.parent.name
        if p.pid not in primed:
            primed[p.pid] = p
            p.cpu_percent(None)
            cpu = "…"
        else:
            cpu = f"{primed[p.pid].cpu_percent(None) / psutil.cpu_count():5.1f}%"
        mb = (p.info["memory_info"].rss if p.info["memory_info"] else 0) // 2**20
        up = fmt_age(int(time.time() - p.info["create_time"]))
        test = " test" if "--test" in cmd else ""
        rows.append(f"  {p.pid:>6}  {crate}{test:<5}  {CYAN}{target:<24}{RESET} "
                    f"cpu {cpu:>6}  {mb:>5} MB  {up}")
    return rows or [f"  {DIM}(nenhum){RESET}"]


def frame(primed: dict) -> str:
    d = heavy.state_dir()
    lines = [f"{BOLD}heavy broker{RESET}  {DIM}{d}  {time.strftime('%H:%M:%S')}{RESET}", ""]

    held = heavy._entries(d / "held", reap=False)
    used = sum(int(r.get("weight") or 1) for _, r in held)
    lines.append(f"{BOLD}capacidade{RESET} {bar(used, heavy.capacity())}")
    if psutil is not None:
        vm, sw = psutil.virtual_memory(), psutil.swap_memory()
        colour = RED if vm.available < 2 * 2**30 else (YELLOW if vm.available < 4 * 2**30 else GREEN)
        lines.append(f"{BOLD}RAM{RESET}        {colour}{vm.available / 2**30:4.1f} GB livres{RESET} "
                     f"de {vm.total / 2**30:.1f}  {DIM}swap {sw.used / 2**30:.1f} GB{RESET}")
    lines.append("")

    lines.append(f"{BOLD}a correr ({len(held)}){RESET}")
    for f, r in held:
        lines.append(f"  {GREEN}●{RESET} {fmt_age(age_s(f, r)):>6}  w{r.get('weight')}  "
                     f"{short_wt(r.get('worktree')):<28} {DIM}{short_argv(r)}{RESET}")
    if not held:
        lines.append(f"  {DIM}(nada){RESET}")
    lines.append("")

    queue = heavy._queue_order(heavy._entries(d / "queue", reap=False), d)
    lines.append(f"{BOLD}na fila ({len(queue)}){RESET}")
    for i, (f, r) in enumerate(queue, 1):
        lines.append(f"  {YELLOW}{i}.{RESET} {fmt_age(age_s(f, r)):>6}  "
                     f"p{heavy._int_prio(r.get('prio'))} w{r.get('weight')}  "
                     f"{short_wt(r.get('worktree')):<28} {DIM}{short_argv(r)}{RESET}")
    if not queue:
        lines.append(f"  {DIM}(vazia){RESET}")
    lines.append("")

    lines.append(f"{BOLD}target slots{RESET}")
    try:
        for k in range(1, heavy.target_slots() + 1):
            rec = heavy._read_slot(d, k)
            if heavy._slot_busy(rec):
                state = f"{GREEN}ocupado{RESET} {short_wt(rec.get('worktree'))}"
            else:
                state = f"{DIM}livre{RESET}"
            lines.append(f"  pool-{k}: {state}  {DIM}último={short_wt(rec.get('last_worktree'))}{RESET}")
    except Exception as exc:  # a view must not die on one unreadable file
        lines.append(f"  {RED}ilegível: {exc}{RESET}")
    lines.append("")

    wt = d / "wt"
    stale = []
    for lock in sorted(wt.glob("*.lock")) if wt.is_dir() else []:
        rec = heavy._read_lock(lock)
        if rec and not heavy.is_alive(rec.get("pid"), rec.get("ctime")):
            stale.append(rec)
    if stale:
        lines.append(f"{BOLD}locks órfãos ({len(stale)}){RESET} {DIM}pid morto; o broker limpa-os sozinho{RESET}")
        for r in stale:
            lines.append(f"  {RED}✗{RESET} {short_wt(r.get('worktree')):<28} {DIM}{short_argv(r)}{RESET}")
        lines.append("")

    lines.append(f"{BOLD}rustc{RESET}")
    lines.extend(rustc_rows(primed))
    lines.append("")
    lines.append(f"{DIM}Ctrl+C para sair{RESET}")
    return "\n".join(lines)


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--interval", type=float, default=2.0)
    args = ap.parse_args()
    os.system("")  # turns on ANSI escapes in a Windows console
    primed: dict = {}
    try:
        while True:
            text = frame(primed)
            sys.stdout.write("\x1b[H\x1b[2J" + text + "\n")
            sys.stdout.flush()
            time.sleep(args.interval)
    except KeyboardInterrupt:
        return 0


if __name__ == "__main__":
    raise SystemExit(main())
