#!/usr/bin/env python3
"""Cover `scripts/statusline-context.py`.

A status line is read a hundred times a day and questioned never. If the number
is wrong nobody finds out — it is not attached to an assertion anywhere, and the
whole reason it exists is that the quantity it reports was previously invisible.
So the arithmetic gets a test even though the script only prints.

Three properties, each of which was a decision rather than an accident:

  context    is input + cache_read + cache_creation, and EXCLUDES output. Output
             is not re-read on the next turn, so it is not what the session is
             carrying. (It is also 0.4% of the total, so including it would be
             wrong and invisible at the same time -- the worst combination.)
  own lane   a subagent turn (``isSidechain``) is skipped. It runs in its own
             window; counting it would make the number jump and drop back for
             no reason a reader could see.
  tail first transcripts here reach hundreds of megabytes and this runs on every
             redraw, so the first read is the last 256 KB. It WIDENS when that
             finds no turn at all -- one screenshot record fills the window on
             its own and the number used to vanish, silently, right after the
             turn that most deserved one -- and gives up past MAX_SCAN. The test
             writes padding past the first window and checks all three.

Run:  python scripts/test-statusline-context.py
"""

import importlib.util
import json
import os
import sys
import tempfile

ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), ".."))
TARGET = os.path.join(ROOT, "scripts", "statusline-context.py")

spec = importlib.util.spec_from_file_location("statusline_context", TARGET)
statusline = importlib.util.module_from_spec(spec)
spec.loader.exec_module(statusline)


def turn(fresh=0, cread=0, ccreate=0, out=0, sidechain=False):
    return {
        "type": "assistant",
        "isSidechain": sidechain,
        "message": {"usage": {
            "input_tokens": fresh,
            "cache_read_input_tokens": cread,
            "cache_creation_input_tokens": ccreate,
            "output_tokens": out,
        }},
    }


def write(path, records):
    with open(path, "w", encoding="utf-8") as fh:
        for r in records:
            fh.write((r if isinstance(r, str) else json.dumps(r)) + "\n")
    return path


def cases(tmp):
    p = os.path.join(tmp, "t.jsonl")

    write(p, [turn(fresh=100, cread=9000, ccreate=500, out=99999)])
    yield ("context excludes output", statusline.last_context(p), 9600)

    write(p, [turn(fresh=10), turn(fresh=777, sidechain=True)])
    yield ("a subagent turn is skipped", statusline.last_context(p), 10)

    write(p, [turn(fresh=1), turn(fresh=2), turn(fresh=3)])
    yield ("the LAST assistant turn wins", statusline.last_context(p), 3)

    write(p, [turn(fresh=5), {"type": "user", "message": {"usage": {"input_tokens": 999}}}])
    yield ("a non-assistant record is ignored", statusline.last_context(p), 5)

    write(p, [turn(fresh=8), "{broken json"])
    yield ("an unparseable trailing line does not hide the answer",
           statusline.last_context(p), 8)

    write(p, [{"type": "assistant", "message": {}}])
    yield ("an assistant turn with no usage yields None", statusline.last_context(p), None)

    yield ("a missing transcript yields None",
           statusline.last_context(os.path.join(tmp, "absent.jsonl")), None)

    # The tail is read first, and an old value buried under more than
    # TAIL_BYTES of padding must not beat the recent one.
    padding = json.dumps({"type": "padding", "blob": "x" * 4000})
    n = (statusline.TAIL_BYTES // len(padding)) + 40
    write(p, [turn(fresh=111111)] + [padding] * n + [turn(fresh=42)])
    yield ("the first read finds the recent turn",
           statusline.last_context(p), 42)

    # The regression, and the reason this widens at all: a screenshot arrives as
    # one record of megabytes on one line, which fills the first window by
    # itself. The old tail-only read printed nothing here -- no number, no
    # error, right after the most expensive turn in the session.
    write(p, [turn(fresh=111111)] + [padding] * n)
    yield ("a turn past the first window is still found",
           statusline.last_context(p), 111111)

    # But bounded: past MAX_SCAN the number is given up rather than reading a
    # 400 MB transcript on every redraw. Shrunk here so the file need not be.
    was = (statusline.TAIL_BYTES, statusline.MAX_SCAN)
    statusline.TAIL_BYTES, statusline.MAX_SCAN = 2048, 8192
    write(p, [turn(fresh=111111)] + [padding] * n)
    yield ("a turn past MAX_SCAN is given up on", statusline.last_context(p), None)
    statusline.TAIL_BYTES, statusline.MAX_SCAN = was

    # Ceiling
    for raw, want, label in [(None, 250_000, "absent"), ("100000", 100_000, "valid"),
                             ("nonsense", 250_000, "unparseable"), ("0", 250_000, "zero"),
                             ("-5", 250_000, "negative")]:
        if raw is None:
            os.environ.pop("NUCLEOS_CONTEXT_CEILING", None)
        else:
            os.environ["NUCLEOS_CONTEXT_CEILING"] = raw
        yield (f"ceiling from a {label} env value", statusline.ceiling(), want)
    os.environ.pop("NUCLEOS_CONTEXT_CEILING", None)


def main() -> int:
    failures = 0
    total = 0
    with tempfile.TemporaryDirectory() as tmp:
        for label, got, want in cases(tmp):
            total += 1
            if got != want:
                failures += 1
                print(f"FAIL {label}: expected {want!r}, got {got!r}")
    print(f"{total - failures}/{total} as expected")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
