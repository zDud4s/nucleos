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
`ABLATION.md`'s tables. That is the exact shape `.ai/scripts/evidence_gate.py` refuses in a Handoff:
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

import hashlib
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
    """One (task, layer) run of the ladder, as `ladder.py` already has it in hand.

    `base` is the commit `base.sh` laid the task's tree out from. It is `None` on every row
    transcribed before the ledger kept it — which is all nineteen `manual` rows.
    """
    task: str
    layer: str
    verdict: str
    turns: float = None
    cost: float = None
    wall: float = None
    base: str = None


def _value(cell, metric):
    return SCOREABLE[cell.verdict] if metric == "verdict" else getattr(cell, metric)


def _counts(cell, task, layer, metric):
    """Whether this cell is one of the numbers behind `layer`'s side of `task`'s pair."""
    return (cell.task == task and cell.layer == layer and cell.verdict in SCOREABLE
            and _value(cell, metric) is not None)


def _values(cells, task, layer, metric):
    """Every number this layer produced for this task — repeats included, order preserved."""
    if metric not in METRICS:
        raise IngestError(f"unknown metric '{metric}'; known: {', '.join(METRICS)}")
    return [_value(cell, metric) for cell in cells if _counts(cell, task, layer, metric)]


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
        # Two bases are two tasks that happen to share a name. Averaging them would pair a
        # measurement of one against a measurement of the other and call it a repeat.
        bases = {cell.base for cell in cells if cell.base and any(
            _counts(cell, task, layer, metric) for layer in (candidate, baseline))}
        if len(bases) > 1:
            raise IngestError(
                f"{task} was measured against {len(bases)} different base commits "
                f"({', '.join(sorted(base[:12] for base in bases))}); "
                "those are different tasks, not repeats of one"
            )
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


def references(rows, candidate, baseline, metric):
    """What a decision over these ledger rows was made of, as `promote.Candidate` references.

    One trace reference per paired task: a digest over every row behind either side of its pair,
    so changing one number, one run id or adding one repeat moves the candidate's address. One
    evidence reference per base commit those rows were measured against, and `unrecorded` for
    rows that never said — named in the address, so a decision resting on transcribed history
    carries that fact with it instead of passing for one anybody could re-mount.
    """
    cells = [cell_from(row) for row in rows]
    traces, evidence = [], []
    for task in sorted(sample.unit for sample in pair(cells, candidate, baseline, metric)):
        used = sorted(
            (row for row, cell in zip(rows, cells)
             if any(_counts(cell, task, layer, metric) for layer in (candidate, baseline))),
            key=promote.canonical_json,
        )
        digest = hashlib.sha256(promote.canonical_json(used).encode("utf-8")).hexdigest()
        traces.append(f"rows:{task}:{digest}")
        evidence.extend(f"base:{task}:{base}"
                        for base in sorted({row.get("base") or "unrecorded" for row in used}))
    return traces, evidence


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
        "base": cell.base,
        "run_ids": list(run_ids),
        "source": source,
    }
    with open(path, "a", encoding="utf-8") as handle:
        handle.write(json.dumps(row, ensure_ascii=False) + "\n")


def cell_from(row):
    return Cell(row["task"], row["layer"], row["verdict"],
                row.get("turns"), row.get("cost"), row.get("wall"), row.get("base"))


def read_cells(path):
    return [cell_from(row) for row in read_rows(path)]


def read_rows(path):
    """The ledger as written, one dict per row — what `references` digests."""
    if not os.path.exists(path):
        return []
    rows = []
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
        rows.append(row)
    return rows


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
