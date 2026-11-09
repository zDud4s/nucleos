---
name: model-requests
description: Use when an agent writes or changes a request to a decision or classification model (TypeSafe Jev, or a chat model used as a classifier, judge or scorer), or evaluates or debugs the quality of such a request
---

# Model requests

How to write, evaluate and debug a request to a model that makes a decision. The rules below
come from measurement (2026-11), not from taste. Each carries its evidence strength; a rule
backed by one use is labelled so, and an open question is labelled open. Do not promote a rule
beyond its evidence, and do not drop one because it feels obvious.

Companion files in this directory:

- `jev.md`: the TypeSafe Jev request reference (endpoint, question types, criteria shapes, price).
- `uses.md`: per-use extensions: judge, size prediction, router, compaction, chat models.

## Anatomy of a request

Every request has four parts. Name them before changing any of them.

1. **State**: the evidence the model reads. It is the only information the model has.
2. **Question(s)**: what is decided. One question per decision you will act on.
3. **Output read**: a probability, a choice, or a score taken from the response.
4. **Decision rule**: what the program does with the output: thresholds, bands (allow, ask,
   deny), combinations such as `min()` over several questions.

A quality problem lives in exactly one of these. Find which before editing.

## Rules and their evidence

**R1. Ask exactly the decision you will act on and the label you will measure: same threshold,
same population, same definition.** Evidence: 2 uses. In the judge, many "harmful" red-team
labels were project-policy labels that neither question asks about, so they looked like model
failures. In size prediction, the first label counted only main-thread tool calls while the
question's threshold differed from the label's. Applies to the request AND to the eval.

**R2. Before blaming wording, score the same states with (a) a trivial baseline (length, a
previous-count feature) and (b) a strong chat model given exactly the same state, no tools, no
labels, empty working directory.** If all are low, the information is not in the state: change
the state, not the wording. Never give the reference model information the classifier will not
have (such as the eventual transcript). Evidence: 2 uses. Size: Jev 0.65, haiku 0.71, codex
terra 0.72, sonnet 0.74, baseline 0.60, heldout all about 0.5-0.67, so information-limited.
Judge: Jev 0.992 AUC against sonnet 1.000 and codex 0.998, so near ceiling on ranking and
behind on margin.

**R3. Rewording moves Jev little. A dev-set gain must replicate on a heldout set fixed BEFORE
the results, with hypotheses written before the numbers, and be tested with a paired case
bootstrap.** Evidence: 2 uses. Judge: 3 rewordings, all within +-0.003 AUC and +-2 positives.
Size: a label-matched reframing gained +0.044 [+0.019, +0.070] on dev and -0.002 on heldout.

**R4. Judge decision quality by margin, not by AUC alone**: coverage of positives at the
threshold where zero negatives pass, and the highest negative probability. Evidence: 1 use
(2 models compared). Jev had AUC 0.992 yet its top negative was 0.847 against a 0.85 allow
line, allowing 40 of 60 positives; sonnet's top negative was 0.12, allowing 60 of 60.

**R5. Criteria that only restate the question add nothing and can hurt.** Evidence: one use,
2 datasets. Judge safe AUC went 0.928 to 0.864 on dev; no change on the blind set.

Older, one-use evidence (weaker, treat as a default to re-test, not a law):

- No biasing preamble in the state or question (judge, compaction).
- Concrete wording and recent actions in the state help the judge.

Open (not a rule): describing who acts and when the work ends in the state (size
prediction). It gained on dev only and was never replicated on heldout.

## Evaluation procedure

1. Fix the labels before any result is seen, and write them in the same definition the request
   asks (R1).
2. Split dev and heldout by session or episode, never by row, so near-duplicates do not leak.
3. Write the hypotheses before running the numbers.
4. Run repeats and average per case. Jev repeat noise is small (within-case std about
   0.005-0.03), so a few repeats are enough.
5. Report AUC with a case-bootstrap confidence interval, and paired differences between
   variants. A variant whose interval spans zero did not win.
6. Always include the baselines of R2.
7. Estimate cost first. `chars/4` under-counts Jev tokens by about 1.3-1.9x; take the counts
   from the response afterwards.
8. Secret-scan every state before it leaves the machine, and get the owner's authorisation for
   egress. Never print or log an API key.

## Rationalization -> Reality

| Rationalization | Reality |
|---|---|
| "The field names (`questions`, `state`, `criteria`) are placeholders to check against docs.typesafe.ai." | Read `jev.md` (or the docs) before writing a body; a guessed schema returns 422 or silently changes the question. |
| "A continuous output is probably the biggest cheap gain." | Jev's `noul` already returns a probability; check what the API returns before proposing changes, or you spend the work on a gain that already exists. |
| "Have a human, or a strong LLM given the full eventual transcript, rate difficulty" | The reference gets the same state only (R2); otherwise it measures leakage, not the ceiling, and the conclusion is wrong. |
| "Answer YES only for loss of untracked, non-regenerable data ... Deleting gitignored build output (target/, node_modules/, dist/) ... is NO." | A measured rewording along these lines (2026-11) did not move cleanup allows (R3); measure on a fixed heldout before assuming the wording is the cause. |
| "Make the threshold per question. Keep 0.85 on `safe` and relax `scope` to about 0.7." | Set thresholds from the margin on labelled negatives (R4), never by feel; a feel-set line lets a negative through unnoticed. |
| "Calibrate the probabilities before feeding the cards." | For a router, the tier choice is set by the cards and the ladder; measured calibration moved tier agreement by +5 points with an interval that includes 0, so a week of calibration leaves the cost where it was. |

## Failure modes

- Comparing variants on dev only, then shipping the best one (R3).
- Reporting AUC without the top negative probability (R4).
- Calling a policy-label miss a model failure; guard policy-only negatives in code (R1, `uses.md`).
- Editing the state and the wording in the same step, so no gain can be attributed.
