#!/usr/bin/env python3
"""Seed a cold gate target dir with registry-crate state from a warm sibling.

Every verify gate builds in `~/.nucleos/warm/<project>/<worktree_id>/target`, a directory that is new
for every worktree, so ~290 registry dependencies (build scripts and proc-macros included, which
sccache never caches) recompiled each time. Their fingerprints and artifacts do not depend on the
workspace path, so they can be taken from a recent sibling. Workspace crates never are: they rebuild
in a new checkout anyway and their fingerprints are path-keyed.

Absolute paths: a build script's `output` and `root-output` name the donor's target dir (link-search
paths, generated-file paths). Those two files are rewritten to the new target dir while copying.

Fails open: any error leaves the target dir as it was and the exit code 0. One line on stderr.

Usage: gate_seed.py <target_dir>
Env:   NUCLEOS_GATE_SEED=0 disables.
"""
import os
import shutil
import sys
import time

WORKSPACE_PREFIXES = ("nucleos",)  # packages, binaries and `nucleos_*` / `libnucleos_*` artifacts
MIN_AGE = 600.0  # a donor touched more recently than this may be a build in progress
MAX_AGE = 3 * 86400.0
LOCK_STALE = 1800.0
REWRITE_NAMES = ("output", "root-output")


def is_workspace(stem):
    if stem.startswith("lib") and stem[3:].startswith(WORKSPACE_PREFIXES):
        stem = stem[3:]
    return stem.startswith(WORKSPACE_PREFIXES)


def newest_mtime(path):
    newest = 0.0
    stack = [path]
    while stack:
        cur = stack.pop()
        try:
            with os.scandir(cur) as it:
                for e in it:
                    if e.is_dir(follow_symlinks=False):
                        stack.append(e.path)
                    else:
                        newest = max(newest, e.stat(follow_symlinks=False).st_mtime)
        except OSError:
            continue
    return newest


def find_donor(target_dir, now):
    """Most recently used sibling `<warm>/*/<target name>` that is neither busy nor stale."""
    own = os.path.abspath(target_dir)
    leaf = os.path.basename(own)
    warm = os.path.dirname(os.path.dirname(own))
    best = None
    try:
        names = os.listdir(warm)
    except OSError:
        return None
    for n in names:
        cand = os.path.join(warm, n, leaf)
        fp = os.path.join(cand, "debug", ".fingerprint")
        if os.path.abspath(cand) == own or not os.path.isdir(fp):
            continue
        age = now - newest_mtime(fp)
        if MIN_AGE < age < MAX_AGE and (best is None or age < best[0]):
            best = (age, cand)
    return best[1] if best else None


def _rewrite(src, dst, old_forms, new):
    with open(src, "rb") as f:
        text = f.read().decode("utf-8", "surrogateescape")
    for old in old_forms:
        text = text.replace(old, new)
    with open(dst, "wb") as f:
        f.write(text.encode("utf-8", "surrogateescape"))
    st = os.stat(src)
    os.utime(dst, ns=(st.st_atime_ns, st.st_mtime_ns))


def _copy_tree(src, dst, old_forms, new):
    os.makedirs(dst)
    for cur, dirs, files in os.walk(src):
        rel = os.path.relpath(cur, src)
        out = dst if rel == "." else os.path.join(dst, rel)
        for d in dirs:
            os.makedirs(os.path.join(out, d), exist_ok=True)
        for f in files:
            s, t = os.path.join(cur, f), os.path.join(out, f)
            if f in REWRITE_NAMES:
                _rewrite(s, t, old_forms, new)
            else:
                shutil.copy2(s, t)


def seed_from(donor, target_dir):
    """Copy registry state from donor/debug into target_dir/debug. Returns the entry count."""
    src, tmp = os.path.join(donor, "debug"), os.path.join(target_dir, "debug.seeding")
    new = os.path.abspath(target_dir)
    old = os.path.abspath(donor)
    # Longest first so a form that contains another is not half-replaced.
    old_forms = sorted({old, old.replace("\\", "/"), old.replace("/", "\\")}, key=len, reverse=True)
    if os.path.exists(tmp):
        shutil.rmtree(tmp)
    n = 0
    for sub in (".fingerprint", "build"):
        base = os.path.join(src, sub)
        if not os.path.isdir(base):
            continue
        os.makedirs(os.path.join(tmp, sub))
        for e in os.listdir(base):
            if is_workspace(e):
                continue
            s = os.path.join(base, e)
            if os.path.isdir(s):
                _copy_tree(s, os.path.join(tmp, sub, e), old_forms, new)
            else:
                shutil.copy2(s, os.path.join(tmp, sub, e))
            n += 1
    deps = os.path.join(src, "deps")
    if os.path.isdir(deps):
        os.makedirs(os.path.join(tmp, "deps"))
        for e in os.listdir(deps):
            s = os.path.join(deps, e)
            if is_workspace(e) or not os.path.isfile(s):
                continue
            t = os.path.join(tmp, "deps", e)
            try:
                os.link(s, t)
            except OSError:
                shutil.copy2(s, t)
            n += 1
    info = os.path.join(donor, ".rustc_info.json")
    if os.path.isfile(info) and not os.path.exists(os.path.join(target_dir, ".rustc_info.json")):
        shutil.copy2(info, os.path.join(target_dir, ".rustc_info.json"))
    os.rename(tmp, os.path.join(target_dir, "debug"))
    return n


def main(argv, env=os.environ, now=None):
    if len(argv) != 2:
        print("usage: gate_seed.py <target_dir>", file=sys.stderr)
        return 0
    target = argv[1]
    if env.get("NUCLEOS_GATE_SEED") == "0":
        return 0
    if os.path.exists(os.path.join(target, "debug")):
        return 0
    started = time.time()
    now = started if now is None else now
    lock = os.path.join(os.path.dirname(os.path.abspath(target)), "." + os.path.basename(target) + ".seed.lock")
    try:
        try:
            os.makedirs(os.path.dirname(lock), exist_ok=True)
            os.mkdir(lock)
        except FileExistsError:
            try:
                stale = time.time() - os.stat(lock).st_mtime > LOCK_STALE
            except OSError:
                stale = False
            if not stale:
                print("gates: seed skipped, another gate is seeding this dir", file=sys.stderr)
                return 0
            os.utime(lock)
        try:
            if os.path.exists(os.path.join(target, "debug")):
                return 0
            donor = find_donor(target, now)
            if donor is None:
                print("gates: seed skipped, no warm donor", file=sys.stderr)
                return 0
            os.makedirs(target, exist_ok=True)
            n = seed_from(donor, target)
            print(f"gates: seeded {n} entries from {donor} in {time.time() - started:.0f}s", file=sys.stderr)
        finally:
            try:
                os.rmdir(lock)
            except OSError:
                pass
    except Exception as exc:  # fail open
        print(f"gates: seed skipped ({exc})", file=sys.stderr)
        shutil.rmtree(os.path.join(target, "debug.seeding"), ignore_errors=True)
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
