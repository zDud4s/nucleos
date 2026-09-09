#!/usr/bin/env python3
"""Drive `promote.py` — the content address and the promotion policy.

Wired into `scripts/gates.sh` as `eval: promote`, alongside its neighbours in the hooks leg.
Also runs standalone: `python scripts/eval/test-promote.py`.

Two of these tests are the ablation's own findings written as assertions, and they are the reason
this module diverges from the design it was copied from. `ABLATION.md` measured, on 2026-08-18/19,
that the variance WITHIN a layer is as large as the difference BETWEEN layers — H3 on T3 spent 16
turns in one run and 63 in the other. A policy that compares arithmetic means with `min_samples: 2`
would have promoted the first pass's headline, which the second pass falsified. So the gate here is
an exact sign test and a bootstrap interval, not a mean and a win rate.
"""

import os
import subprocess
import sys
import tempfile

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)

import ingest  # noqa: E402
import promote  # noqa: E402


def main():
    failures = 0

    def check(label, condition):
        nonlocal failures
        print(("ok   " if condition else "FAIL ") + label)
        failures += 0 if condition else 1

    def raises(fn):
        try:
            fn()
        except promote.PromoteError:
            return True
        except Exception:
            return False
        return False

    # --- the content address -------------------------------------------------

    first = promote.Candidate.new(
        {"model": "sonnet", "limits": {"turns": 3, "tools": ["read"]}},
        [" abc1234 ", "def5678"],
        ["evidence://review-1"],
    )
    second = promote.Candidate.new(
        {"limits": {"tools": ["read"], "turns": 3}, "model": "sonnet"},
        ["def5678", "abc1234"],
        ["evidence://review-1"],
    )
    check("the address is stable under key order and reference order",
          first.id == second.id)
    check("references are normalized to sorted, trimmed form",
          first.trace_refs == ["abc1234", "def5678"])

    changed = promote.Candidate.new(
        {"model": "sonnet", "limits": {"turns": 3, "tools": ["read"]}},
        ["abc1234", "9999999"],
        ["evidence://review-1"],
    )
    check("the address changes when a trace reference changes",
          first.id != changed.id)

    tampered = promote.Candidate.new({"model": "sonnet"}, ["abc1234"], ["evidence://one"])
    tampered.trace_refs = ["deadbee"]
    check("integrity rejects a reference swapped after construction",
          raises(tampered.verify_integrity))

    check("a candidate with no trace reference is refused",
          raises(lambda: promote.Candidate.new({"model": "sonnet"}, [], ["evidence://one"])))

    # --- the binary gate: did the cell resolve? ------------------------------

    policy = promote.BinaryPolicy(min_pairs=3, max_p_value=0.05)

    # Every cell resolved under both layers: nothing separates them, and a gate that reads this as
    # agreement rather than as absence of evidence is the whole failure mode being guarded against.
    concordant = [promote.PairedSample(f"T{i}", 1.0, 1.0) for i in range(1, 5)]
    decision = policy.decide(concordant)
    check("concordant pairs carry no evidence and the gate says so",
          not decision.passed
          and any("discordant" in failure for failure in decision.failures))

    # Three tasks, all three favouring the candidate — the exact shape of the first pass's
    # headline. Exact one-sided sign test: p = 1/8 = 0.125, which does not clear 0.05.
    three_for_three = [
        promote.PairedSample("T1", 1.0, 0.0),
        promote.PairedSample("T2", 1.0, 0.0),
        promote.PairedSample("T3", 1.0, 0.0),
    ]
    decision = policy.decide(three_for_three)
    check("three tasks pointing the same way do not clear the sign test",
          not decision.passed and abs(decision.p_value - 0.125) < 1e-12)

    eight_for_eight = [promote.PairedSample(f"T{i}", 1.0, 0.0) for i in range(1, 9)]
    decision = policy.decide(eight_for_eight)
    check("eight discordant pairs all favouring the candidate do clear it",
          decision.passed and abs(decision.p_value - 1 / 256) < 1e-12)

    # A pair the candidate lost is evidence against it, and must not be silently dropped.
    seven_and_one = eight_for_eight[:7] + [promote.PairedSample("T8", 0.0, 1.0)]
    check("a lost pair weakens the same sample size",
          policy.decide(seven_and_one).p_value > policy.decide(eight_for_eight).p_value)

    # --- the continuous gate: turns and cost ---------------------------------

    # Fewer turns is better, so the candidate is the smaller number.
    turns = promote.ContinuousPolicy(
        min_pairs=3, confidence=0.95, resamples=2000, seed=7, higher_is_better=False
    )

    # The real H1-vs-H3 turn counts, per task, from ABLATION.md's second pass. H3 wins every task
    # on the mean, and the within-layer spread (16 and 63 on T3) is the size of the gap.
    measured = [
        promote.PairedSample("T1", 13.5, 53.5),
        promote.PairedSample("T2", 34.5, 44.0),
        promote.PairedSample("T3", 39.5, 67.0),
    ]
    decision = turns.decide(measured)

    # The trap, in the project's own numbers: the interval DOES exclude zero, so a gate whose only
    # continuous criterion is "the interval misses no-effect" — which is what a mean-and-win-rate
    # policy amounts to — would promote H3 here. The second pass says that reading did not hold.
    check("an interval that excludes zero is not on its own enough to promote",
          decision.ci_low > 0.0 and not decision.passed)

    check("with three tasks the interval is wider than the effect it measures",
          (decision.ci_high - decision.ci_low) > decision.point_estimate)

    check("the bootstrap is deterministic given a seed",
          turns.decide(measured) == turns.decide(measured))

    # 200 resamples put the same data's interval at [15.5, 40.0] instead of [9.5, 40.0] — narrow
    # enough to pass the precision check it should fail. A module that refuses to conclude from too
    # little cannot accept a bootstrap too small to support its own interval.
    check("a bootstrap too small to support the interval is refused",
          raises(lambda: promote.ContinuousPolicy(
              min_pairs=3, confidence=0.95, resamples=200, seed=7, higher_is_better=False)))

    # Every failed condition is reported, not just the first one found.
    strict = promote.ContinuousPolicy(
        min_pairs=99, confidence=0.95, resamples=2000, seed=7, higher_is_better=False
    )
    decision = strict.decide(measured)
    check("all failed conditions are reported, not only the first",
          len(decision.failures) >= 2)

    # --- the command line ----------------------------------------------------

    # Exit codes follow `score.sh`'s discipline, and for its reason: a gate that cannot decide
    # must not be read as a gate that said no. 0 promote, 1 refused, 2 cannot decide, 3 usage.
    def run_gate(cells, *args):
        work = tempfile.mkdtemp()
        ledger = os.path.join(work, "eval-cells.jsonl")
        for cell in cells:
            ingest.append_cell(ledger, cell, run_ids=[], source="manual")
        return subprocess.run(
            [sys.executable, os.path.join(HERE, "promote.py"), "--ledger", ledger, *args],
            capture_output=True, text=True,
        )

    clean = []
    for index, (cand, base) in enumerate(
            [(30, 50), (29, 50), (31, 50), (30, 51), (28, 50), (32, 51)], start=1):
        clean.append(ingest.Cell(f"K{index}", "H3", "solved", turns=cand))
        clean.append(ingest.Cell(f"K{index}", "H1", "solved", turns=base))
    result = run_gate(clean, "--candidate", "H3", "--baseline", "H1",
                      "--metric", "turns", "--min-pairs", "6")
    # Asserting on the output too, not just the code: a script with no command line at all exits 0
    # without doing anything, and a test that only reads the code would call that a pass.
    check("a gate that passes exits 0 and says so",
          result.returncode == 0 and "PROMOTE" in result.stdout)

    noisy = [
        ingest.Cell("T1", "H3", "solved", turns=13), ingest.Cell("T1", "H3", "solved", turns=14),
        ingest.Cell("T1", "H1", "solved", turns=42), ingest.Cell("T1", "H1", "solved", turns=65),
        ingest.Cell("T2", "H3", "solved", turns=33), ingest.Cell("T2", "H3", "solved", turns=36),
        ingest.Cell("T2", "H1", "solved", turns=44),
        ingest.Cell("T3", "H3", "solved", turns=16), ingest.Cell("T3", "H3", "solved", turns=63),
        # T3 under H1 resolved once and failed once — the second pass's finding, kept verbatim.
        ingest.Cell("T3", "H1", "solved", turns=94),
        ingest.Cell("T3", "H1", "not-solved", turns=40),
    ]
    result = run_gate(noisy, "--candidate", "H3", "--baseline", "H1",
                      "--metric", "turns", "--min-pairs", "3")
    check("a gate that refuses exits 1", result.returncode == 1)
    check("the refusal names the failed condition", "interval width" in result.stdout)
    check("the report shows the spread behind each pair", "16, 63" in result.stdout)

    absent = [ingest.Cell("T1", "H3", "solved", turns=13)]
    result = run_gate(absent, "--candidate", "H3", "--baseline", "H1", "--metric", "turns")
    check("nothing to compare is cannot-decide, not refusal", result.returncode == 2)

    result = run_gate(noisy, "--candidate", "H3", "--baseline", "H1", "--metric", "verdict")
    check("a verdict that varies between runs is cannot-decide, not refusal",
          result.returncode == 2)

    total = 20
    print(f"\n{total - failures}/{total} as expected")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
