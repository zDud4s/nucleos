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

**Removed everywhere: `hooks.PostToolUse`.** It runs `.ai/scripts/log_event.py`, and `.ai/` is
gitignored, so that file is in no candidate tree of any layer. Left in place it fires and fails on
every Bash call in every layer — noise, equal across layers, but noise inside the thing being timed.

## What this does NOT do

H3 is H2 plus the exit gate, which `README.md` places in `gate_command:` in the `.ai/autopilot.yaml`
of the PROJECT ROOT — daemon-side configuration, not a property of this tree. It is not written here
because writing it into the tree would put it somewhere the daemon does not read, which is worse than
leaving it undone: the layer would look prepared and would be H2.

Nor does it choose the run's mode. H0 is `mode: real` and H1-H3 are `mode: worktree`; that is an
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

    # Only the worktree layers need it, and H0 is deliberately left as the plain directory
    # `materialize.sh` argues for.
    repo_note = None
    if LAYER_MODE[args.layer] == "worktree":
        repo_note = ensure_repository(args.tree)

    print(f"layer.py: {args.tree} is now {args.layer}")
    if repo_note:
        print(f"  git repository: {repo_note} — worktree mode cannot start without one")
    print(f"  PreToolUse:  {'kept' if args.layer in LAYERS_WITH_HOOK else 'removed'}"
          f" (was {'present' if had_pre else 'absent'})")
    print(f"  PostToolUse: {'removed' if had_post else 'absent'} — its script is gitignored, so it "
          f"is in no candidate tree")
    print(f"  permissions.allow: {len(EVAL_ALLOW)} entries, identical in every layer")
    print(f"  launch this run with mode={LAYER_MODE[args.layer]}"
          + ("  and a gate_command in the PROJECT ROOT's .ai/autopilot.yaml"
             if args.layer == "H3" else ""))
    return 0


if __name__ == "__main__":
    sys.exit(main())
