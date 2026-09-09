#!/usr/bin/env python3
"""Record ablation cells as they are measured, and pair them into `promote.PairedSample` rows.

    import ingest
    ingest.append_cell(ingest.LEDGER, cell, run_ids=[900276, 900277], source="ladder")
    pairs = ingest.pair(ingest.read_cells(ingest.LEDGER), candidate="H3", baseline="H1",
                        metric="turns")

`promote.py` decides; this gathers. The split is deliberate — `promote` is pure functions over
samples and its tests depend on that, while everything here touches the filesystem and git.

## Why the ledger exists at all

`ladder.py` accumulates its results in a list and prints a tab-separated summary at the end. It
never writes them anywhere. The nineteen cells of 2026-08-17/19 — about $45 of measurement — went
to a terminal that has since closed, and survive only as prose transcribed by hand into
`ABLATION.md`'s tables. That is the exact shape `scripts/evidence_gate.py` refuses in a Handoff:
a number reported rather than recorded. A gate that decides promotions from hand-transcribed rows
inherits the problem, so rows carry `source` — `ladder` for a cell this module wrote as it
happened, `manual` for one typed in afterwards. The distinction is the one `workflow.md` Rule 5
already draws for `metrics.jsonl`.

## The two pairing rules, both taken from the ablation rather than invented

**A missing cell is not a failed cell.** `ABLATION.md`: "uma célula em falta é diferente de uma
célula que falhou". A refused, inconclusive or timed-out cell produced no verdict, so it leaves the
sample set entirely. Scoring it zero would hand the other layer a win it never earned.

**A task whose verdict varies between runs is not half a success.** The second pass found T3
resolving under H1 once and failing once. Averaging that to 0.5 buries the finding in a number
`promote.BinaryPolicy` has to reject anyway, with the reason gone. Pairing refuses and names the
task; the same task still pairs on turns or cost, where the spread is the thing worth seeing.
"""

import json
import os
import subprocess
from dataclasses import dataclass

import promote


class IngestError(Exception):
    """A ledger row, a metric, or a reference the module refuses to guess at."""


ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", ".."))
LEDGER = os.path.join(ROOT, ".ai", "local", "ledgers", "eval-cells.jsonl")

# A cell only carries a number when it reached a verdict. Everything else — `refused`,
# `inconclusive`, `timed_out` — is an absent measurement, not a bad one.
SCOREABLE = {"solved": 1.0, "not-solved": 0.0}

NUMERIC_METRICS = ("turns", "cost", "wall")
METRICS = NUMERIC_METRICS + ("verdict",)


@dataclass
class Cell:
    """One (task, layer) run of the ladder, as `ladder.py` already has it in hand."""
    task: str
    layer: str
    verdict: str
    turns: float = None
    cost: float = None
    wall: float = None


def _values(cells, task, layer, metric):
    """Every number this layer produced for this task — repeats included, order preserved."""
    if metric not in METRICS:
        raise IngestError(f"unknown metric '{metric}'; known: {', '.join(METRICS)}")
    out = []
    for cell in cells:
        if cell.task != task or cell.layer != layer or cell.verdict not in SCOREABLE:
            continue
        value = SCOREABLE[cell.verdict] if metric == "verdict" else getattr(cell, metric)
        if value is not None:
            out.append(value)
    return out


def spread(cells, task, layer, metric):
    """The individual runs behind a paired value, so the within-layer variance stays visible."""
    return tuple(sorted(_values(cells, task, layer, metric)))


def pair(cells, candidate, baseline, metric):
    """Build one `promote.PairedSample` per task scored under both layers."""
    if metric not in METRICS:
        raise IngestError(f"unknown metric '{metric}'; known: {', '.join(METRICS)}")

    samples = []
    for task in sorted({cell.task for cell in cells}):
        left = _values(cells, task, candidate, metric)
        right = _values(cells, task, baseline, metric)
        if not left or not right:
            continue
        if metric == "verdict":
            for layer, values in ((candidate, left), (baseline, right)):
                if len(set(values)) > 1:
                    raise IngestError(
                        f"{task} under {layer} resolved in some runs and not in others; "
                        "that variability is the result, not a number to average"
                    )
        samples.append(promote.PairedSample(
            unit=task,
            candidate=sum(left) / len(left),
            baseline=sum(right) / len(right),
        ))
    return samples


def append_cell(path, cell, run_ids, source):
    """Append one measured cell. Called by `ladder.py` as each cell finishes, never in bulk."""
    if source not in ("ladder", "manual"):
        raise IngestError("source must be 'ladder' (measured here) or 'manual' (transcribed)")
    os.makedirs(os.path.dirname(os.path.abspath(path)), exist_ok=True)
    row = {
        "task": cell.task,
        "layer": cell.layer,
        "verdict": cell.verdict,
        "turns": cell.turns,
        "cost": cell.cost,
        "wall": cell.wall,
        "run_ids": list(run_ids),
        "source": source,
    }
    with open(path, "a", encoding="utf-8") as handle:
        handle.write(json.dumps(row, ensure_ascii=False) + "\n")


def read_cells(path):
    if not os.path.exists(path):
        return []
    cells = []
    for number, line in enumerate(open(path, encoding="utf-8"), start=1):
        line = line.strip()
        if not line:
            continue
        try:
            row = json.loads(line)
        except json.JSONDecodeError as error:
            raise IngestError(f"{path}:{number} is not valid JSON: {error}") from error
        for required in ("task", "layer", "verdict"):
            if required not in row:
                raise IngestError(f"{path}:{number} has no '{required}'")
        cells.append(Cell(row["task"], row["layer"], row["verdict"],
                          row.get("turns"), row.get("cost"), row.get("wall")))
    return cells


def resolve_reference(reference, root=ROOT):
    """Expand a task's reference to a full sha, refusing one this repository does not have.

    On 2026-08-16 the history was rewritten and every sha cited under `.ai/` stopped resolving.
    `base.sh` found out at `git apply` time and died with `no such commit`, which reads like a
    broken script. Resolving up front turns that into a named, early refusal — and, because
    `promote.Candidate` hashes the references into the candidate's address, a reference that
    cannot resolve invalidates the candidate instead of leaving it pointing at nothing.
    """
    result = subprocess.run(
        ["git", "rev-parse", "--verify", "--quiet", f"{reference}^{{commit}}"],
        cwd=root, capture_output=True, text=True,
    )
    sha = result.stdout.strip()
    if result.returncode != 0 or not sha:
        raise IngestError(f"this repository has no commit '{reference}'")
    return sha
