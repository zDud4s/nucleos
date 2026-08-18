#!/usr/bin/env python3
"""Run an ablation ladder end to end — prepare, launch, approve, watch, score — one cell at a time.

    python scripts/eval/ladder.py T3            # all four layers of T3
    python scripts/eval/ladder.py T2:H3 T3      # one cell, then a whole ladder

Every step here was done by hand for T1 on 2026-08-17. It is checked in for the reason `base.sh`
gives about its own patches: a recipe written in prose is a recipe nobody can rerun, and three
earlier attempts at this ablation died on setup nobody had written down.

## Why cells run one at a time

`layer.py` points every layer at one shared `CARGO_TARGET_DIR`, because a per-tree target is ~12 GB
and a twelve-cell ladder does not fit on this machine. Cargo locks that directory. Two cells at once
therefore each report the other's compile inside their own wall clock — and wall clock is half of
what the table says. This holds for any build of yours during a cell, not just for a second cell.

## Three traps this encodes, each measured

**A `mode: real` run carries no `project_id`.** `create_run_inner` only requires one for a worktree.
A watcher that filters by project alone finds nothing for H0, sits out its entire ceiling and then
reports a timeout that never happened, while the row it cannot see finished long ago. Forty minutes
per H0 cell, on 2026-08-18, before anyone noticed.

**Approving does not resume the run it approved.** `resume_after_approval` marks that run
`superseded` and INSERTS a successor with a new id. So the approver is scoped by PROJECT — scoped to
a run id it answers the first question and goes deaf — and a cell's cost is the sum of its chain.
The superseded leg's `cost_usd` is NULL, which makes that sum a lower bound.

**H0 is pre-warmed and the others are not, deliberately.** A worktree is a fresh checkout and pays one
crate compile inside its own clock; `mode: real` runs in a tree that has been built, as it would in
the repo of whoever is programming. That difference belongs to the modes and is reported beside the
time rather than erased. The stamp `layer.py` applies is what keeps it honest — without it cargo can
call a tree fresh and run a binary built from another one.

## What this does NOT decide

Not the prompt (a column of `tasks.tsv`), not what a layer is (`layer.py`), not the verdict
(`score.sh`, which grafts the reference's own test). It only sequences them, so that what varies
between two rows is the layer and not the day someone had.

## Environment

Same as `score.sh`: `cargo` on PATH, `RUSTUP_HOME`/`CARGO_HOME` set, and a daemon answering on 8791.
Git's bash by absolute path — a bare `bash` here is WSL.
"""
import glob
import json
import os
import subprocess
import sys
import time
import urllib.error
import urllib.request

# Overridable, because a checked-in script that only runs on the machine it was written on is a
# personal note with a path in the repository. Defaults are this machine's.
ROOT = os.environ.get("NUCLEOS_ROOT") or os.path.abspath(
    os.path.join(os.path.dirname(__file__), "..", "..")
).replace("\\", "/")
TREES = os.environ.get("EVAL_TREES", "C:/Projects/nucleos-eval")
DAEMON = os.environ.get("NUCLEOS_DAEMON_URL", "http://127.0.0.1:8791")
GIT_BASH = os.environ.get("GIT_BASH", "C:/Program Files/Git/bin/bash.exe")

# The copy first: `layer.py` explains why the daemon is run from outside `target/` (bin-only crate,
# Windows denies relinking an exe it holds open). Either binary prints the same token — it is state,
# not a property of the build — so falling back to the project's own is safe.
DAEMON_EXE = next(
    (candidate for candidate in (
        os.environ.get("NUCLEOS_DAEMON_EXE"),
        "C:/Projects/nucleos-daemon/nucleos-core.exe",
        f"{ROOT}/target/debug/nucleos-core.exe",
    ) if candidate and os.path.exists(candidate)),
    f"{ROOT}/target/debug/nucleos-core.exe",
)

LAYERS = ["H0", "H1", "H2", "H3"]
MODE = {"H0": "real", "H1": "worktree", "H2": "worktree", "H3": "worktree"}
TERMINAL = ("succeeded", "completed", "failed", "timed_out", "cancelled", "killed", "errored")

ENV = dict(os.environ)
ENV.update({
    "RUSTUP_HOME": "C:/Projects/rustup",
    "CARGO_HOME": "C:/Projects/cargo",
    "PATH": "C:\\Projects\\mingw64\\bin;C:\\Projects\\cargo\\bin;" + os.environ.get("PATH", ""),
})

token = subprocess.run([DAEMON_EXE, "--print-token"], capture_output=True, text=True).stdout.strip()


def say(text):
    print(text, flush=True)


def call(path, method="GET", body=None):
    request = urllib.request.Request(
        DAEMON + path,
        data=json.dumps(body).encode() if body is not None else None,
        headers={"Authorization": f"Bearer {token}", "Content-Type": "application/json"},
        method=method,
    )
    with urllib.request.urlopen(request, timeout=30) as response:
        raw = response.read()
    return json.loads(raw) if raw else None


def prompt_for(task):
    for line in open(os.path.join(ROOT, "scripts/eval/tasks.tsv"), encoding="utf-8"):
        if line.startswith("#") or not line.strip():
            continue
        parts = line.rstrip("\n").split("\t")
        if parts[0] == task:
            return parts[4]
    raise SystemExit(f"no prompt for {task}")


def sh(args, timeout=2400):
    return subprocess.run(args, cwd=ROOT, env=ENV, capture_output=True, text=True, timeout=timeout)


def prepare(task, layer, tree):
    for stale in glob.glob(f"{TREES}/nucleos-worktrees/{task}-{layer}"):
        subprocess.run(["cmd", "/c", "rmdir", "/s", "/q", stale.replace("/", "\\")],
                       capture_output=True)
    done = sh([GIT_BASH, "scripts/eval/base.sh", "--task", task, "--dest", tree])
    if done.returncode != 0:
        raise SystemExit(f"base.sh failed for {task}: {done.stderr[-400:]}")
    done = sh(["python", "scripts/eval/layer.py", "--layer", layer, "--tree", tree])
    if done.returncode != 0:
        raise SystemExit(f"layer.py failed for {task}/{layer}: {done.stderr[-400:]}")

    # H0 works in this very tree, as `mode: real` works in the repo of whoever is programming — warm.
    # H1-H3 get a fresh checkout from the daemon and pay one crate compile inside their own clock;
    # that difference belongs to the modes, and is reported rather than erased.
    if layer == "H0":
        started = time.time()
        built = subprocess.run(["cargo", "test", "-p", "nucleos-core", "--no-run"],
                               cwd=tree, env=ENV, capture_output=True, text=True, timeout=2400)
        say(f"    pre-aquecimento H0: exit={built.returncode} {int(time.time()-started)}s")


def watch(project, first_id):
    """Follow the chain, or the single run when there is no chain to follow.

    `mode: real` takes no `project_id` — `create_run_inner` only requires one for a worktree — so a
    watcher that filtered by project alone found nothing, sat out its whole 2400s ceiling and then
    reported a timeout that never happened. Cost 40 minutes per H0 cell before anyone noticed,
    because the row it could not see had long since finished.
    """
    started = time.time()
    last = None
    mine = []
    while True:
        try:
            rows = call("/runs")
        except (urllib.error.URLError, OSError) as error:
            # Survivable, and it has to be. `auto-approve.py` already says why for its own loop: the
            # daemon gets restarted around here — rebuilt, or restarted by whoever else is working in
            # this repo — and a watcher that died on the first gap would have to be babysat by the
            # person it exists to replace. Measured 2026-08-18: the daemon went down mid-cell and
            # took the ladder with it, leaving a run nobody was watching.
            say(f"    daemon inalcançável ({error}); a tentar de novo")
            time.sleep(10)
            continue
        rows = rows if isinstance(rows, list) else rows.get("runs", [])
        mine = ([r for r in rows if r.get("project_id") == project] if project
                else [r for r in rows if r.get("id") == first_id])
        row = max(mine, key=lambda r: r.get("id", 0)) if mine else {}
        status = row.get("status")
        if (row.get("id"), status) != last:
            say(f"    [{int(time.time()-started):4d}s] run {row.get('id')} {status}")
            last = (row.get("id"), status)
        if status in TERMINAL:
            break
        if time.time() - started > 2400:
            say("    watcher: a cadeia passou o tecto da worktree")
            break
        time.sleep(10)
    wall = None
    if mine:
        ordered = sorted(mine, key=lambda r: r.get("id", 0))
        try:
            import datetime
            t0 = datetime.datetime.fromisoformat(ordered[0]["created_at"])
            t1 = datetime.datetime.fromisoformat(ordered[-1]["completed_at"])
            wall = int((t1 - t0).total_seconds())
        except Exception:
            pass
    return {
        "runs": [r.get("id") for r in sorted(mine, key=lambda r: r.get("id", 0))],
        "status": (max(mine, key=lambda r: r.get("id", 0)) or {}).get("status") if mine else None,
        "cost": round(sum(r.get("cost_usd") or 0 for r in mine), 4),
        "turns": (max(mine, key=lambda r: r.get("id", 0)) or {}).get("num_turns") if mine else None,
        "wall": wall,
        "first": first_id,
    }


def score(task, layer, tree, first_id):
    target = tree
    if MODE[layer] == "worktree":
        found = glob.glob(f"{TREES}/nucleos-worktrees/{task}-{layer}/run-*")
        if not found:
            return "no-worktree"
        target = found[0].replace("\\", "/")
    done = sh([GIT_BASH, "scripts/eval/score.sh", "--task", task, "--tree", target], timeout=2400)
    for piece in (done.stdout or "").split():
        if piece.startswith("verdict="):
            return piece.split("=", 1)[1]
    return f"unreadable(exit {done.returncode})"


def requested(argv):
    """`T3` means the whole ladder; `T2:H3` means one cell.

    A ladder that stops half-way — the account's monthly spend limit did it on 2026-08-18 — must be
    finishable without paying again for the cells that already landed.
    """
    for arg in argv:
        task, _, layer = arg.partition(":")
        for one in ([layer] if layer else LAYERS):
            if one not in LAYERS:
                raise SystemExit(f"unknown layer {one!r} in {arg!r}")
            yield task, one


def turns_of(run_ids):
    """Read from the database, because `GET /runs` does not carry `num_turns`."""
    import sqlite3
    db = os.environ.get("NUCLEOS_DB") or os.path.join(
        os.environ.get("LOCALAPPDATA", ""), "nucleos", "NucleOS", "data", "nucleos.db"
    )
    try:
        con = sqlite3.connect(f"file:{db}?mode=ro", uri=True)
        rows = con.execute(
            "SELECT num_turns FROM runs WHERE id IN (%s) AND num_turns IS NOT NULL"
            % ",".join("?" * len(run_ids)),
            list(run_ids),
        ).fetchall()
        return sum(r[0] for r in rows) or None
    except Exception:
        return None


results = []
for task, layer in requested(sys.argv[1:]):
    prompt = prompt_for(task)
    if True:
        project = f"eval-{task}-{layer}"
        tree = f"{TREES}/{task}-{layer}"
        say(f"\n===== {task} x {layer} =====")
        prepare(task, layer, tree)

        body = {"prompt": prompt, "cwd": tree, "mode": MODE[layer]}
        if MODE[layer] == "worktree":
            body["project_id"] = project
        try:
            created = call("/runs", "POST", body)
        except urllib.error.HTTPError as error:
            say(f"    recusado: HTTP {error.code} {error.read().decode()[:200]}")
            results.append((task, layer, "refused", None, None, None))
            continue
        first_id = created["id"]
        say(f"    run {first_id} criado")

        approver = None
        if layer in ("H2", "H3"):
            approver = subprocess.Popen(
                ["python", "scripts/eval/auto-approve.py", "--project", project,
                 "--interval", "3", "--max", "200"],
                cwd=ROOT, env=ENV, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
            )
        try:
            outcome = watch(project if MODE[layer] != "real" else None, first_id)
        finally:
            if approver:
                approver.terminate()

        verdict = score(task, layer, tree, first_id)
        turns = turns_of(outcome["runs"]) if outcome["runs"] else None
        say(f"    -> {verdict} | {outcome['wall']}s | ${outcome['cost']} | "
            f"turnos={turns} | runs={outcome['runs']}")
        results.append((task, layer, verdict, outcome["wall"], outcome["cost"], turns))

say("\n================ RESUMO ================")
for row in results:
    say("\t".join("" if v is None else str(v) for v in row))
