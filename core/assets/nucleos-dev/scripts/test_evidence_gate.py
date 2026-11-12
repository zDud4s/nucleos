#!/usr/bin/env python3
"""Hermetic coverage for ``evidence_gate.py``.

Run: python test_evidence_gate.py
"""

import subprocess
import sys
import tempfile
from pathlib import Path

GATE = Path(__file__).resolve().parent / "evidence_gate.py"


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

# Batched mode is the only mode: a command may be deferred to the controller's one batched run.
deferred_ok = run(packet("$ python check.py\ndeferred: batched"))
assert deferred_ok.returncode == 0, deferred_ok.stderr
deferred_wrong_command = run(packet("$ python other.py\ndeferred: batched"))
assert deferred_wrong_command.returncode == 1 and "python check.py" in deferred_wrong_command.stderr
deferred_missing = run(packet(""))
assert deferred_missing.returncode == 1, deferred_missing.stderr

missing = run(packet("", commands="- `python check.py`\nTest runs: batched"))
assert deferred_missing.returncode == 1, deferred_missing.stderr

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

# A packet still carrying the schema's own comment has not been authored. It must still
# refuse -- nothing is proven -- but as "not yet authored", not as damage. Reporting it
# as malformed is what sent somebody looking for a corrupt file on 2026-09-19 when the
# real fault was a controller writing the active-packet marker before filling the packet.
TEMPLATE = """  <!-- exact commands to run after implementation.
  Test commands the executor hands back. -->"""

unfilled = run(packet("", commands=TEMPLATE))
assert unfilled.returncode == 1, unfilled.stderr
assert "not yet authored" in unfilled.stderr, unfilled.stderr
assert "malformed" not in unfilled.stderr, unfilled.stderr

# The real schema opens the comment on the `Commands:` line itself, so the detection
# cannot look only at the lines below it.
inline = run(
    "# Execution Packet\n\n## Validation\nCommands: <!-- what to run\n  and why -->\n"
    "Expected result: <!-- what success looks like -->\n\n## Handoff\nValidation evidence:\n"
)
assert inline.returncode == 1 and "not yet authored" in inline.stderr, inline.stderr

# With the placeholder deliberately removed and nothing put in its place, the packet is
# authored and wrong -- that stays exit 2.
empty = run(packet("$ python check.py\nexit: 0\ntail: all good", commands=""))
assert empty.returncode == 2 and "has no commands" in empty.stderr, empty.stderr


# The final verification: a `verify` ticket and its result stand in for exit + tail.
ticket_ok = run(packet("$ python check.py\nverify ticket t-42\nresult: green"))
assert ticket_ok.returncode == 0, ticket_ok.stderr
ticket_no_result = run(packet("$ python check.py\nverify ticket t-42"))
assert ticket_no_result.returncode == 1 and "python check.py" in ticket_no_result.stderr
result_no_ticket = run(packet("$ python check.py\nresult: green"))
assert result_no_ticket.returncode == 1, result_no_ticket.stderr

# No marker machinery any more: the hook mode is gone.
gone = subprocess.run([sys.executable, str(GATE), "--hook"], input="", text=True, capture_output=True)
assert gone.returncode != 0 and not (Path(__file__).parent / "set_active_packet.py").exists()

print("evidence gate tests passed")
