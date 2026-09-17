#!/usr/bin/env python3
"""Drive `ingest.py` — the cell ledger and the pairing that feeds `promote.py`.

Wired into `scripts/gates.sh` as `eval: ingest`. Also runs standalone:
`python scripts/eval/test-ingest.py`.

The rules under test are the ablation's own, taken from `ABLATION.md` rather than invented:

  "uma célula em falta é diferente de uma célula que falhou"

is why a refused or inconclusive cell is excluded from the sample set instead of scoring zero, and
the within-layer spread is carried on every pair because the second pass found that spread to be
the size of the effect.
"""

import contextlib
import io
import json
import os
import subprocess
import sys
import tempfile
import urllib.error
from dataclasses import asdict

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

import ingest  # noqa: E402

ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", ".."))


def main():
    failures = 0

    def check(label, condition):
        nonlocal failures
        print(("ok   " if condition else "FAIL ") + label)
        failures += 0 if condition else 1

    def raises(fn):
        try:
            fn()
        except ingest.IngestError:
            return True
        except Exception:
            return False
        return False

    # The first pass and second pass of T1, plus the cells that never produced a number.
    cells = [
        ingest.Cell("T1", "H1", "solved", turns=42, cost=1.77, wall=303),
        ingest.Cell("T1", "H1", "solved", turns=65, cost=2.10, wall=340),
        ingest.Cell("T1", "H3", "solved", turns=13, cost=0.97, wall=630),
        ingest.Cell("T1", "H3", "solved", turns=14, cost=1.02, wall=640),
        ingest.Cell("T2", "H3", "solved", turns=33, cost=1.65, wall=1014),
        ingest.Cell("T3", "H1", "refused", turns=None, cost=None, wall=None),
        ingest.Cell("T3", "H3", "solved", turns=16, cost=1.28, wall=722),
    ]

    pairs = ingest.pair(cells, candidate="H3", baseline="H1", metric="turns")

    check("only tasks scored on both sides are paired",
          [p.unit for p in pairs] == ["T1"])

    check("repeated runs of a cell average into one pair",
          pairs[0].candidate == 13.5 and pairs[0].baseline == 53.5)

    spread = ingest.spread(cells, "T1", "H3", "turns")
    check("the within-layer spread is available, not averaged away",
          spread == (13, 14))

    # T2 has a candidate cell and no baseline; T3's baseline was refused. Neither may become a
    # loss for the side that is present — a missing cell is not a failed cell.
    check("a refused cell is excluded rather than counted against the candidate",
          all(p.unit != "T3" for p in pairs))

    check("a cell with no number for the metric does not pair as zero",
          all(p.candidate != 0.0 and p.baseline != 0.0 for p in pairs))

    binary = ingest.pair(cells, candidate="H3", baseline="H1", metric="verdict")
    check("a verdict pairs as 1 and 0, not as a string",
          binary[0].candidate == 1.0 and binary[0].baseline == 1.0)

    unscoreable = [
        ingest.Cell("T1", "H1", "inconclusive", turns=None, cost=None, wall=None),
        ingest.Cell("T1", "H3", "solved", turns=16, cost=1.28, wall=722),
    ]
    check("an inconclusive cell is not a not-solved cell",
          ingest.pair(unscoreable, candidate="H3", baseline="H1", metric="verdict") == [])

    check("an unknown metric is refused rather than silently empty",
          raises(lambda: ingest.pair(cells, candidate="H3", baseline="H1", metric="vibes")))

    # The T3 finding of the second pass: H1 resolved it once and failed it once. Averaging that
    # into 0.5 would hand `promote.BinaryPolicy` a number it must reject anyway, with the reason
    # lost. The variability IS the result, so pairing refuses and names the task.
    varying = [
        ingest.Cell("T3", "H1", "solved", turns=94, cost=5.73, wall=775),
        ingest.Cell("T3", "H1", "not-solved", turns=40, cost=2.10, wall=500),
        ingest.Cell("T3", "H3", "solved", turns=16, cost=1.28, wall=722),
        ingest.Cell("T3", "H3", "solved", turns=63, cost=3.00, wall=900),
    ]
    check("a task whose verdict varies between runs is refused, not averaged into a half",
          raises(lambda: ingest.pair(varying, candidate="H3", baseline="H1", metric="verdict")))

    check("that same task still pairs on a continuous metric",
          ingest.pair(varying, candidate="H3", baseline="H1", metric="turns")[0].candidate == 39.5)

    # --- the ledger ----------------------------------------------------------

    with tempfile.TemporaryDirectory() as work:
        ledger = os.path.join(work, "eval-cells.jsonl")
        ingest.append_cell(ledger, cells[0], run_ids=[900268], source="ladder")
        ingest.append_cell(ledger, cells[2], run_ids=[900276, 900277], source="ladder")
        read_back = ingest.read_cells(ledger)

        check("a cell appended to the ledger reads back as the same cell",
              read_back[0].task == "T1" and read_back[0].turns == 42
              and read_back[1].turns == 13)

        rows = [json.loads(line) for line in open(ledger, encoding="utf-8")]
        check("the ledger records which runs produced the cell",
              rows[1]["run_ids"] == [900276, 900277])
        check("the ledger records whether a row was measured or transcribed",
              rows[0]["source"] == "ladder")

    # --- references ----------------------------------------------------------

    head = subprocess.run(["git", "rev-parse", "HEAD"], cwd=ROOT,
                          capture_output=True, text=True).stdout.strip()
    check("a reference git knows resolves to a full sha",
          ingest.resolve_reference(head[:7], ROOT) == head)

    # The 2026-08-16 blocker, as a test: after the history rewrite every sha cited under `.ai/`
    # stopped resolving, and `base.sh` died with `no such commit` instead of saying so up front.
    check("a reference git does not know is refused rather than left dangling",
          raises(lambda: ingest.resolve_reference("41e9a56", ROOT)))

    # --- the base a cell was measured against --------------------------------
    #
    # `base.sh` prints the commit it laid the tree out from, and until now the ladder dropped it. A
    # ledger that cannot say what was measured cannot tell a repeat from a different task.

    sha_a, sha_b = "a" * 40, "b" * 40
    with tempfile.TemporaryDirectory() as work:
        ledger = os.path.join(work, "eval-cells.jsonl")
        ingest.append_cell(ledger, ingest.Cell("T1", "H3", "solved", turns=13, base=sha_a),
                           run_ids=[900474], source="ladder")
        check("a ledger row records the commit its task's tree was laid out from",
              ingest.read_cells(ledger)[0].base == sha_a)

    split = [ingest.Cell("T1", "H3", "solved", turns=13, base=sha_a),
             ingest.Cell("T1", "H0", "solved", turns=41, base=sha_b)]
    check("a task measured against two different bases is refused, not paired as repeats",
          raises(lambda: ingest.pair(split, "H3", "H0", "turns")))

    legacy = [ingest.Cell("T1", "H3", "solved", turns=13),
              ingest.Cell("T1", "H0", "solved", turns=41, base=sha_a)]
    check("a transcribed row with no recorded base still pairs",
          len(ingest.pair(legacy, "H3", "H0", "turns")) == 1)
    _, evidence = ingest.references([asdict(cell) for cell in legacy], "H3", "H0", "turns")
    check("and the references name it unrecorded instead of passing it off as measured",
          evidence == [f"base:T1:{sha_a}", "base:T1:unrecorded"])

    # --- the ladder's write path ---------------------------------------------
    #
    # Stubbed rather than run: a real cell needs a daemon on 8791, builds a worktree, and cost
    # between $0.97 and $7.14 the twelve times it was done for real. What is under test is the
    # wiring — that the numbers `watch()` returns reach the ledger in the right fields — and that
    # survives stubbing. `test-auto-approve.py` stubs its daemon for the same reason.

    import ladder  # noqa: E402  -- safe to import only since the loop moved into main()

    check("importing the ladder contacts no daemon and runs no cell",
          ladder._TOKEN is None)

    def drive(work, call_impl, cell=("T1", "H0"), base=None, status="completed", scored=None):
        ledger = os.path.join(work, "eval-cells.jsonl")
        # `getattr` with a default so a name the ladder does not have yet reads as a FAIL line
        # below rather than as a traceback that takes every other check down with it.
        saved = {name: getattr(ladder, name, None) for name in
                 ("requested", "prompt_for", "prepare", "call", "watch", "score", "turns_of",
                  "start_approver")}
        ingest.LEDGER, saved_ledger = ledger, ingest.LEDGER
        try:
            ladder.requested = lambda argv: [cell]
            # Stubbed like the daemon: the real one is `auto-approve.py` against port 8791, and a
            # test must not answer anybody's approvals.
            ladder.start_approver = lambda project: None
            ladder.prompt_for = lambda task: "fix the thing"
            ladder.prepare = lambda task, layer, tree: base
            ladder.call = call_impl
            ladder.watch = lambda project, first: {"wall": 265, "cost": 1.89, "runs": [900267],
                                                   "status": status}
            ladder.score = lambda task, layer, tree, first: (scored.append(tree) if scored
                                                             is not None else None) or "solved"
            ladder.turns_of = lambda runs: 34
            ladder.main([])
        finally:
            for name, value in saved.items():
                setattr(ladder, name, value)
            ingest.LEDGER = saved_ledger
        return [json.loads(line) for line in open(ledger, encoding="utf-8")]

    with tempfile.TemporaryDirectory() as work:
        rows = drive(work, lambda path, method="GET", body=None: {"id": 900267})
        # The real T1 x H0 cell of the first pass, as `ABLATION.md` records it.
        check("a finished cell reaches the ledger with every number in its own field",
              len(rows) == 1 and rows[0]["task"] == "T1" and rows[0]["layer"] == "H0"
              and rows[0]["verdict"] == "solved" and rows[0]["turns"] == 34
              and rows[0]["cost"] == 1.89 and rows[0]["wall"] == 265)
        check("the ledger row names the runs the cell was made of, and calls itself measured",
              rows[0]["run_ids"] == [900267] and rows[0]["source"] == "ladder")

    with tempfile.TemporaryDirectory() as work:
        rows = drive(work, lambda path, method="GET", body=None: {"id": 900267}, base=sha_a)
        check("the row carries the base commit the cell's tree was laid out from",
              rows[0]["base"] == sha_a)
    check("which the ladder reads off base.sh's own report line",
          ladder.base_of(f"T1\tsynthetic\t{sha_a}\tC:/Projects/nucleos-eval/T1-H3\n") == sha_a
          and ladder.base_of("") is None)

    # T4 x H3 on 2026-09-13: the watcher gave up at its ceiling while the daemon was still gating
    # the chain, and the ladder scored the worktree anyway, mid-edit. A chain that is not over
    # has no verdict yet, and the tree it is still writing to must not be read as one.
    with tempfile.TemporaryDirectory() as work:
        scored = []
        rows = drive(work, lambda path, method="GET", body=None: {"id": 900267},
                     status="running", scored=scored)
        check("a chain the watcher gave up on is recorded as timed out, not scored mid-flight",
              rows[0]["verdict"] == "timed_out" and scored == [])

    def refuse(path, method="GET", body=None):
        raise urllib.error.HTTPError(
            "http://x/runs", 409, "Conflict", {}, io.BytesIO(b"busy"))

    with tempfile.TemporaryDirectory() as work:
        rows = drive(work, refuse)
        check("a refused cell is recorded as refused, not left out of the ledger",
              len(rows) == 1 and rows[0]["verdict"] == "refused"
              and rows[0]["turns"] is None and rows[0]["run_ids"] == [])

    # A cell that already cost money must not be lost to a ledger that cannot be written.
    with tempfile.TemporaryDirectory() as work:
        blocked = os.path.join(work, "missing-dir", "\0", "cells.jsonl")
        saved_ledger, ingest.LEDGER = ingest.LEDGER, blocked
        try:
            ladder.record(ingest.Cell("T1", "H0", "solved", turns=34), [900267])
            survived = True
        except Exception:
            survived = False
        finally:
            ingest.LEDGER = saved_ledger
        check("a ledger write that fails shouts instead of killing the cell", survived)

    # --- the ladder, under the daemon's project gate -------------------------
    #
    # Since a840181 (2026-08-26) `create_run` refuses an unattended run for a project in `off`, and a
    # project the daemon has never heard of reads as off. The ladder ran on 2026-08-17/19, before
    # that door asked anything — which is how all four layers ran then and three of them 422 now.

    class Daemon:
        """Records every call and answers the way the real routes do."""

        def __init__(self, refuse=lambda method, path, body: False):
            self.calls = []
            self.refuse = refuse

        def __call__(self, path, method="GET", body=None):
            self.calls.append((method, path, body))
            code = self.refuse(method, path, body)
            if code:
                # True is the 422 every refusal here used to be; a number is that status instead.
                raise urllib.error.HTTPError(
                    "http://x" + path, 422 if code is True else code, "Refused", {},
                    io.BytesIO(b"project is not onboarded to .ai/workflow"))
            if (method, path) == ("POST", "/runs"):
                return {"id": 900500}
            return {}

        def modes(self):
            return [(body["project_id"], body["mode"], body.get("project_root"))
                    for method, path, body in self.calls
                    if (method, path) == ("POST", "/autopilot/state")]

        def first(self, method, path):
            return next((i for i, (m, p, _) in enumerate(self.calls)
                         if (m, p) == (method, path)), None)

    daemon = Daemon()
    with tempfile.TemporaryDirectory() as work:
        drive(work, daemon, cell=("T1", "H2"))
    modes = daemon.modes()
    shadow_at = daemon.first("POST", "/autopilot/state")
    launch_at = daemon.first("POST", "/runs")
    check("a worktree cell puts its own project in shadow, rooted at its own tree, before launching",
          bool(modes) and modes[0] == ("eval-T1-H2", "shadow", f"{ladder.TREES}/T1-H2")
          and shadow_at is not None and launch_at is not None and shadow_at < launch_at)
    check("and switches that project back off once the cell is over",
          len(modes) == 2 and modes[1] == ("eval-T1-H2", "off", None))

    # `off` alone leaves a row in the owner's roster, which lists every project in any mode — the
    # probe of 2026-09-11 found `eval-T1-H2` there, switched off, where before it there was nothing.
    # `DELETE /projects/{id}` takes it off the roster, touches nothing on disk, and keeps the history
    # the ledger's run ids point into unless `forget_history` is asked for — which the exact-path
    # match below also rules out.
    off_at = next((i for i in reversed(range(len(daemon.calls)))
                   if daemon.calls[i][:2] == ("POST", "/autopilot/state")
                   and daemon.calls[i][2]["mode"] == "off"), None)
    removal_at = daemon.first("DELETE", "/projects/eval-T1-H2")
    check("and then takes it off the roster, keeping its history",
          off_at is not None and removal_at is not None and removal_at > off_at)

    daemon = Daemon(refuse=lambda method, path, body: (method, path) == ("POST", "/runs"))
    with tempfile.TemporaryDirectory() as work:
        rows = drive(work, daemon, cell=("T1", "H2"))
    check("a run the daemon refuses still leaves its project off",
          [mode for _, mode, _ in daemon.modes()] == ["shadow", "off"]
          and rows[0]["verdict"] == "refused")

    daemon = Daemon(refuse=lambda method, path, body: (body or {}).get("mode") == "shadow")
    with tempfile.TemporaryDirectory() as work:
        rows = drive(work, daemon, cell=("T1", "H3"))
    check("a refused activation is recorded as refused and launches nothing",
          rows[0]["verdict"] == "refused" and daemon.first("POST", "/runs") is None)

    daemon = Daemon()
    with tempfile.TemporaryDirectory() as work:
        drive(work, daemon, cell=("T1", "H0"))
    check("a real-mode cell touches no project's mode", daemon.modes() == [])

    # Switching back off can fail too, and a project left in shadow is inert but visible in the
    # roster — so it must be said, with the command that fixes it, and never kill the ladder.
    daemon = Daemon(refuse=lambda method, path, body: (body or {}).get("mode") == "off")
    heard = io.StringIO()
    with tempfile.TemporaryDirectory() as work, contextlib.redirect_stdout(heard):
        try:
            drive(work, daemon, cell=("T1", "H2"))
            survived = True
        except Exception:
            survived = False
    check("a project that cannot be switched off is shouted about, not left silently in shadow",
          survived and "eval-T1-H2" in heard.getvalue() and '"mode": "off"' in heard.getvalue())

    # After a real cell the daemon may still hold the run's worktree — released later, or swept
    # within half an hour — and `DELETE /projects/{id}` answers 409 until then. The door has to be
    # shut already by that point, which is why `off` comes first and the held removal is only told.
    daemon = Daemon(refuse=lambda method, path, body: 409 if method == "DELETE" else False)
    heard = io.StringIO()
    with tempfile.TemporaryDirectory() as work, contextlib.redirect_stdout(heard):
        try:
            drive(work, daemon, cell=("T1", "H2"))
            survived = True
        except Exception:
            survived = False
    check("a removal the daemon holds leaves the project off, and says so",
          survived and bool(daemon.modes()) and daemon.modes()[-1][1] == "off"
          and "eval-T1-H2" in heard.getvalue() and "409" in heard.getvalue())

    # H1 is the worktree layer WITHOUT the classifier hook, and activating a project requires that
    # very hook (`activation_prerequisites`, `autopilot.rs`). The daemon no longer runs the
    # configuration H1 describes, so the ladder stops offering it rather than paying to hear 422.
    try:
        list(ladder.requested(["T1:H1"]))
        why = ""
    except SystemExit as refusal:
        why = str(refusal)
    check("H1 is no longer a layer the ladder runs, and the refusal says why",
          "a840181" in why and "hook" in why)
    check("a whole ladder is the three layers the daemon can still run",
          [layer for _, layer in ladder.requested(["T1"])] == ["H0", "H2", "H3"])

    total = 36
    print(f"\n{total - failures}/{total} as expected")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
