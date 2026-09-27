#!/usr/bin/env python3
"""Put a candidate tree into one layer of the ablation ladder.

    python scripts/eval/layer.py --layer H0|H1|H2|H3 --tree <path>

Run it after `base.sh`, before the run. `base.sh` decides which TASK the tree is; this decides which
LAYER, and until it existed nothing did — which is not a gap in the documentation but the reason the
ablation has never measured anything.

**What went wrong without it.** `README.md` says the H1/H2 difference is the `hooks.PreToolUse` entry
in the candidate repository's `.claude/settings.json`, and that it "has to be committed in each
layer's candidate repository". `base.sh` hands over the file the reference commit carries, and every
reference commit registers the hook — so all four layers came out as H2+, and a completed ablation
would have been four names for one layer. Measured on 2026-08-16: a run launched as H0 stopped at
`awaiting_approval`, which an H0 has nobody to ask.

## What varies, and what deliberately does not

**Varies: `hooks.PreToolUse` only.** Present in H2 and H3, absent in H0 and H1. That single entry is
the whole difference the ladder claims between those layers, so it had better be the only thing this
touches.

**Nothing else, since the daemon moved its per-project state (2026-09-27).** This used to plant
`.ai/workflow/workflow.md` in H2 and H3, because activation looked for it, and write H3's gate into
`.ai/autopilot.yaml`. The daemon reads neither any more: activation asks for the project to have been
*onboarded*, which the daemon records in `~/.nucleos/projects/<id>/`, and the gate lives beside it.
Both are now done by `ladder.py` through the daemon's own `POST /projects/{id}/onboard`, with the
project id it registers — this file never knew that id, and never had a daemon to ask. What it keeps
is only what varies per layer in the TREE; the gate a layer asks for is `gate_for(layer)` below.

**Constant: `permissions.allow`.** Every layer gets the same list, and that is a decision worth
stating because the opposite reading is tempting. `ABLATION.md` worried that enumerating allowed
commands would "make that list part of the harness, arbitrary, biasing exactly what we want to
measure" — true of a list only SOME layers get, which is a second variable wearing the first one's
name. Given to all four identically it is a constant, and constants do not bias comparisons.

It has to be given. Without the hook a run inherits the project's own `permissions.allow`, written
for interactive work where a person clicks; nothing in it allows `cargo`. That is the 2026-07-30
blocker, and it is not a property of the harness — it is the CLI's permission UX, which is not what
this ladder measures. An H0 that cannot compile does not measure a weaker layer, it measures nothing,
and it does so while looking like a result.

**Except that the list has never once taken effect, and this file said otherwise for three weeks.**
Every eval run so far — 900265, 900268, 900270, 900272, all four layers — printed the same line on
stderr, which nobody read until 2026-08-17:

    Ignoring 16 permissions.allow entries from .claude/settings.json: this workspace has not been
    trusted. Run Claude Code interactively here once and accept the trust dialog, or set
    projects["<tree>"].hasTrustDialogAccepted: true in ~/.claude.json

A candidate tree is a fresh directory that no human has ever opened, so it is untrusted by
construction and the CLI drops the whole list. The runs work anyway, which is why this went unnoticed:
the daemon launches autonomous runs with permissions bypassed, so the allow list was never what was
letting `cargo` through. It is inert in all four layers equally, so it biases no comparison — the
paragraph above is right about the constant and wrong about the mechanism. Left in place deliberately:
removing it would make the trees differ from every row already recorded, for a list that does nothing.
Whoever wants it to bite must set `hasTrustDialogAccepted` per tree, and must then re-run the whole
ladder, because that changes what H0 and H1 are.

**Removed everywhere: `hooks.PostToolUse`.** It runs `.ai/scripts/log_event.py`, and `.ai/` is
gitignored, so that file is in no candidate tree of any layer. Left in place it fires and fails on
every Bash call in every layer — noise, equal across layers, but noise inside the thing being timed.

**Constant: one shared, pre-warmed `CARGO_TARGET_DIR`,** written into the tree as `.cargo/config.toml`
so it travels into the worktree the daemon makes. This was the first thing measured that did not
compare: T1×H0 ran against a target directory I had warmed by hand and T1×H1 against a cold one, and
$1.59 vs $2.93 was in good part the price of a dependency graph, not of a layer.

It is not really a choice. `README.md` already prescribes the shared directory for the scorer and
says why — a per-tree target costs ~12 GB, and this machine has 20.9 GB free. Two cold cells fill the
disk; a twelve-cell ladder was never going to run. Sharing makes every layer pay the same one
workspace-crate compile and none of them the ~500-crate dependency build.

**But not the project's own `target/`, which is what `README.md` gives the scorer.** The scorer runs
when nothing else does; a run does not. Pointing cells at the live repository's target directory was
tried here first and is wrong twice over:

- **Cargo locks the build directory.** Measured while preparing this: `Blocking waiting for file
  lock on build directory`, because the other session was building. Every cell's wall clock would
  then include however long somebody else's compile took, and wall clock is half of what the ladder
  reports.
- **It lets a live repository's binary run inside a cell.** Cargo's artifact hash for this package
  does not include the workspace path, so every tree writes the same `nucleos_core-<hash>` and shares
  one fingerprint; only mtimes separate them. A build in the live repo mid-run leaves artifacts newer
  than the cell's untouched sources, and cargo calls the cell fresh and runs the other binary. That is
  not hypothetical — `README.md` records a gate reporting green in 0.87s while running a test its tree
  did not contain. Its blast radius here is the agent's first `cargo test`, before it has edited
  anything, which is exactly the observation an agent decides its approach from.

Hence a directory of the eval's own, built once and shared by every cell, and torn down after. The
same mtime trap still applies between cells, so this stamps the tree — last, after the commit, since
`git commit` does not touch working-tree mtimes.

The stamp also decides something the ladder reports. A worktree is checked out fresh and is newborn
by construction, so H1–H3 each pay one workspace-crate compile inside their own clock. Stamped, H0
pays it too. That is deliberate: unstamped, H0 would be the one layer allowed to run a binary built
from another tree, and buying a fair comparison with a stale-artifact bug is not a trade worth making.

The related fix, which outlives the choice above: the daemon runs from a copy at
`C:/Projects/nucleos-daemon/`, not from `target/debug/nucleos-core.exe`. `nucleos-core` is bin-only,
so anything building it relinks that path, and Windows denies the write while the daemon holds it
open. The token is state, not a property of the binary, so the copy prints the same one.

**H3 only: an exit gate**, `GATE_COMMAND`, handed to the daemon by `ladder.py` when it onboards
the cell's project — see `gate_for`. It is read before the worktree exists, from the project's state
directory, which is what stops an agent from repointing its own gate.

The command names Git's bash by absolute path, quoted. `bash` alone resolves to `C:\Windows\System32\
bash.exe` on this machine — WSL, a different operating system with no Windows cargo in it — and H3
would then measure a gate that could never have gone green. `split_command` keeps quoted groups
together, so the space in `Program Files` survives.

## What this does NOT do

It does not choose the run's mode. H0 is `mode: real` and H1-H3 are `mode: worktree`; that is an
argument to the run, and it is named in the summary this prints so the caller cannot forget it.
"""

import argparse
import json
import os
import subprocess
import sys

# The layers that carry the PreToolUse gate. The only thing this file varies.
LAYERS_WITH_HOOK = ("H2", "H3")
LAYERS = ("H0", "H1", "H2", "H3")

# The run mode each layer is launched with. Printed, never applied — it belongs to the run.
LAYER_MODE = {"H0": "real", "H1": "worktree", "H2": "worktree", "H3": "worktree"}

# The one target directory every layer builds into. The eval's own, not the project's — see the
# module docstring for the two ways sharing the live repository's target corrupts a cell.
SHARED_TARGET_DIR = "C:/Projects/nucleos-eval-target"

# H3's exit gate. Git's bash by absolute path — see the docstring on why the bare name is a trap.
GATE_COMMAND = '"C:/Program Files/Git/bin/bash.exe" scripts/gates.sh core'


def write_cargo_config(tree: str) -> str:
    """Point this tree's cargo at the shared target directory.

    In the tree rather than in the run's environment because the worktree layers do not run where
    this script runs — the daemon checks the candidate out somewhere else, and a variable exported
    here would not be there. A committed `.cargo/config.toml` is found by cargo walking up from
    wherever the crate ends up.
    """
    config_dir = os.path.join(tree, ".cargo")
    config_path = os.path.join(config_dir, "config.toml")
    if os.path.exists(config_path):
        # No base of this repository carries one today. If one ever does it will hold real settings,
        # and merging TOML by hand is how those get silently dropped — so stop and let a person look.
        raise SystemExit(f"layer.py: {config_path} already exists; it would be overwritten. "
                         f"Merge the target-dir into it by hand and re-run.")
    os.makedirs(config_dir, exist_ok=True)
    with open(config_path, "w", encoding="utf-8", newline="\n") as handle:
        handle.write("# Written by scripts/eval/layer.py — see its docstring.\n"
                     "[build]\n"
                     f'target-dir = "{SHARED_TARGET_DIR}"\n')
    return config_path


def gate_for(layer: str):
    """The exit gate this layer's project is onboarded with: H3's, and nobody else's.

    `ladder.py` sends it as the confirmed `gate_command` of `POST /projects/{id}/onboard`, which
    stores it where the daemon reads it. `None` is "no gate confirmed", and leaves the project's
    rules as they are — an H2 cell has none.
    """
    return GATE_COMMAND if layer == "H3" else None


def stamp(tree: str) -> int:
    """Give every file a current mtime, so cargo cannot mistake this tree for one it already built.

    Last, after the commit: `git commit` does not touch working-tree mtimes, and this has to be the
    newest thing that happened to the tree before the run starts.
    """
    count = 0
    for directory, _, names in os.walk(tree):
        if ".git" in directory.split(os.sep):
            continue
        for name in names:
            try:
                os.utime(os.path.join(directory, name), None)
                count += 1
            except OSError:
                pass
    return count


def ensure_repository(tree: str) -> str:
    """Make the candidate a git repository, because `mode: worktree` cannot start without one.

    `materialize.sh` lays the tree out with `git archive | tar -x` and says why in its own comment:
    an extraction reads and nothing else, where `git worktree add` would write to shared `.git`
    state on a machine somebody is working on. That is right for what it was for — H0 and the
    scorer, which both want a directory of files.

    It is not enough for the rest of the ladder, and nothing said so. `create_run_inner` refuses
    `worktree` mode without a `project_id` and a `cwd`, and `worktree.rs` then makes a worktree OF
    that cwd — so H1, H2 and H3 each need the candidate to be a repository with the base at HEAD.
    Three of the four layers could not have been launched at all.

    One commit, made here rather than by hand, so the tree the agent is handed is the tree the base
    describes and nothing about which files were staged is left to whoever ran it. `target/` is
    excluded by the repository's own committed `.gitignore`, which the archive carries — so this
    stays cheap even after a pre-warm.
    """
    if os.path.isdir(os.path.join(tree, ".git")):
        head = subprocess.run(["git", "-C", tree, "rev-parse", "--short", "HEAD"],
                              capture_output=True, text=True)
        return f"already a repository at {head.stdout.strip() or 'an unborn HEAD'}"

    for command in (
        ["git", "-C", tree, "init", "-q"],
        # Named locally: the eval must not inherit whoever's identity happens to be configured, and
        # a repository with no identity refuses to commit at all.
        ["git", "-C", tree, "config", "user.email", "eval@nucleos.invalid"],
        ["git", "-C", tree, "config", "user.name", "nucleos eval"],
        ["git", "-C", tree, "add", "-A"],
        ["git", "-C", tree, "commit", "-q", "-m", "eval base"],
    ):
        done = subprocess.run(command, capture_output=True, text=True)
        if done.returncode != 0:
            raise SystemExit(f"layer.py: {' '.join(command[3:])} failed: "
                             f"{(done.stderr or done.stdout).strip()[:200]}")
    head = subprocess.run(["git", "-C", tree, "rev-parse", "--short", "HEAD"],
                          capture_output=True, text=True)
    return f"initialised, base committed at {head.stdout.strip()}"

# Identical in every layer, on purpose. Enough to build, test and read the tree, and nothing that
# reaches outside it: no network, no installs, no writes anywhere but the candidate tree.
EVAL_ALLOW = [
    "Bash(cargo *)",
    "Bash(rustc *)",
    "Bash(rustup *)",
    "Bash(bash scripts/gates.sh *)",
    "Bash(git status*)",
    "Bash(git diff*)",
    "Bash(git log*)",
    "Bash(git show*)",
    "Bash(ls *)",
    "Bash(cat *)",
    "Bash(head *)",
    "Bash(tail *)",
    "Bash(grep *)",
    "Bash(rg *)",
    "Bash(find *)",
    "Bash(wc *)",
]


def main() -> int:
    parser = argparse.ArgumentParser(description="Put a candidate tree into one ablation layer.")
    parser.add_argument("--layer", required=True, choices=LAYERS)
    parser.add_argument("--tree", required=True, help="the candidate tree, as base.sh laid it out")
    parser.add_argument("--print-only", action="store_true", help="report, change nothing")
    args = parser.parse_args()

    settings_path = os.path.join(args.tree, ".claude", "settings.json")
    if not os.path.exists(settings_path):
        # Not created from nothing: a tree without it is not a tree base.sh produced from a
        # reference of this repository, and guessing at one would silently invent the layer.
        print(f"layer.py: no {settings_path} — is --tree a tree base.sh laid out?", file=sys.stderr)
        return 2

    with open(settings_path, encoding="utf-8") as handle:
        settings = json.load(handle)

    hooks = settings.get("hooks", {})
    had_pre = "PreToolUse" in hooks
    had_post = "PostToolUse" in hooks

    if args.layer in LAYERS_WITH_HOOK:
        if not had_pre:
            # The reference always carries it, so its absence means the tree was already put into a
            # lower layer. Refused rather than reconstructed: this file does not know what the hook
            # entry should say, and a wrong one would be an H2 that governs nothing.
            print("layer.py: this tree has no PreToolUse to keep — it is already H0/H1; "
                  "rebuild it with base.sh before asking for H2/H3", file=sys.stderr)
            return 2
    else:
        hooks.pop("PreToolUse", None)

    hooks.pop("PostToolUse", None)
    settings["hooks"] = hooks
    settings.setdefault("permissions", {})["allow"] = list(EVAL_ALLOW)

    if args.print_only:
        print(json.dumps(settings, indent=2))
        return 0

    with open(settings_path, "w", encoding="utf-8", newline="\n") as handle:
        json.dump(settings, handle, indent=2)
        handle.write("\n")

    # Before the commit: the worktree layers only ever see what HEAD carries.
    config_path = write_cargo_config(args.tree)

    # Only the worktree layers need it, and H0 is deliberately left as the plain directory
    # `materialize.sh` argues for.
    repo_note = None
    if LAYER_MODE[args.layer] == "worktree":
        repo_note = ensure_repository(args.tree)

    stamped = stamp(args.tree)

    print(f"layer.py: {args.tree} is now {args.layer}")
    if repo_note:
        print(f"  git repository: {repo_note} — worktree mode cannot start without one")
    print(f"  cargo target-dir: {SHARED_TARGET_DIR} (via {os.path.relpath(config_path, args.tree)})"
          f" — shared and pre-warmed, identical in every layer")
    print(f"  stamped {stamped} files — without it cargo can call this tree fresh and run another's "
          f"binary")
    if gate_for(args.layer):
        print(f"  gate_command: {GATE_COMMAND}")
        print("    confirmed by ladder.py when it onboards this cell's project — stored by the daemon "
              "in the project's state directory, never in the tree")
    print(f"  PreToolUse:  {'kept' if args.layer in LAYERS_WITH_HOOK else 'removed'}"
          f" (was {'present' if had_pre else 'absent'})")
    print(f"  PostToolUse: {'removed' if had_post else 'absent'} — its script is gitignored, so it "
          f"is in no candidate tree")
    print(f"  permissions.allow: {len(EVAL_ALLOW)} entries, identical in every layer")
    print(f"  launch this run with mode={LAYER_MODE[args.layer]}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
