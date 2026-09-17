#!/usr/bin/env python3
"""Hermetic coverage for ``scripts/evidence_gate.py``.

Run: python scripts/test-evidence-gate.py
"""

import subprocess
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
GATE = ROOT / "scripts" / "evidence_gate.py"


def packet(evidence: str, commands: str = "- `python check.py`") -> str:
    return f"""# Execution Packet

## Validation
Commands:
{commands}
Expected result: exit 0

## Handoff
Validation evidence:
{evidence}
Deviations from plan: none
"""


def run(contents: str) -> subprocess.CompletedProcess[str]:
    with tempfile.NamedTemporaryFile("w", encoding="utf-8", suffix=".md", delete=False) as handle:
        handle.write(contents)
        path = Path(handle.name)
    try:
        return subprocess.run([sys.executable, str(GATE), str(path)], text=True, capture_output=True)
    finally:
        path.unlink()


def test_inline_tail_is_valid() -> None:
    result = run(packet("$ python check.py\nexit: 0\ntail: all good"))
    assert result.returncode == 0, result.stderr


def test_no_evidence_block_fails() -> None:
    result = run(packet(""))
    assert result.returncode == 1 and "python check.py" in result.stderr, result.stderr


def test_empty_tail_followed_by_fence_fails() -> None:
    result = run(packet("$ python check.py\nexit: 0\ntail:\n```"))
    assert result.returncode == 1 and "python check.py" in result.stderr, result.stderr


def test_multiline_tail_before_fence_is_valid() -> None:
    result = run(packet("$ python check.py\nexit: 0\ntail:\nok 1\nok 2\n```"))
    assert result.returncode == 0, result.stderr


test_inline_tail_is_valid()
test_no_evidence_block_fails()
test_empty_tail_followed_by_fence_fails()
test_multiline_tail_before_fence_is_valid()

missing = run(packet("$ python another.py\nexit: 0\ntail: all good"))
assert missing.returncode == 1 and "python check.py" in missing.stderr, missing.stderr

no_tail = run(packet("$ python check.py\nexit: 0\ntail:"))
assert no_tail.returncode == 1 and "python check.py" in no_tail.stderr, no_tail.stderr

multiline_tail = run(packet("$ python check.py\nexit: 0\ntail:\nall good\nsecond line"))
assert multiline_tail.returncode == 0, multiline_tail.stderr

extra = run(packet("$ python check.py\nexit: 0\ntail: all good\n$ python extra.py\nexit: 0\ntail: irrelevant"))
assert extra.returncode == 0, extra.stderr

malformed = run("# not a packet\n")
assert malformed.returncode == 2 and "malformed input" in malformed.stderr, malformed.stderr

print("evidence gate tests passed")
