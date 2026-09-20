#!/usr/bin/env python3
"""Refuse a review packet that contradicts itself.

The sibling of ``evidence_gate.py``, and the same promise: this gate can refuse
but it can never approve.  Exit 0 means nothing here could be refused
deterministically, which is not the same as a good review and must never be read
as one -- the checklist above the verdict is a model's judgement and stays
advisory, exactly as `workflow.md` Rule 6 says.

What IS decidable without judgement is whether the packet agrees with itself.
Until now nothing checked that at all: `execute.md` had a gate and `review.md`
had none, so the one packet whose whole output is a verdict was the one nobody
verified.  Three contradictions are caught here.

**An approval standing on top of its own findings.** A verdict of ``approve``
beside an unchecked hard gate, a named potential regression or an unjustified
missing test is not an approval, it is a description of work that is not done
wearing the wrong word.  A reader downstream sees only the verdict.

**A review that never says what it could not see.** `Limitations:` is required
and may be ``none``, but it has to be answered.  An unbounded claim is the
failure mode of a review that ran out of context, could not build, or never
opened half the diff, and the reader has no way to tell that from a thorough
one -- both end in the same word.

**A plan that moved after it was approved.** `Plan sha256:` pins the raw bytes
of the persisted plan as they stood at approval.  Re-derived here: if the file
has changed since, the execution under review implemented something other than
what was agreed, and the review is judging it against a plan that no longer
exists.  Distinct from the `prompt_sha256` in `metrics.jsonl`, which pins the
bytes of a DISPATCHED PROMPT after the fact so a ledger audit can tell an
amended archive from an original.  That one is an audit of what was sent; this
one is a precondition of what is being judged.

Malformed packets fail closed (exit 2).  A packet still carrying the schema's
placeholders exits 1 as "not yet authored", the same distinction and for the
same reason as `evidence_gate.UnfilledPacket`: it sends a different reader
somewhere different, and it still refuses.
"""

from __future__ import annotations

import hashlib
import re
import sys
from pathlib import Path


ROOT = Path(__file__).resolve().parent.parent
HEADING = re.compile(r"^##\s+(.+?)\s*$")
FIELD = re.compile(r"^([A-Z][A-Za-z0-9 /]*?):\s*(.*)$")
UNCHECKED = re.compile(r"^\s*-\s+\[ \]\s*(.+?)\s*$")

#: The three words a verdict may be, from `review.md`.  Anything else is a
#: packet this gate cannot interpret rather than one it disagrees with.
VERDICTS = ("approve", "request-changes", "escalate")

#: Answers that mean "nothing to report" wherever a field may legitimately be
#: empty.  Compared case-folded and stripped of trailing punctuation, because
#: `none` and `None.` are the same answer and refusing the second would teach
#: reviewers to fight the gate rather than fill the packet.
NOTHING = ("none", "n/a", "na", "nothing", "-")


class MalformedPacket(ValueError):
    """The packet cannot be safely interpreted."""


class UnfilledPacket(ValueError):
    """The packet is intact but still carries the schema's placeholders."""


def _empty(value: str) -> bool:
    """Whether a field's value says "nothing to report"."""
    return value.strip().rstrip(".").casefold() in NOTHING or not value.strip()


def _placeholder(value: str) -> bool:
    """Whether a field still holds the schema's own HTML comment."""
    return "<!--" in value


def fields(lines: list[str]) -> dict[str, str]:
    """Every ``Name: value`` line written at column zero, first occurrence winning.

    The whole document and not just its header, because `review.md` does not keep
    its fields in one place: `Task ID:` and `Risk level:` sit above `## Hard
    gates`, while `Potential regressions:`, `Missing tests:` and `Verdict:` --
    every field this gate actually judges -- sit BELOW `## Review Checklist`.  A
    reader that stopped at the first heading would find no verdict in any real
    packet and call all of them malformed.

    Column zero is what keeps a sentence from being read as a field.  A reviewer
    writing a regression over several indented lines can easily produce one
    shaped like ``Verdict: approve was wrong here``, and mistaking that for the
    verdict would invert this gate's entire answer.  On its own line and indented
    it is prose; at column zero it is a field.  `- [ ]` list items are skipped
    for the same reason.
    """
    found: dict[str, str] = {}
    for line in lines:
        if line.startswith(("-", " ", "\t", "#")):
            continue
        match = FIELD.match(line)
        if match:
            found.setdefault(match.group(1), match.group(2))
    return found


def section(lines: list[str], name: str) -> list[str]:
    """Return a level-two heading's body, or an empty body when it is absent.

    Softer than `evidence_gate.section`, deliberately.  There the section holds
    the evidence and its absence is the whole failure; here an absent `## Hard
    gates` leaves one of three checks with nothing to say while the other two
    still stand, and failing the packet closed over it would refuse reviews for
    a heading rather than for a contradiction.  The verdict check below is what
    fails closed.
    """
    start = None
    for index, line in enumerate(lines):
        match = HEADING.match(line)
        if match and match.group(1) == name:
            start = index + 1
            break
    if start is None:
        return []
    end = len(lines)
    for index in range(start, len(lines)):
        if HEADING.match(lines[index]):
            end = index
            break
    return lines[start:end]


def verdict_of(found: dict[str, str]) -> str:
    """The verdict, lowercased, or a refusal explaining which way it is wrong."""
    if "Verdict" not in found:
        raise MalformedPacket("missing Verdict:")
    raw = found["Verdict"]
    if _placeholder(raw):
        raise UnfilledPacket("Verdict: still holds the schema template")
    value = raw.strip().casefold()
    if value not in VERDICTS:
        raise MalformedPacket(
            f"Verdict: {raw.strip()!r} is not one of {', '.join(VERDICTS)}"
        )
    return value


def findings(lines: list[str], found: dict[str, str]) -> list[str]:
    """Everything in this packet that says the work is not finished."""
    material: list[str] = []
    for gate in section(lines, "Hard gates"):
        match = UNCHECKED.match(gate)
        if match:
            material.append(f"hard gate not met: {match.group(1)}")
    for name in ("Potential regressions", "Missing tests"):
        value = found.get(name, "")
        if _placeholder(value):
            raise UnfilledPacket(f"{name}: still holds the schema template")
        if not _empty(value):
            material.append(f"{name}: {value.strip()}")
    return material


def plan_moved(found: dict[str, str]) -> str | None:
    """Whether the persisted plan still hashes to what approval pinned.

    Silent when either field says `none`: a task whose plan was never written to
    a file has no bytes to pin, and inventing a complaint about that would make
    the field noise on every trivial task instead of a guard on the ones that
    have a plan to move.
    """
    recorded = found.get("Plan sha256", "")
    where = found.get("Plan path", "")
    if _placeholder(recorded) or _placeholder(where):
        raise UnfilledPacket("Plan sha256:/Plan path: still hold the schema template")
    if _empty(recorded) or _empty(where):
        return None
    path = ROOT / where.strip()
    try:
        actual = hashlib.sha256(path.read_bytes()).hexdigest()
    except OSError as exc:
        return f"the plan this review pins cannot be read: {exc}"
    if actual != recorded.strip():
        return (
            f"{where.strip()} has changed since the plan was approved -- pinned "
            f"{recorded.strip()[:12]}..., file is {actual[:12]}.... The execution "
            f"under review implemented a plan that no longer exists"
        )
    return None


def check_packet(text: str) -> list[str]:
    """Every deterministic contradiction in this packet, in the reader's words."""
    lines = text.splitlines()
    if not lines:
        raise MalformedPacket("empty packet")
    found = fields(lines)
    verdict = verdict_of(found)
    refusals: list[str] = []

    if "Limitations" not in found:
        refusals.append(
            "no Limitations: field -- a review that does not say what it could not "
            "inspect is an unbounded claim. `none` is an answer; silence is not"
        )
    elif _placeholder(found["Limitations"]):
        raise UnfilledPacket("Limitations: still holds the schema template")

    material = findings(lines, found)
    if verdict == "approve" and material:
        refusals.append("approved while its own findings say the work is not done:")
        refusals.extend(f"  - {item}" for item in material)

    moved = plan_moved(found)
    if moved:
        refusals.append(moved)
    return refusals


def check_path(path: Path) -> int:
    try:
        return report(check_packet(path.read_text(encoding="utf-8")))
    except UnfilledPacket as exc:
        print(f"review gate: packet not yet authored: {exc}", file=sys.stderr)
        return 1
    except (OSError, UnicodeError, MalformedPacket) as exc:
        print(f"review gate: malformed input: {exc}", file=sys.stderr)
        return 2


def report(refusals: list[str]) -> int:
    if not refusals:
        return 0
    print("review gate: this packet contradicts itself:", file=sys.stderr)
    for line in refusals:
        print(f"- {line}" if not line.startswith("  ") else line, file=sys.stderr)
    return 1


def main(argv: list[str]) -> int:
    if len(argv) > 1:
        print("usage: review_gate.py [packet-path]", file=sys.stderr)
        return 2
    if argv:
        return check_path(Path(argv[0]))
    try:
        return report(check_packet(sys.stdin.read()))
    except UnfilledPacket as exc:
        print(f"review gate: packet not yet authored: {exc}", file=sys.stderr)
        return 1
    except MalformedPacket as exc:
        print(f"review gate: malformed input: {exc}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
