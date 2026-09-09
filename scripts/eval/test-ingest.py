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

import json
import os
import subprocess
import sys
import tempfile

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

    total = 15
    print(f"\n{total - failures}/{total} as expected")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
