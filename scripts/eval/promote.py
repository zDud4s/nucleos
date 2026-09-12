#!/usr/bin/env python3
"""Address an ablation candidate by its content, and decide whether it may replace the baseline.

    import promote
    candidate = promote.Candidate.new(config, trace_refs, evidence_refs)
    decision = promote.BinaryPolicy(min_pairs=8, max_p_value=0.05).decide(samples)

This module does not measure anything. `verify-task.sh` and `score.sh` produce the verdicts; this
decides what a set of verdicts is allowed to conclude. `ingest.py` turns the ladder's ledger into
`PairedSample` rows, and into the references the command line addresses each decision by.

## Why the address covers the references

The candidate id is a SHA-256 over the canonical config AND its trace and evidence references,
not over the config alone. On 2026-08-16 the history was rewritten and every sha cited under
`.ai/` stopped resolving; `base.sh` died with `no such commit` and three tasks silently stopped
being mountable. When the sha is part of the address, a dead reference invalidates the candidate
instead of leaving it pointing at nothing.

The design is borrowed from `ecc2/src/harness_eval.rs` in affaan-m/ECC, with one thing deliberately
left behind: that module's `verify_persisted_id` also accepts a legacy id computed over the config
alone, and applies the reference check only when the id is the newer form. A record stored under
the legacy address therefore has its references outside the address entirely. There is one address
here, and it always covers the references.

What the command line addresses is the decision. Its config is the two layers, the metric and the
policy; its trace references digest the ledger rows behind each pair; its evidence references are
the base commits those rows were measured against — `unrecorded` for the transcribed history,
which the address names rather than excuses.

## Why the gate is not a mean and a win rate

The borrowed policy compares arithmetic means with a minimum sample count that may be as low as
two. `ABLATION.md` measured why that is not enough here: in the second pass (2026-08-18/19) the
variance WITHIN a layer was as large as the difference BETWEEN layers — H3 on T3 spent 16 turns in
one run and 63 in the other — and two of the three headline claims from the first pass did not
survive. Three tasks all pointing the same way is p = 0.125 under an exact sign test, which is the
first pass's headline and is not evidence.

So: binary outcomes go through an exact one-sided sign test over discordant pairs, and continuous
metrics through a percentile bootstrap that must both exclude zero and be narrower than the effect
it claims. The second requirement is the one that matters — on the measured H1-vs-H3 turn counts
the interval excludes zero and is still wider than the effect, so "excludes zero" alone would have
promoted a reading the second pass falsified.

Both decisions are pure functions of their samples, and both accumulate every failed condition
rather than returning at the first, so one run of the gate names everything that is wrong.
"""

import argparse
import hashlib
import json
import math
import random
import sys
from dataclasses import asdict, dataclass, field


class PromoteError(Exception):
    """A candidate or a sample set that the gate refuses to reason about."""


MAX_REFS = 100
MAX_REF_LEN = 4096
MAX_CONFIG_BYTES = 1024 * 1024
MIN_RESAMPLES = 1000


def canonical_json(value):
    """Serialize with keys sorted at every depth, so the address ignores authoring order."""
    return json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=False)


def _normalize_refs(kind, refs):
    refs = list(refs)
    if not refs or any(not str(ref).strip() for ref in refs):
        raise PromoteError(f"at least one non-empty {kind} reference is required")
    if len(refs) > MAX_REFS or any(len(str(ref)) > MAX_REF_LEN for ref in refs):
        raise PromoteError(f"{kind} references exceed bounded limits")
    return sorted({str(ref).strip() for ref in refs})


@dataclass
class Candidate:
    id: str
    canonical_config: str
    trace_refs: list
    evidence_refs: list

    @classmethod
    def new(cls, config, trace_refs, evidence_refs):
        traces = _normalize_refs("trace", trace_refs)
        evidence = _normalize_refs("evidence", evidence_refs)
        canonical_config = canonical_json(config)
        if len(canonical_config.encode("utf-8")) > MAX_CONFIG_BYTES:
            raise PromoteError("candidate configuration exceeds 1 MiB")
        artifact = canonical_json({
            "config": json.loads(canonical_config),
            "trace_refs": traces,
            "evidence_refs": evidence,
        })
        digest = hashlib.sha256(artifact.encode("utf-8")).hexdigest()
        return cls(digest, canonical_config, traces, evidence)

    def verify_integrity(self):
        """Rebuild the address from the parts and refuse anything that moved since construction."""
        rebuilt = Candidate.new(
            json.loads(self.canonical_config), self.trace_refs, self.evidence_refs
        )
        if (rebuilt.id != self.id
                or rebuilt.canonical_config != self.canonical_config
                or rebuilt.trace_refs != self.trace_refs
                or rebuilt.evidence_refs != self.evidence_refs):
            raise PromoteError("candidate content address is invalid")


@dataclass(frozen=True)
class PairedSample:
    """One unit of comparison — a task — scored under the candidate and under the baseline."""
    unit: str
    candidate: float
    baseline: float


@dataclass
class Decision:
    passed: bool
    sample_count: int
    failures: list = field(default_factory=list)
    p_value: float = None
    wins: int = None
    losses: int = None
    point_estimate: float = None
    ci_low: float = None
    ci_high: float = None


def _validate(samples):
    if not samples:
        raise PromoteError("samples cannot be empty")
    units = [sample.unit for sample in samples]
    if len(set(units)) != len(units):
        raise PromoteError("sample units must be unique")
    for sample in samples:
        for value in (sample.candidate, sample.baseline):
            if not math.isfinite(value):
                raise PromoteError("scores must be finite")


def sign_test_p_value(wins, discordant):
    """Exact one-sided binomial probability of at least `wins` successes in `discordant` fair trials."""
    if discordant == 0:
        return 1.0
    tail = sum(math.comb(discordant, k) for k in range(wins, discordant + 1))
    return tail / (2 ** discordant)


@dataclass(frozen=True)
class BinaryPolicy:
    """Gate a yes/no outcome — did the cell resolve — with an exact sign test."""
    min_pairs: int
    max_p_value: float

    def decide(self, samples):
        _validate(samples)
        for sample in samples:
            if sample.candidate not in (0.0, 1.0) or sample.baseline not in (0.0, 1.0):
                raise PromoteError("a binary outcome must be 0 or 1")

        wins = sum(1 for s in samples if s.candidate > s.baseline)
        losses = sum(1 for s in samples if s.candidate < s.baseline)
        discordant = wins + losses
        p_value = sign_test_p_value(wins, discordant)

        failures = []
        if len(samples) < self.min_pairs:
            failures.append(f"minimum pair count is {self.min_pairs}, got {len(samples)}")
        if discordant == 0:
            # Concordant pairs are not agreement that the candidate is better; they are the
            # absence of any evidence either way, and the gate must not read them as support.
            failures.append("no discordant pairs: the two layers were never told apart")
        if p_value > self.max_p_value:
            failures.append(f"sign test p {p_value:.6f} is above {self.max_p_value:.6f}")

        return Decision(
            passed=not failures,
            sample_count=len(samples),
            failures=failures,
            p_value=p_value,
            wins=wins,
            losses=losses,
        )


@dataclass(frozen=True)
class ContinuousPolicy:
    """Gate a continuous metric — turns, cost, wall clock — with a percentile bootstrap.

    `higher_is_better` is False for turns and cost, where the candidate should be the smaller
    number. `precision_ratio` is the requirement the borrowed design lacks: an interval wider than
    the effect it reports does not support the effect, however far it sits from zero.
    """
    min_pairs: int
    confidence: float
    resamples: int
    seed: int
    higher_is_better: bool = True
    precision_ratio: float = 1.0

    def __post_init__(self):
        # Measured on the H1-vs-H3 turn counts: 200 resamples put the interval at [15.5, 40.0]
        # where 2000 put it at [9.5, 40.0]. Too few draws never reach the tails of the resampling
        # distribution, so the interval comes back narrower than the data supports — which is the
        # failure this gate exists to refuse, arriving through the gate's own knob.
        if self.resamples < MIN_RESAMPLES:
            raise PromoteError(f"a percentile interval needs at least {MIN_RESAMPLES} resamples")
        if not 0.0 < self.confidence < 1.0:
            raise PromoteError("confidence must be between 0 and 1")
        if self.min_pairs < 1:
            raise PromoteError("minimum pair count must be positive")

    def decide(self, samples):
        _validate(samples)
        deltas = [
            (s.candidate - s.baseline) if self.higher_is_better else (s.baseline - s.candidate)
            for s in samples
        ]
        point = sum(deltas) / len(deltas)

        rng = random.Random(self.seed)
        means = sorted(
            sum(draw) / len(draw)
            for draw in (rng.choices(deltas, k=len(deltas)) for _ in range(self.resamples))
        )
        tail = (1.0 - self.confidence) / 2.0
        ci_low = means[int(tail * self.resamples)]
        ci_high = means[int((1.0 - tail) * self.resamples) - 1]
        width = ci_high - ci_low

        failures = []
        if len(samples) < self.min_pairs:
            failures.append(f"minimum pair count is {self.min_pairs}, got {len(samples)}")
        if ci_low <= 0.0:
            failures.append(f"the {self.confidence:.0%} interval includes no effect")
        if width > self.precision_ratio * abs(point):
            failures.append(
                f"interval width {width:.6f} exceeds {self.precision_ratio:g}x the effect {point:.6f}"
            )

        return Decision(
            passed=not failures,
            sample_count=len(samples),
            failures=failures,
            point_estimate=point,
            ci_low=ci_low,
            ci_high=ci_high,
        )


# --- command line ------------------------------------------------------------
#
# Exit codes follow `score.sh`'s, and for the same reason it gives: a run that could not be scored
# is not a run that scored badly. Here, a gate with nothing to compare must never read as a gate
# that said no.
#
#   0  promote      -- every condition passed
#   1  refused      -- the gate ran and at least one condition failed
#   2  undecidable  -- no comparable pairs, or samples the gate refuses to reason about
#   3  usage        -- bad arguments

EXIT_PROMOTE, EXIT_REFUSED, EXIT_UNDECIDABLE, EXIT_USAGE = 0, 1, 2, 3


def main(argv=None):
    # Imported here rather than at module scope: `ingest` imports this module, and the two would
    # deadlock on each other at import time. By the time main runs, this module is fully loaded.
    import ingest

    parser = argparse.ArgumentParser(
        description="Decide whether one ablation layer may replace another.",
        epilog="exit: 0 promote, 1 refused, 2 undecidable, 3 usage",
    )
    parser.add_argument("--candidate", required=True, help="layer under test, e.g. H3")
    parser.add_argument("--baseline", required=True, help="layer it would replace, e.g. H1")
    parser.add_argument("--metric", default="turns", choices=list(ingest.METRICS))
    parser.add_argument("--ledger", default=ingest.LEDGER)
    parser.add_argument("--min-pairs", type=int, default=8)
    parser.add_argument("--max-p", type=float, default=0.05, help="verdict metric only")
    parser.add_argument("--confidence", type=float, default=0.95)
    parser.add_argument("--resamples", type=int, default=10_000)
    parser.add_argument("--seed", type=int, default=7)
    parser.add_argument("--precision-ratio", type=float, default=1.0)
    args = parser.parse_args(argv)

    try:
        rows = ingest.read_rows(args.ledger)
        cells = [ingest.cell_from(row) for row in rows]
        pairs = ingest.pair(cells, args.candidate, args.baseline, args.metric)
    except ingest.IngestError as error:
        print(f"undecidable: {error}")
        return EXIT_UNDECIDABLE

    if not pairs:
        print(f"undecidable: no task was scored under both {args.candidate} and {args.baseline}")
        return EXIT_UNDECIDABLE

    try:
        if args.metric == "verdict":
            policy = BinaryPolicy(min_pairs=args.min_pairs, max_p_value=args.max_p)
        else:
            # Turns, cost and wall clock are all better when smaller.
            policy = ContinuousPolicy(
                min_pairs=args.min_pairs, confidence=args.confidence,
                resamples=args.resamples, seed=args.seed,
                higher_is_better=False, precision_ratio=args.precision_ratio,
            )
        # A verdict quoted without its address cannot be told apart from the same verdict over a
        # ledger that has since grown a row, or under a laxer policy.
        traces, evidence = ingest.references(rows, args.candidate, args.baseline, args.metric)
        candidate = Candidate.new(
            {"candidate": args.candidate, "baseline": args.baseline, "metric": args.metric,
             "policy": {"kind": type(policy).__name__, **asdict(policy)}},
            traces, evidence,
        )
        candidate.verify_integrity()
        decision = policy.decide(pairs)
    except PromoteError as error:
        print(f"undecidable: {error}")
        return EXIT_UNDECIDABLE

    print(f"{args.metric}: {args.candidate} (candidate) vs {args.baseline} (baseline)")
    print(f"candidate {candidate.id}")
    for sample in pairs:
        left = ingest.spread(cells, sample.unit, args.candidate, args.metric)
        right = ingest.spread(cells, sample.unit, args.baseline, args.metric)
        print(f"  {sample.unit:<6} {sample.candidate:>8.2f} vs {sample.baseline:>8.2f}"
              f"   runs: [{', '.join(str(v) for v in left)}]"
              f" vs [{', '.join(str(v) for v in right)}]")

    unrecorded = sorted(ref.split(":")[1] for ref in candidate.evidence_refs
                        if ref.endswith(":unrecorded"))
    if unrecorded:
        print(f"  no recorded base commit for {', '.join(unrecorded)}: transcribed rows,"
              " and the address says so")

    print()
    if decision.p_value is not None:
        print(f"  {decision.wins} wins, {decision.losses} losses, sign test p = {decision.p_value:.6f}")
    else:
        print(f"  effect {decision.point_estimate:.2f}"
              f"   {args.confidence:.0%} interval [{decision.ci_low:.2f}, {decision.ci_high:.2f}]")

    if decision.passed:
        print(f"\nPROMOTE: {args.candidate} over {args.baseline} on {args.metric}"
              f" ({decision.sample_count} pairs) as candidate {candidate.id[:12]}")
        return EXIT_PROMOTE
    print(f"\nREFUSED: {args.candidate} over {args.baseline} on {args.metric}"
          f" as candidate {candidate.id[:12]}")
    for failure in decision.failures:
        print(f"  - {failure}")
    return EXIT_REFUSED


if __name__ == "__main__":
    sys.exit(main())
