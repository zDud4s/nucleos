#!/usr/bin/env python3
"""Put the nineteen already-measured ablation cells into the cell ledger, as transcription.

    python scripts/eval/backfill-ablation.py            # into .ai/local/ledgers/eval-cells.jsonl
    python scripts/eval/backfill-ablation.py --ledger /tmp/cells.jsonl

Every row here is `source: "manual"`, and that is the whole point of the field. These cells were
measured on 2026-08-17/19 by a `ladder.py` that printed its results and kept none of them; what
survives is `ABLATION.md`'s tables, typed by hand from a terminal that has since closed. A row
transcribed from prose is not a row a gate watched happen, and `promote.py` should be able to tell
the difference when it is asked why it promoted something.

## What is here and what is not

Only what the tables state outright. Where `ABLATION.md` marks a number approximate, lost, or
unrecorded, the field is left empty rather than guessed:

- **T2 x H0 cost** — "~$0.50 aprox.", explicitly "não medido" because the run was killed by the
  600s ceiling and the figure is `runs.rs`'s approximation. Recorded as an unknown cost, not $0.50.
- **T2 x H0 turns** — the summary table gives "—t".
- **second-pass cost and wall clock** — the second-pass table reports turns and verdicts only.
- **T2 x H1, second pass** — `interrupted`. The daemon went down mid-run; `score.sh` says `solved`
  over the tree it left, and `ABLATION.md` refuses that verdict because a run stopped halfway does
  not measure a layer. The verdict is recorded verbatim, and `interrupted` is not in
  `ingest.SCOREABLE`, so pairing drops it exactly as the ablation's own accounting does.

Turns for the first pass come from the twelve-cell summary table; several per-run rows in the
detail table below it record turns as "—", and the summary is the reconciled figure.
"""

import argparse
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

import ingest  # noqa: E402

# task, layer, verdict, turns, cost, wall
FIRST_PASS = [
    ("T1", "H0", "solved", 34, 1.89, 265),
    ("T1", "H1", "solved", 42, 1.77, 303),
    ("T1", "H2", "solved", 32, 1.92, 678),
    ("T1", "H3", "solved", 13, 0.97, 630),
    ("T2", "H0", "solved", None, None, 600),
    ("T2", "H1", "solved", 44, 3.67, 577),
    ("T2", "H2", "solved", 41, 2.70, 1066),
    ("T2", "H3", "solved", 33, 1.65, 1014),
    ("T3", "H0", "not-solved", 39, 1.88, 321),
    ("T3", "H1", "solved", 94, 5.73, 775),
    ("T3", "H2", "solved", 84, 7.14, 1379),
    ("T3", "H3", "solved", 16, 1.28, 722),
]

# Seven repeated cells. H2 was left out of the second pass on purpose — the most expensive cell
# measured, with a mechanical explanation a repeat would confirm without changing.
SECOND_PASS = [
    ("T1", "H1", "solved", 65, None, None),
    ("T1", "H3", "solved", 14, None, None),
    ("T2", "H1", "interrupted", None, None, None),
    ("T2", "H3", "solved", 36, None, None),
    ("T3", "H0", "not-solved", 40, None, None),
    ("T3", "H1", "not-solved", 40, None, None),
    ("T3", "H3", "solved", 63, None, None),
]


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--ledger", default=ingest.LEDGER)
    args = parser.parse_args(argv)

    # Appending twice would double every pair and quietly halve the apparent variance, which is
    # the one quantity this whole gate turns on.
    existing = ingest.read_cells(args.ledger)
    if any(cell.task in ("T1", "T2", "T3") for cell in existing):
        print(f"{args.ledger} already holds T1/T2/T3 cells; refusing to append them twice")
        return 1

    for task, layer, verdict, turns, cost, wall in FIRST_PASS + SECOND_PASS:
        ingest.append_cell(
            args.ledger,
            ingest.Cell(task, layer, verdict, turns=turns, cost=cost, wall=wall),
            run_ids=[],
            source="manual",
        )

    print(f"wrote {len(FIRST_PASS) + len(SECOND_PASS)} transcribed cells to {args.ledger}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
