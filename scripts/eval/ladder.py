#!/usr/bin/env python3
"""Run an ablation ladder end to end — prepare, launch, approve, watch, score — one cell at a time.

    python scripts/eval/ladder.py T3            # every layer of T3 still runnable: H0, H2, H3
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

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import ingest  # noqa: E402
import layer as layer_module  # noqa: E402

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

LAYERS = ["H0", "H2", "H3"]

# Retired, not forgotten. H1 is the worktree layer WITHOUT the classifier hook. Since `a840181`
# (2026-08-26) `create_run` refuses an unattended run for a project in `off`, and putting a project
# in `shadow` requires that very hook (`activation_prerequisites`, `core/src/autopilot.rs`) — so the
# daemon no longer runs the configuration H1 describes. The ladder ran on 2026-08-17/19, before that
# door asked anything; H1's cells from then stay in the ledger as history, and no new one can be
# measured under this daemon.
RETIRED = {
    "H1": "it is the worktree layer without the classifier hook, and since a840181 (2026-08-26) a "
          "worktree run needs its project in shadow, which requires that hook -- the daemon no "
          "longer runs what H1 describes",
}
MODE = {"H0": "real", "H1": "worktree", "H2": "worktree", "H3": "worktree"}
TERMINAL = ("succeeded", "completed", "failed", "timed_out", "cancelled", "killed", "errored")
# How long a chain may run before the watcher stops following it. A worktree chain pays a fresh
# crate compile, every approval pause and the daemon's own gate before it turns terminal: T4xH3 on
# 2026-09-13 was still inside that gate at 2400s, and the ladder went on to score a tree the agent
# was still writing to. An environment override, because a slow machine needs more, not a patch.
WATCH_CEILING = int(os.environ.get("EVAL_WATCH_CEILING", "5400"))

# `core/src/worktree.rs::worktree_root()`'s default moved from a SIBLING of the project root
# (`<parent>/nucleos-worktrees/<project>/run-*`) to INSIDE it (`<project>/.nucleos/worktrees/run-*`,
# `ARTIFACTS_DIR` + "worktrees" there). Named once so `prepare()` and `score()` below can't drift
# out of sync with each other the way they drifted out of sync with the daemon: `glob.glob` on the
# old shape doesn't error when it matches nothing, so `score()` silently scored every worktree-mode
# layer "no-worktree" instead of scoring it, and `prepare()`'s stale-tree cleanup silently stopped
# cleaning anything.
WORKTREES_SUBDIR = ".nucleos/worktrees"

ENV = dict(os.environ)
ENV.update({
    "RUSTUP_HOME": "C:/Projects/rustup",
    "CARGO_HOME": "C:/Projects/cargo",
    "PATH": "C:\\Projects\\mingw64\\bin;C:\\Projects\\cargo\\bin;" + os.environ.get("PATH", ""),
})

_TOKEN = None


def daemon_token():
    """Fetch the daemon token on first use, not at import.

    This used to run at module scope, which meant importing this file spawned the daemon binary --
    and, with the ladder loop also at module scope, an `import ladder` ran real cells at real cost.
    Nothing could test any function in here without paying for it. Both are now behind a call.
    """
    global _TOKEN
    if _TOKEN is None:
        _TOKEN = subprocess.run(
            [DAEMON_EXE, "--print-token"], capture_output=True, text=True
        ).stdout.strip()
    return _TOKEN


def say(text):
    print(text, flush=True)


Cell = ingest.Cell


def record(cell, run_ids):
    """Append the cell to the durable ledger as it finishes, not at the end of the ladder.

    A ladder that only prints its results loses them when the terminal closes, which is how the
    nineteen cells of 2026-08-17/19 came to survive only as prose. A failure to write must not kill
    a cell that already cost real money, so it is shouted with the row inline and the run carries
    on — the summary below is still printed either way.
    """
    try:
        ingest.append_cell(ingest.LEDGER, cell, run_ids=run_ids, source="ladder")
    except Exception as error:  # noqa: BLE001 — losing the row is worse than any write failure
        say(f"    AVISO: a linha do ledger nao foi escrita ({error}); grava-a a mao: {cell}")


def start_approver(project):
    """The approver that answers the H2/H3 classifier, scoped to this cell's project and nothing else.

    A function rather than inline so a test can stand it in: the real one polls the daemon on 8791
    and answers every pending approval of the project it is given, which no test should do.
    """
    return subprocess.Popen(
        ["python", "scripts/eval/auto-approve.py", "--project", project,
         "--interval", "3", "--max", "200"],
        cwd=ROOT, env=ENV, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
    )


def onboarding_request(layer, tree):
    """The body of `POST /projects/{id}/onboard` for one cell: its tree, and its layer's gate.

    Pure, so a test can hold it without a daemon. The gate is `layer.gate_for`'s — H3's, and `None`
    ("no gate confirmed") for every other layer, which leaves the project's rules untouched.
    """
    return {"project_root": tree, "gate_command": layer_module.gate_for(layer)}


def activate(project, tree, layer):
    """Onboard the cell's own project, then put it in `shadow`, rooted at the cell's own tree.

    Needed since `a840181`: a worktree run for a project in `off` is refused, and a project this
    daemon has never heard of reads as off. `shadow`, not `active` — the gate refuses only `off`,
    and `shadow` is the least the door accepts.

    Onboarding first, because the mode door refuses a project nobody onboarded. It goes through the
    daemon rather than through files, so the marker and H3's gate land where the daemon reads them —
    `~/.nucleos/projects/<project>/` — under the id this function registers. It also (re)wires the
    classifier hook at the tree's ROOT, which the H2/H3 trees already carry: the root is what
    activation inspects, and the worktree the agent is handed is checked out from HEAD, which this
    does not touch.

    Checked to be inert before it was written. The scheduler, the repo trigger and the webhook all
    act only on a project's autopilot rules, and an eval project has none but H3's `gate_command`:
    no `schedules`, no `repo_triggers`. The project exists for exactly one reason — so this cell's
    run is let in.

    Raises the daemon's `HTTPError` when onboarding or activation is refused, so the caller records
    the cell.
    """
    call(f"/projects/{project}/onboard", "POST", onboarding_request(layer, tree))
    call("/autopilot/state", "POST", {"project_id": project, "mode": "shadow", "project_root": tree})


def deactivate(project):
    """Take the cell's project back out: `off` first, then off the owner's roster. Never raises.

    `off` first because it is what shuts the door, and nothing can hold it up. Then
    `DELETE /projects/{id}` with no query — the removal the daemon calls the reversible one: nothing
    on disk is touched, and the history the ledger's run ids point into is kept unless
    `forget_history` is asked for, which it never is here. Without that second step every worktree
    cell left a row behind, because the roster lists projects in every mode: the probe of 2026-09-11
    found `eval-T1-H2` there, switched off, where before it there had been nothing.

    The removal answers 409 while the daemon still holds the run's worktree. The daemon releases it,
    or its half-hourly sweep marks it removed, and the next cell for the same project takes the row
    out — so a 409 is reported, not fought, and the project it leaves behind is already `off`. Any
    failure is shouted with the request that fixes it and the ladder goes on: killing a cell that
    already cost money over a roster entry would be the worse trade.
    """
    off = {"project_id": project, "mode": "off"}
    try:
        call("/autopilot/state", "POST", off)
    except Exception as error:  # noqa: BLE001 — a stuck roster entry is worse than no ladder
        say(f"    AVISO: nao foi possivel desligar {project} ({error}). "
            f"Desliga-o: POST /autopilot/state {json.dumps(off)}")
    try:
        call(f"/projects/{project}", "DELETE")
    except urllib.error.HTTPError as error:
        if error.code == 404:
            return  # never on the roster, or already off it: the state this wants
        why = ("o daemon ainda tem a worktree ou o slot do run" if error.code == 409
               else error.read().decode()[:200])
        say(f"    AVISO: {project} ficou no roster, desligado (HTTP {error.code}: {why}). "
            f"Inerte; sai na proxima celula deste projecto. Tira-o: DELETE /projects/{project}")
    except Exception as error:  # noqa: BLE001
        say(f"    AVISO: {project} ficou no roster ({error}). Tira-o: DELETE /projects/{project}")


def call(path, method="GET", body=None):
    request = urllib.request.Request(
        DAEMON + path,
        data=json.dumps(body).encode() if body is not None else None,
        headers={"Authorization": f"Bearer {daemon_token()}", "Content-Type": "application/json"},
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


def base_of(output):
    """The commit `base.sh` laid the tree out from, read off its report line.

    Its last line is `task<TAB>kind<TAB>sha<TAB>dest`. `None` when there is no such line, so the
    row reads as unrecorded rather than the cell dying after its tree was already built.
    """
    lines = output.strip().splitlines()
    fields = lines[-1].split("\t") if lines else []
    return fields[2] if len(fields) >= 3 else None


def prepare(task, layer, tree):
    """Lay out the cell's tree and return the base commit it was laid out from."""
    # Sweep both addresses a worktree-mode cell may have left something at: the legacy sibling
    # (`nucleos-worktrees/<task>-<layer>`, what a machine that ran the ladder before the daemon's
    # default moved can still have on disk) and the current one, `<tree>/WORKTREES_SUBDIR`.
    # `materialize.sh`'s `rm -rf $dest` a few lines below already clears the current address in the
    # ordinary case, but not when a lock survives it (a build, a still-running daemon holding an exe
    # open) — the same reason this loop existed for the legacy address in the first place.
    for stale in (
        f"{TREES}/nucleos-worktrees/{task}-{layer}",
        f"{tree}/{WORKTREES_SUBDIR}",
    ):
        for hit in glob.glob(stale):
            subprocess.run(["cmd", "/c", "rmdir", "/s", "/q", hit.replace("/", "\\")],
                           capture_output=True)
    done = sh([GIT_BASH, "scripts/eval/base.sh", "--task", task, "--dest", tree])
    if done.returncode != 0:
        raise SystemExit(f"base.sh failed for {task}: {done.stderr[-400:]}")
    base = base_of(done.stdout)
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
    return base


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
        # `id >= first_id` scopes this to THIS cell. A project id outlives a cell — a re-run
        # after a failure reuses it — so without the floor the chain swept up every earlier
        # attempt: T2xH3 reported 6311s and $2.08 across two dead runs and the live one, where
        # the cell was 1014s and $1.65.
        mine = ([r for r in rows
                 if r.get("project_id") == project and (r.get("id") or 0) >= first_id]
                if project else [r for r in rows if r.get("id") == first_id])
        row = max(mine, key=lambda r: r.get("id", 0)) if mine else {}
        status = row.get("status")
        if (row.get("id"), status) != last:
            say(f"    [{int(time.time()-started):4d}s] run {row.get('id')} {status}")
            last = (row.get("id"), status)
        if status in TERMINAL:
            break
        if time.time() - started > WATCH_CEILING:
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
        # Current address first (`WORKTREES_SUBDIR`, see the constant above), legacy sibling as a
        # fallback for a tree that predates the daemon's default moving. Before this the glob only
        # ever matched the legacy shape, so `found` came back empty for every worktree-mode layer —
        # H1, H2 and H3, three of the ladder's four — and this returned "no-worktree" instead of a
        # verdict, silently: `glob.glob` does not error on a shape nobody writes to anymore.
        found = (glob.glob(f"{tree}/{WORKTREES_SUBDIR}/run-*") or
                 glob.glob(f"{TREES}/nucleos-worktrees/{task}-{layer}/run-*"))
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
            if one in RETIRED:
                raise SystemExit(f"{one} is retired: {RETIRED[one]}")
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


def main(argv):
    results = []
    for task, layer in requested(argv):
        prompt = prompt_for(task)
        if True:
            project = f"eval-{task}-{layer}"
            tree = f"{TREES}/{task}-{layer}"
            say(f"\n===== {task} x {layer} =====")
            base = prepare(task, layer, tree)

            body = {"prompt": prompt, "cwd": tree, "mode": MODE[layer]}
            worktree = MODE[layer] == "worktree"
            if worktree:
                body["project_id"] = project
                # After `prepare`, not before: activation inspects the tree `layer.py` just wrote.
                try:
                    activate(project, tree, layer)
                except urllib.error.HTTPError as error:
                    say(f"    ativacao recusada: HTTP {error.code} {error.read().decode()[:200]}")
                    results.append((task, layer, "refused", None, None, None))
                    record(Cell(task, layer, "refused", base=base), [])
                    continue
            try:
                try:
                    created = call("/runs", "POST", body)
                except urllib.error.HTTPError as error:
                    say(f"    recusado: HTTP {error.code} {error.read().decode()[:200]}")
                    results.append((task, layer, "refused", None, None, None))
                    record(Cell(task, layer, "refused", base=base), [])
                    continue
                first_id = created["id"]
                say(f"    run {first_id} criado")

                approver = start_approver(project) if layer in ("H2", "H3") else None
                try:
                    outcome = watch(project if MODE[layer] != "real" else None, first_id)
                finally:
                    if approver:
                        approver.terminate()
            finally:
                # The chain is over, or never started: back to off either way, and before scoring,
                # so the project is in shadow for exactly as long as a run needed it.
                if worktree:
                    deactivate(project)

            # A chain the watcher gave up on is still writing to the tree it would score, so any
            # verdict read from it is about a half-edited file. It is recorded as what it is.
            verdict = (score(task, layer, tree, first_id) if outcome["status"] in TERMINAL
                       else "timed_out")
            turns = turns_of(outcome["runs"]) if outcome["runs"] else None
            say(f"    -> {verdict} | {outcome['wall']}s | ${outcome['cost']} | "
                f"turnos={turns} | runs={outcome['runs']}")
            results.append((task, layer, verdict, outcome["wall"], outcome["cost"], turns))
            record(Cell(task, layer, verdict, turns, outcome["cost"], outcome["wall"], base),
                   outcome["runs"] or [first_id])

    say("\n================ RESUMO ================")
    for row in results:
        say("\t".join("" if v is None else str(v) for v in row))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
