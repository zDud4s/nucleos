#!/usr/bin/env python3
"""tree.py resource verbs: budget, open, close, gate (spec 2026-10-21, sections 6-8).

The controller is the only thing that spends disk, so these verbs measure it, decide how many
branches fit, and own the worktree/target-dir lifecycle. Every measurement goes through a module
level function (disk_free_gb, dir_size_gb, heavy_status, run, find_core) so tests replace it.
"""
from __future__ import annotations

import json
import os
import shutil
import subprocess
import sys
from pathlib import Path

from tree_state import (  # noqa: F401  (load_* are re-bound here so tests can patch them)
    TreeError,
    add_tree_arg,
    dev_home,
    load_config,
    load_manifest,
    now_iso,
    parse_iso,
    resolve_tree_id,
    save_manifest,
)

GB = 1024 ** 3
# States in which a branch owns a worktree and a target dir that may still grow.
OPEN_STATES = {"open", "building", "gating", "green", "red", "reviewing", "fixing"}
DEFAULT_WORKTREES_DIR = ".nucleos/worktrees"
LOG_TAIL_LINES = 40
SAMPLE_CAP = 500
IS_WINDOWS = os.name == "nt"


# --- measurement seams ------------------------------------------------------------------------

def disk_free_gb(path):
    """Free GB on the volume holding `path` (or its nearest existing parent)."""
    p = Path(path)
    while not p.exists() and p != p.parent:
        p = p.parent
    return shutil.disk_usage(p).free / GB


def _is_link(path):
    if os.path.islink(path):
        return True
    isjunction = getattr(os.path, "isjunction", None)
    return bool(isjunction and isjunction(path))


def dir_size_gb(path):
    """Size of a directory in GB, never following links or junctions."""
    total = 0
    for base, dirs, files in os.walk(path, followlinks=False):
        dirs[:] = [d for d in dirs if not _is_link(os.path.join(base, d))]
        for name in files:
            try:
                total += os.lstat(os.path.join(base, name)).st_size
            except OSError:
                pass
    return total / GB


def heavy_status(cfg, project_root):
    """`heavy.py status --json` as a dict, or None when unconfigured, absent or failing."""
    heavy = cfg.get("heavy")
    if not heavy:
        return None
    script = Path(heavy)
    if not script.is_absolute():
        script = Path(project_root) / script
    if not script.is_file():
        return None
    try:
        proc = subprocess.run(
            [sys.executable, str(script), "status", "--json"],
            cwd=str(project_root), capture_output=True, text=True, timeout=10,
        )
        if proc.returncode != 0:
            return None
        data = json.loads(proc.stdout)
    except (OSError, subprocess.TimeoutExpired, ValueError):
        return None
    return data if isinstance(data, dict) else None


def run(cmd, cwd=None):
    """Run a command, return (returncode, stdout+stderr). Never raises."""
    try:
        proc = subprocess.run(cmd, cwd=cwd, capture_output=True, text=True, timeout=300)
    except (OSError, subprocess.TimeoutExpired) as exc:
        return 1, str(exc)
    return proc.returncode, (proc.stdout + proc.stderr).strip()


def find_core():
    """The staged nucleos-core binary, searched like .githooks/post-checkout (dev home run dir)."""
    for name in ("nucleos-core.exe", "nucleos-core"):
        candidate = Path(dev_home()) / "run" / name
        if candidate.is_file():
            return str(candidate)
    return None


# --- small helpers ----------------------------------------------------------------------------

def _tree_id(args):
    """--tree, else $NUCLEOS_TREE_ID, else the only open tree (tree_state's resolver)."""
    return resolve_tree_id(args)


def _norm(path):
    text = str(path).replace("\\", "/").rstrip("/")
    return text.lower() if IS_WINDOWS else text


def _cost(cfg, m, layer):
    measured = (m.get("costs_gb") or {}).get(layer)
    return float(measured) if measured is not None else float(cfg.get("seed_gb_per_layer", 8))


def _free(cfg, project_root):
    """Free GB: the smaller of the dev home volume and the project root volume (spec 7)."""
    return min(disk_free_gb(str(dev_home())), disk_free_gb(str(project_root)))


def _sample(m, free_gb, verb):
    samples = m.setdefault("disk_samples", [])
    samples.append({"at": now_iso(), "free_gb": round(free_gb, 2), "verb": verb})
    del samples[:-SAMPLE_CAP]


def _ready(m, branch):
    base = branch.get("base")
    return base is None or (m.get("branches", {}).get(base) or {}).get("state") == "merged"


def _target_dir(tree_id, name):
    return str(Path(dev_home()) / "cargo-target" / f"{tree_id}-{name}")


# --- budget -----------------------------------------------------------------------------------

def compute_budget(cfg, m, project_root, free_gb):
    """Spec 7: how many queued branches may open now. No side effects."""
    floor = float(cfg.get("floor_gb", 15))
    cap = int(cfg.get("max_live_branches", 4))
    branches = m.get("branches", {})
    live = [b for b in branches.values() if b.get("state") in OPEN_STATES]

    growth = 0.0
    for b in live:
        size = dir_size_gb(b["target_dir"]) if b.get("target_dir") else 0.0
        growth += max(0.0, _cost(cfg, m, b.get("layer")) - size)
    slack = free_gb - floor - growth

    reasons, can_open = [], []
    cleanup = [b["target_dir"] for b in branches.values()
               if b.get("state") == "merged" and b.get("target_dir")]
    below_floor = free_gb < floor

    if below_floor:
        reasons.append(
            f"disco: {free_gb:.1f} GB free is below the {floor:.0f} GB floor; "
            "open no branch and start no gate"
        )
        if cleanup:
            reasons.append(
                "disco: clean only the target dirs of merged branches: " + ", ".join(cleanup)
            )
        else:
            reasons.append("disco: nothing of ours left to clean; park the tree with --needs disco")
    else:
        room = cap - len(live)
        remaining = slack
        for name, b in branches.items():
            if b.get("state") != "queued":
                continue
            if not _ready(m, b):
                reasons.append(f"{name}: waiting for base {b.get('base')} to be merged")
                continue
            if room <= 0:
                reasons.append(f"max_live_branches {cap} reached")
                break
            cost = _cost(cfg, m, b.get("layer"))
            if cost > 0 and int(remaining // cost) < 1:
                reasons.append(
                    f"disco: {name} needs {cost:.1f} GB, the slack left is {remaining:.1f} GB"
                )
                break
            can_open.append(name)
            remaining -= cost
            room -= 1

    heavy = heavy_status(cfg, project_root)
    land_waiting = False
    if heavy is None:
        gates = 1
    else:
        gates = max(0, (int(heavy.get("capacity", 0)) - int(heavy.get("held_weight", 0))) // 2)
        root = _norm(project_root)
        for lock in heavy.get("locks") or []:
            if (lock.get("alive") and _norm(lock.get("worktree", "")) == root
                    and lock.get("pid") not in (os.getpid(), os.getppid())):
                land_waiting = True
        if land_waiting:
            reasons.append("land: the main checkout is locked by another session; do not gate there")
    if below_floor:
        gates = 0

    return {
        "free_gb": round(free_gb, 2), "floor_gb": floor, "slack_gb": round(slack, 2),
        "can_open": can_open, "gates": gates, "land_waiting": land_waiting,
        "land_wait_minutes": 0, "reasons": reasons, "cleanup": cleanup,
    }


def _track_land_wait(cfg, m, result):
    """Spec 7.1 item 2: remember when the land lock was first seen; past lock_wait_minutes, switch."""
    if not result["land_waiting"]:
        m.pop("land_waiting_since", None)
        return
    now = now_iso()
    since = m.setdefault("land_waiting_since", now)
    minutes = int((parse_iso(now) - parse_iso(since)).total_seconds() // 60)
    result["land_wait_minutes"] = minutes
    if minutes > int(cfg.get("lock_wait_minutes", 10)):
        result["reasons"].append(f"land à espera há {minutes} min: passar a outro ramo")


def run_budget(args):
    cfg = load_config()
    tid = _tree_id(args)
    m = load_manifest(tid)
    root = m["project_root"]
    free = _free(cfg, root)
    result = compute_budget(cfg, m, root, free)
    _track_land_wait(cfg, m, result)
    _sample(m, free, "budget")
    save_manifest(tid, m)
    return result


# --- gate -------------------------------------------------------------------------------------

def _branch(m, name):
    b = m.get("branches", {}).get(name)
    if b is None:
        raise TreeError(f"unknown branch {name!r}")
    return b


def run_gate(args):
    """Register a gate the controller ran: log tail, disk sample, state, first-green cost."""
    cfg = load_config()
    tid = _tree_id(args)
    m = load_manifest(tid)
    b = _branch(m, args.branch)
    try:
        text = Path(args.log).read_text(encoding="utf-8", errors="replace")
    except OSError as exc:
        raise TreeError(f"cannot read the gate log {args.log}: {exc}")
    free = _free(cfg, m["project_root"])
    record = {
        "at": now_iso(), "exit": args.exit_code,
        "log_tail": "\n".join(text.splitlines()[-LOG_TAIL_LINES:]),
        "sha": args.sha, "free_gb": round(free, 2),
    }
    b.setdefault("gates", []).append(record)
    b["state"] = "green" if args.exit_code == 0 else "red"
    _sample(m, free, "gate")

    measured = None
    costs = m.setdefault("costs_gb", {})
    layer = b.get("layer")
    if args.exit_code == 0 and layer is not None and costs.get(layer) is None and b.get("target_dir"):
        size = round(dir_size_gb(b["target_dir"]), 2)
        if size > 0:
            costs[layer] = measured = size
    save_manifest(tid, m)

    reasons = []
    floor = float(cfg.get("floor_gb", 15))
    if free < floor:
        reasons.append(
            f"disco: {free:.1f} GB free is below the {floor:.0f} GB floor; start no new gate"
        )
    return {
        "branch": args.branch, "state": b["state"], "exit": args.exit_code,
        "free_gb": round(free, 2), "cost_measured": measured, "reasons": reasons,
    }


# --- links, workflow sync, open ---------------------------------------------------------------

def create_link(src, dst, cwd):
    """Junction on Windows (`mklink /J`), symlink elsewhere."""
    if IS_WINDOWS:
        rc, out = run(["cmd", "/c", "mklink", "/J", str(dst), str(src)], cwd)
        if rc != 0:
            raise TreeError(f"could not link {dst}: {out}")
        return
    try:
        os.symlink(str(src), str(dst), target_is_directory=True)
    except OSError as exc:
        raise TreeError(f"could not link {dst}: {exc}")


def remove_link(path, cwd):
    """Undo a link WITHOUT following it. `rmdir`/`unlink` refuse a real non-empty directory."""
    if IS_WINDOWS:
        rc, out = run(["cmd", "/c", "rmdir", str(path)], cwd)
        if rc != 0:
            raise TreeError(f"could not unlink {path}: {out}")
        return
    try:
        os.unlink(str(path))
    except OSError as exc:
        raise TreeError(f"could not unlink {path}: {exc}")


def _workflow_sync(worktree):
    core = find_core()
    if not core:
        return "skipped: no staged nucleos-core in the dev home run dir"
    rc, out = run([core, "--workflow-sync", str(worktree)], str(worktree))
    return "ok" if rc == 0 else f"failed: {out or 'rc ' + str(rc)}; re-run it once the daemon is up"


def run_open(args):
    cfg = load_config()
    tid = _tree_id(args)
    m = load_manifest(tid)
    name = args.branch
    b = _branch(m, name)
    if b.get("state") != "queued":
        raise TreeError(f"branch {name!r} is {b.get('state')}, only a queued branch can be opened")

    root = m["project_root"]
    free = _free(cfg, root)
    budget = compute_budget(cfg, m, root, free)
    _sample(m, free, "budget")
    if name not in budget["can_open"]:
        save_manifest(tid, m)
        raise TreeError(
            f"the budget does not allow opening {name!r}: "
            + ("; ".join(budget["reasons"]) or "no room")
        )

    worktree = Path(root) / (cfg.get("worktrees_dir") or DEFAULT_WORKTREES_DIR) / f"{tid}-{name}"
    if worktree.exists():
        raise TreeError(f"{worktree} already exists")
    git_branch = f"tree/{tid}/{name}"
    feature = m["feature_branch"]

    rc, _ = run(["git", "rev-parse", "--verify", "--quiet", f"refs/heads/{feature}"], root)
    if rc != 0:
        rc, out = run(["git", "branch", feature, cfg.get("base_branch", "master")], root)
        if rc != 0:
            raise TreeError(f"cannot create the feature branch {feature}: {out}")
    rc, out = run(["git", "worktree", "add", "-b", git_branch, str(worktree), feature], root)
    if rc != 0:
        raise TreeError(f"git worktree add failed: {out}")

    made, skipped = [], []
    try:
        for link in cfg.get("links") or []:
            src = Path(root) / link
            if not src.exists():
                skipped.append(link)
                continue
            dst = worktree / link
            dst.parent.mkdir(parents=True, exist_ok=True)
            create_link(src, dst, root)
            made.append(dst)
    except TreeError:
        clean = True
        for dst in reversed(made):
            try:
                remove_link(dst, root)
            except TreeError:
                clean = False
        if clean:  # never force-remove a tree that still holds a link into the main checkout
            run(["git", "worktree", "remove", "--force", str(worktree)], root)
            run(["git", "branch", "-D", git_branch], root)  # the branch `worktree add -b` created
        save_manifest(tid, m)  # keeps the disk sample of this attempt
        raise

    sync = _workflow_sync(worktree)
    target = _target_dir(tid, name)
    b.update(state="open", git_branch=git_branch, worktree=str(worktree), target_dir=target)
    save_manifest(tid, m)
    return {
        "branch": name, "worktree": str(worktree), "git_branch": git_branch,
        "env": {"CARGO_TARGET_DIR": target}, "skipped_links": skipped, "workflow_sync": sync,
        "orchestrator_piece": {
            "branch": name, "layer": b.get("layer"), "paths": b.get("paths", []),
            "worktree": str(worktree), "git_branch": git_branch, "target_dir": target,
        },
    }


# --- close (spec 8.3) -------------------------------------------------------------------------

def remove_tree(path):
    shutil.rmtree(str(path))


def _check_target(target):
    """A target dir is only ever deleted when it is strictly inside <dev_home>/cargo-target."""
    base = os.path.normcase(os.path.realpath(Path(dev_home()) / "cargo-target"))
    cand = os.path.normcase(os.path.realpath(target))
    try:
        inside = cand != base and os.path.commonpath([base, cand]) == base
    except ValueError:  # different drives
        inside = False
    if not inside:
        raise TreeError(f"refusing to delete {target}: not under {base}")


def run_close(args):
    """Safe teardown, in this order: links, git worktree, target dir, branch if merged.

    A link that cannot be undone aborts before anything else happens: deleting the worktree
    would otherwise follow the junction into the main checkout.
    """
    cfg = load_config()
    tid = _tree_id(args)
    m = load_manifest(tid)
    b = _branch(m, args.branch)
    root = m["project_root"]
    worktree, target, git_branch = b.get("worktree"), b.get("target_dir"), b.get("git_branch")
    if not worktree:
        raise TreeError(f"branch {args.branch!r} has no worktree (never opened, or already closed)")
    if target:
        _check_target(target)  # validate before touching anything

    unlinked = []
    for link in cfg.get("links") or []:
        path = Path(worktree) / link
        if os.path.lexists(path):
            remove_link(path, root)  # raises TreeError: abort, nothing else is done
            unlinked.append(link)

    if os.path.isdir(worktree):
        rc, out = run(["git", "worktree", "remove", str(worktree)], root)
        if rc != 0:
            raise TreeError(f"git worktree remove failed (links already undone): {out}")
    else:
        run(["git", "worktree", "prune"], root)

    if target and os.path.lexists(target):
        remove_tree(target)

    # `git branch -d` would test against master; the branch lands in the feature branch instead.
    deleted = False
    if b.get("state") == "merged" and git_branch:
        rc, _ = run(["git", "merge-base", "--is-ancestor", git_branch, m["feature_branch"]], root)
        if rc == 0:
            rc, _ = run(["git", "branch", "-D", git_branch], root)
            deleted = rc == 0

    b["worktree"] = None
    b["target_dir"] = None
    save_manifest(tid, m)
    return {
        "branch": args.branch, "unlinked": unlinked, "branch_deleted": deleted,
        "target_dir_removed": bool(target),
    }


def register(verbs):
    """Add budget, open, close and gate to the tree.py subparsers."""
    def verb(name, help_text):
        p = verbs.add_parser(name, help=help_text)
        add_tree_arg(p)
        return p

    verb("budget", "how many queued branches fit now (spec 7)").set_defaults(func=run_budget)

    p = verb("open", "create the worktree, links and target dir of a queued branch")
    p.add_argument("branch")
    p.set_defaults(func=run_open)

    p = verb("close", "safe teardown of a branch worktree (spec 8.3)")
    p.add_argument("branch")
    p.set_defaults(func=run_close)

    p = verb("gate", "register the result of a gate the controller ran")
    p.add_argument("branch")
    p.add_argument("--exit", dest="exit_code", type=int, required=True)
    p.add_argument("--log", required=True)
    p.add_argument("--sha", required=True)
    p.set_defaults(func=run_gate)
