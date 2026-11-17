#!/usr/bin/env python3
"""Live view of the verify queue on top of the heavy broker view (read-only).

    python scripts/queues.py              full-screen live view, Ctrl+C to leave
    python scripts/queues.py --interval 5 redraw period in seconds (default 2)
    python scripts/queues.py --once       one frame on stdout, no screen control (for logs)
    python scripts/queues.py --json       machine output

The VERIFY block reads `verify_runs` + `verify_requests` of the live DB opened `mode=ro`
(NUCLEOS_DB overrides the path). Everything below it is scripts/heavy_watch.py's own frame, and
the screen mechanics are heavy_watch's too. Nothing is written. Stdlib only.
"""
from __future__ import annotations

import argparse
import json
import os
import shutil
import sqlite3
import sys
import time
from datetime import datetime
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import heavy  # noqa: E402
import heavy_watch as hw  # noqa: E402

DEFAULT_DB = r"C:\Users\Dudas\AppData\Local\nucleos\NucleOS\data\nucleos.db"
STALE_RUN_S = 45 * 60
RESET, DIM, BOLD, RED, YELLOW, CYAN = hw.RESET, hw.DIM, hw.BOLD, hw.RED, hw.YELLOW, hw.CYAN


def db_path() -> str:
    return os.environ.get("NUCLEOS_DB") or DEFAULT_DB


def parse_rfc3339(text):
    if not text:
        return None
    try:
        t = text.replace("Z", "+00:00")
        if "." in t:  # fromisoformat takes at most 6 fractional digits on older Pythons
            head, rest = t.split(".", 1)
            frac = ""
            while rest and rest[0].isdigit():
                frac, rest = frac + rest[0], rest[1:]
            t = f"{head}.{frac[:6]}{rest}"
        return datetime.fromisoformat(t).timestamp()
    except ValueError:
        return None


def wt_name(path) -> str:
    if not path:
        return "-"
    p = str(path).replace("\\", "/").rstrip("/")
    return p.rsplit("/", 1)[-1] or p


def norm_wt(path) -> str:
    return str(path or "").replace("\\", "/").replace("//?/", "").rstrip("/").lower()


# ------------------------------------------------------------------ data

def verify_snapshot(now: float) -> dict:
    out = {"db": db_path(), "running": [], "queued": [], "error": None}
    try:
        con = sqlite3.connect(f"file:{db_path()}?mode=ro", uri=True, timeout=2)
        con.row_factory = sqlite3.Row
        base = ("select r.id, r.origin, r.origin_id, r.group_name, r.kind, r.worktree, r.priority, "
                "r.enqueued_ms, r.started_at, r.timeout_ms, q.caller as caller "
                "from verify_runs r left join verify_requests q "
                "on r.origin = 'verify' and q.id = r.origin_id ")
        for r in con.execute(base + "where r.status = 'running' order by r.started_at, r.id"):
            d = dict(r)
            st = parse_rfc3339(d["started_at"])
            d["running_s"] = None if st is None else max(0.0, now - st)
            limit = min(STALE_RUN_S, (d["timeout_ms"] or 0) / 1000 or STALE_RUN_S)
            d["stale"] = bool(d["running_s"] and d["running_s"] > limit)
            out["running"].append(d)
        for r in con.execute(base + "where r.status = 'queued' "
                             "order by r.priority desc, r.enqueued_ms asc, r.id asc"):
            d = dict(r)
            d["waiting_s"] = (max(0.0, now - d["enqueued_ms"] / 1000)
                              if d["enqueued_ms"] else None)
            out["queued"].append(d)
        con.close()
    except sqlite3.Error as exc:
        out["error"] = str(exc)
    for d in out["running"] + out["queued"]:
        d["worktree_short"] = wt_name(d["worktree"])
        d["group_name"] = d["group_name"] or d["kind"] or "?"
        d["ticket"] =str(d["origin_id"]) if d["origin_id"] is not None else "-"
    return out


def live_holders() -> list[dict]:
    """Live heavy holders (broker records, dead owners already filtered, nothing reaped)."""
    out = []
    try:
        for _, rec in heavy._entries(heavy.state_dir() / "held", reap=False):
            out.append({"pid": rec.get("pid"), "worktree": str(rec.get("worktree") or ""),
                        "command": " ".join(str(a) for a in (rec.get("argv") or []))})
    except Exception:
        pass
    return out


def backing_pid(row: dict, holders: list[dict]):
    """HEURISTIC: a live holder whose worktree or command line mentions the row's worktree."""
    wt, name = norm_wt(row["worktree"]), wt_name(row["worktree"]).lower()
    for h in holders:
        hay = norm_wt(h["worktree"]) + " " + h["command"].replace("\\", "/").lower()
        if (wt and wt in hay) or (name and name != "-" and name in hay):
            return h["pid"]
    return None


def by_ticket(rows: list[dict]) -> list[tuple[str, list[dict]]]:
    """Rows grouped by (origin, ticket) in first-seen order."""
    order: dict = {}
    for r in rows:
        order.setdefault((r["origin"], r["ticket"]), []).append(r)
    return [(k[1], v) for k, v in order.items()]


# ------------------------------------------------------------------ frame

GLYPHS = {"broker": "B", "core": "C", "hooks": "H", "shell": "S", "sidecars": "D", "tauri": "T"}
GLYPH_ORDER = "BCHSDT"
GREEN = "\x1b[32m"
STATE_STYLE = {"running": BOLD + GREEN, "queued": DIM, "passed": GREEN, "failed": RED,
               "errored": RED, "cached": CYAN}
WIDE_COLS = 140
LEGEND = ("B broker C core H hooks S shell D sidecars T tauri   "
          "up/down PgUp/PgDn scroll  h rustc  q quit")


def glyph(group: str) -> str:
    g = GLYPHS.get(group)
    return g if g else (group[:1].lower() or "?")


def glyph_str(pairs: list[tuple[str, str]]) -> str:
    """Coloured glyphs for (group, state) pairs, in the canonical order."""
    def key(p):
        g = glyph(p[0])
        return (GLYPH_ORDER.index(g) if g in GLYPH_ORDER else 99, g)
    return "".join(f"{STATE_STYLE.get(s, '')}{glyph(grp)}{RESET}" for grp, s in sorted(pairs, key=key))


def pad_vis(s: str, n: int) -> str:
    return s + " " * max(0, n - hw.visible_len(s))


def trunc(s: str, n: int) -> str:
    return s if len(s) <= n else s[:max(1, n - 1)] + "…"


def row_line(tk, wt, glyphs, gw, mid, tail, w) -> str:
    """`tk wt GLYPHS mid tail`, the worktree name taking whatever width is left."""
    fixed = len(tk) + 1 + 1 + gw + 1 + hw.visible_len(mid) + (1 + hw.visible_len(tail) if tail else 0)
    wtw = max(8, min(22, w - fixed))
    line = f"{BOLD}{tk}{RESET} {pad_vis(trunc(wt, wtw), wtw)} {pad_vis(glyphs, gw)} {mid}"
    return line + (f" {tail}" if tail else "")


def verify_lines(v: dict, holders: list[dict], w: int, h: int, scroll: int = 0) -> tuple[list[str], int]:
    """The verify column, exactly `h` lines; returns (lines, clamped scroll)."""
    run, que = v["running"], v["queued"]
    oldest = max((r["waiting_s"] or 0 for r in que), default=None)
    head = (f"{BOLD}VERIFY{RESET} {len(run)} running, {len(que)} queued"
            + (f", oldest {hw.fmt_age(oldest)}" if oldest is not None else ""))
    L = [head]
    if v["error"]:
        L.append(f"{RED}DB error: {v['error']}{RESET}")
    queued_by_ticket = dict(by_ticket(que))
    gw = 6
    run_rows = []
    for tk, rows in by_ticket(run):
        wts = sorted({r["worktree_short"] for r in rows})
        pairs = [(r["group_name"], "running") for r in rows]
        pairs += [(r["group_name"], "queued") for r in queued_by_ticket.get(tk, [])]
        gw = max(gw, len(pairs))
        age = max((r["running_s"] or 0 for r in rows), default=None)
        pids = []
        for r in rows:
            p = backing_pid(r, holders)
            if p is not None and p not in pids:
                pids.append(p)
        tail = " ".join(hw.tag(p) for p in pids) if pids else f"{YELLOW}no job{RESET}"
        if any(r["stale"] for r in rows):
            tail += f" {RED}STALE{RESET}"
        run_rows.append((tk, ",".join(wts), glyph_str(pairs),
                         hw.fmt_age(age) if age is not None else "?", tail))
    qt = by_ticket(que)
    q_rows = []
    for tk, rows in qt:
        waited = max((r["waiting_s"] or 0 for r in rows), default=None)
        gw = max(gw, len(rows))
        q_rows.append((tk, ",".join(sorted({r["worktree_short"] for r in rows})),
                       glyph_str([(r["group_name"], "queued") for r in rows]),
                       hw.fmt_age(waited) if waited is not None else "?", waited or 0, len(rows)))

    body = max(0, h - 1)  # the legend takes the last line
    # keep a queued header + 2 rows free when there is anything queued
    run_room = max(1, body - len(L) - 1 - (3 if q_rows else 0))
    L.append(f"{BOLD}RUNNING ({len(run)}){RESET}")
    if len(run_rows) > run_room:
        shown_run = run_rows[:max(0, run_room - 1)]
    else:
        shown_run = run_rows
    for tk, wt, gl, age, tail in shown_run:
        L.append(row_line(tk, wt, gl, gw, f"{BOLD}{GREEN}{age}{RESET}", tail, w))
    if len(run_rows) > len(shown_run):
        L.append(f"{DIM}+{len(run_rows) - len(shown_run)} running more{RESET}")
    if not run_rows:
        L.append(f"{DIM}(none){RESET}")
    L.append(f"{BOLD}QUEUED ({len(qt)} tickets, {len(que)} groups){RESET}")
    room = max(0, body - len(L))
    if not q_rows:
        L.append(f"{DIM}(empty){RESET}")
        scroll = 0
    else:
        n = room if len(q_rows) <= room else max(0, room - 1)  # last line = "+N tickets" summary
        scroll = max(0, min(scroll, max(0, len(q_rows) - max(1, n))))
        part = q_rows[scroll:scroll + n]
        for tk, wt, gl, age, _, _ in part:
            L.append(row_line(tk, wt, gl, gw, f"{DIM}{age}{RESET}", "", w))
        rest = q_rows[:scroll] + q_rows[scroll + len(part):]
        if rest and room > 0:
            groups = sum(r[5] for r in rest)
            old = max(r[4] for r in rest)
            L.append(f"{DIM}+{len(rest)} tickets ({groups} groups), oldest {hw.fmt_age(old)}{RESET}")
    L = L[:body]
    while len(L) < body:
        L.append("")
    return L + [f"{DIM}{LEGEND}{RESET}"], scroll


def heavy_text_of(primed: dict, hide_rustc: bool) -> str:
    text = hw.frame(primed)
    if hide_rustc:
        text = "\n".join(ln for ln in text.split("\n") if not ln.startswith(hw.RUSTC_MARK))
    return text


def compose(primed: dict, cols: int, rows: int, state: dict) -> list[str]:
    v = verify_snapshot(time.time())
    holders = live_holders()
    heavy_text = heavy_text_of(primed, state.get("hide_rustc", False))
    if cols >= WIDE_COLS:
        lw = min(72, cols * 2 // 5)
        rw = cols - lw - 1
        left, state["scroll"] = verify_lines(v, holders, lw, rows, state.get("scroll", 0))
        right = [hw.clip(x, rw) for x in hw.fit(heavy_text, rows)]
        out = []
        for i in range(rows):
            lft = hw.clip(left[i], lw) if i < len(left) else ""
            rgt = right[i] if i < len(right) else ""
            out.append(f"{pad_vis(lft, lw)}{DIM}│{RESET}{rgt}")
        return out
    vh = rows // 2
    left, state["scroll"] = verify_lines(v, holders, cols, vh, state.get("scroll", 0))
    return ([hw.clip(x, cols) for x in left]
            + [hw.clip(x, cols) for x in hw.fit(heavy_text, rows - vh)])


def render(primed: dict, cols: int, rows: int, state: dict) -> list[str]:
    try:
        return compose(primed, cols, rows, state)
    except Exception as exc:  # a view must outlive one bad frame
        return [f"{RED}frame error: {type(exc).__name__}: {exc}{RESET}"]


def poll_keys(state: dict, timeout: float) -> bool:
    """Wait up to `timeout` s; True when a key changed the state (redraw now).
    Sets state['quit'] on q. Without msvcrt it just sleeps."""
    try:
        import msvcrt
    except ImportError:
        time.sleep(timeout)
        return False
    end = time.time() + timeout
    while time.time() < end:
        if not msvcrt.kbhit():
            time.sleep(0.05)
            continue
        ch = msvcrt.getwch()
        if ch in ("\x00", "\xe0"):
            step = {"H": -1, "P": 1, "I": -10, "Q": 10}.get(msvcrt.getwch())
            if step is None:
                continue
            state["scroll"] = max(0, state.get("scroll", 0) + step)
            return True
        if ch in ("q", "Q"):
            state["quit"] = True
            return True
        if ch in ("h", "H"):
            state["hide_rustc"] = not state.get("hide_rustc", False)
            return True
    return False


def json_snapshot() -> dict:
    now = time.time()
    v = verify_snapshot(now)
    holders = live_holders()
    for r in v["running"]:
        r["heavy_pid"] = backing_pid(r, holders)
    return {"time": datetime.now().strftime("%Y-%m-%d %H:%M:%S"), "verify": v,
            "heavy_holders": holders}


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--interval", type=float, default=2.0)
    ap.add_argument("--once", action="store_true", help="one frame, no screen control")
    ap.add_argument("--json", action="store_true", help="machine output")
    ap.add_argument("--width", type=int, help="force the layout width (columns)")
    ap.add_argument("--height", type=int, help="force the layout height (rows)")
    args = ap.parse_args()
    try:
        sys.stdout.reconfigure(encoding="utf-8", errors="replace")
    except (AttributeError, ValueError):
        pass
    if args.json:
        print(json.dumps(json_snapshot(), indent=2, default=str))
        return 0
    if args.once:
        text = "\n".join(render({}, args.width or 160, args.height or 60, {}))
        print(hw.ANSI.sub("", text) if not sys.stdout.isatty() else text)
        return 0
    os.system("")  # turns on ANSI escapes in a Windows console
    primed: dict = {}
    state: dict = {}
    # Same screen handling as heavy_watch.main: alternate screen, cursor hidden, wrap off,
    # each frame drawn over the last from the top-left.
    sys.stdout.write("\x1b[?1049h\x1b[?25l\x1b[?7l")
    try:
        while True:
            size = shutil.get_terminal_size((120, 40))
            out = render(primed, args.width or size.columns, max(5, args.height or size.lines), state)
            hw.prune_primed(primed)
            sys.stdout.write("\x1b[H" + "\x1b[K\n".join(out) + "\x1b[K\x1b[J")
            sys.stdout.flush()
            end = time.time() + args.interval
            while not state.get("quit") and time.time() < end:
                if poll_keys(state, max(0.05, end - time.time())):
                    break
            if state.get("quit"):
                return 0
    except KeyboardInterrupt:
        return 0
    finally:
        sys.stdout.write("\x1b[?7h\x1b[?25h\x1b[?1049l")
        sys.stdout.flush()


if __name__ == "__main__":
    raise SystemExit(main())
