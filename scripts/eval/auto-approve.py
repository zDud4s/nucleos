#!/usr/bin/env python3
"""Answer the approval prompts an ablation run raises, and write down every answer.

**Why this is in the harness and not in the daemon.** H2 and H3 stop at `awaiting_approval`: the
hook fires, the classifier files an `action-approval`, and the run waits for a person. That is the
design working, and `ABLATION.md` records the previous decision to widen a safety layer as the
owner's to make. So nothing here changes what the daemon does. The approvals exist only while
somebody is running this script, and only for the runs they named.

**Scoped to explicit run ids, always.** The proposal table is shared with the owner's real work, so
an approver that took "every pending proposal" would answer questions nobody asked it about, from
another project, at two in the morning. `--run` is required and there is no `--all`.

**Every approval is recorded.** An ablation in which the harness quietly approved forty actions is
not measuring the harness. The JSONL this appends is part of the run's evidence: id, run, tool,
and the classifier's own reasoning, so a result can be read back knowing exactly what was waved
through.

Usage:
  python scripts/eval/auto-approve.py --run 900067 --run 900068 [--dry-run]
  python scripts/eval/auto-approve.py --run 900067 --until-idle 20 --max 200
"""

import argparse
import json
import os
import subprocess
import sys
import time
import urllib.error
import urllib.request

DEFAULT_DAEMON_URL = "http://127.0.0.1:8791"


def control_token(root: str) -> str:
    """The daemon's token, read the way every other local tool reads it.

    The environment comes first, exactly as `.claude/hooks/ask_daemon.py` reads it: a caller
    already holding a token should not need a built binary for permission to hand it over.
    Without that door this needs `target/` to exist, which describes a developer's tree and not
    a checkout — so the gate around it passed here and failed in CI, where the hooks job
    installs Python and builds nothing.
    """
    from_env = os.environ.get("NUCLEOS_DAEMON_TOKEN")
    if from_env:
        return from_env

    for profile in ("debug", "release"):
        binary = os.path.join(root, "target", profile, "nucleos-core.exe")
        if not os.path.exists(binary):
            binary = os.path.join(root, "target", profile, "nucleos-core")
        if os.path.exists(binary):
            printed = subprocess.run(
                [binary, "--print-token"], capture_output=True, text=True
            )
            if printed.returncode == 0 and printed.stdout.strip():
                return printed.stdout.strip()
    sys.exit("could not read the daemon control token — is the daemon built?")


def call(url: str, token: str, method: str = "GET"):
    request = urllib.request.Request(
        url,
        headers={"Authorization": f"Bearer {token}", "Content-Type": "application/json"},
        method=method,
        data=b"" if method == "POST" else None,
    )
    with urllib.request.urlopen(request, timeout=30) as response:
        body = response.read()
    return json.loads(body) if body else None


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--run",
        type=int,
        action="append",
        help="a run whose approvals to answer; repeatable. There is deliberately no --all.",
    )
    parser.add_argument(
        "--project",
        action="append",
        help="a project whose runs' approvals to answer; repeatable. Required for H2/H3, because "
        "approving does not resume the run it approved — `resume_after_approval` marks that run "
        "`superseded` and INSERTS a successor with a NEW id. A `--run 900269` approver answers the "
        "first question and then goes deaf, and the cell dies at its wall clock looking like a "
        "timeout. Still scoped, which is the property that mattered: `eval-T1-H2` is the eval's own "
        "project and nobody else's work is in it.",
    )
    parser.add_argument("--daemon-url", default=os.environ.get("NUCLEOS_DAEMON_URL", DEFAULT_DAEMON_URL))
    parser.add_argument("--interval", type=float, default=3.0, help="seconds between polls")
    parser.add_argument(
        "--max",
        type=int,
        default=200,
        help="stop after this many approvals. A ceiling, not a target: a run that asks 200 times "
        "is a result worth reading, not a queue to keep draining.",
    )
    parser.add_argument(
        "--until-idle",
        type=int,
        default=0,
        help="exit after this many consecutive polls with nothing to answer (0 = run until killed)",
    )
    parser.add_argument("--log", default=".ai/local/ledgers/eval-approvals.jsonl")
    parser.add_argument(
        "--dry-run",
        action="store_true",
        help="report what would be approved and approve nothing",
    )
    args = parser.parse_args()

    if not args.run and not args.project:
        sys.exit("one of --run or --project is required; there is deliberately no --all")

    root = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", ".."))
    token = control_token(root)
    wanted = set(args.run or ())
    projects = set(args.project or ())
    log_path = args.log if os.path.isabs(args.log) else os.path.join(root, args.log)
    os.makedirs(os.path.dirname(log_path), exist_ok=True)

    print(f"answering approvals for runs {sorted(wanted)}"
          f"{' (dry run)' if args.dry_run else ''}; ceiling {args.max}", flush=True)

    approved = 0
    idle = 0
    # A proposal is answered once. Without this the loop re-answers anything that stays pending —
    # and something answered with 409 stays pending for ever, because 409 is exactly "this cannot
    # be decided". Found by test rather than by reasoning: the approver hammered one un-answerable
    # proposal every poll and spent the whole ceiling on it, so the ceiling that exists to stop a
    # runaway was consumed by the one row that could never move, and real approvals behind it would
    # then have been refused.
    seen: set = set()
    while approved < args.max:
        try:
            proposals = call(f"{args.daemon_url}/proposals", token) or []
        except urllib.error.URLError as error:
            # Not fatal: the ablation restarts the daemon between layers, and an approver that
            # died on the first gap would have to be babysat by the person it exists to replace.
            print(f"daemon unreachable ({error}); retrying", flush=True)
            time.sleep(args.interval)
            continue

        # Re-read every poll rather than once: the successor run this loop's own approvals create
        # does not exist yet when the loop starts.
        if projects:
            try:
                runs = call(f"{args.daemon_url}/runs", token) or []
            except urllib.error.URLError:
                runs = []
            if not isinstance(runs, list):
                runs = runs.get("runs", [])
            for run in runs:
                if run.get("project_id") in projects and isinstance(run.get("id"), int):
                    wanted.add(run["id"])

        mine = [
            proposal
            for proposal in proposals
            if proposal.get("status") == "pending"
            and proposal.get("kind") == "action-approval"
            and proposal.get("run_id") in wanted
            and proposal.get("id") not in seen
        ]
        if not mine:
            idle += 1
            if args.until_idle and idle >= args.until_idle:
                print(f"nothing pending for {idle} polls; done", flush=True)
                break
            time.sleep(args.interval)
            continue
        idle = 0

        for proposal in mine:
            seen.add(proposal["id"])
            record = {
                "proposal_id": proposal["id"],
                "run_id": proposal["run_id"],
                "tool_name": proposal.get("tool_name"),
                "reasoning": proposal.get("reasoning"),
                "tool_input": proposal.get("tool_input"),
                "created_at": proposal.get("created_at"),
                "dry_run": args.dry_run,
            }
            if not args.dry_run:
                try:
                    call(
                        f"{args.daemon_url}/proposals/{proposal['id']}/approve",
                        token,
                        method="POST",
                    )
                except urllib.error.HTTPError as error:
                    # 409 is the normal race: somebody decided it first, or the run cannot resume.
                    # Recorded rather than swallowed — an ablation reading "approved" for a proposal
                    # that never resumed would be measuring a run that was already over.
                    record["failed"] = f"HTTP {error.code}: {error.read().decode()[:200]}"
            with open(log_path, "a", encoding="utf-8") as handle:
                handle.write(json.dumps(record) + "\n")
            approved += 1
            status = "would approve" if args.dry_run else record.get("failed", "approved")
            print(
                f"{status}: proposal {proposal['id']} run {proposal['run_id']} "
                f"{proposal.get('tool_name') or '?'}",
                flush=True,
            )
            if approved >= args.max:
                print(f"ceiling of {args.max} reached; stopping", flush=True)
                break

    print(f"{approved} answered; recorded in {log_path}", flush=True)
    return 0


if __name__ == "__main__":
    sys.exit(main())
