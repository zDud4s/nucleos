#!/usr/bin/env python3
"""Tests for packets/node.md as consumed by tree.py: orchestrator rules and the report template.

    python -m pytest core/assets/nucleos-dev/scripts/test_tree_packet.py
"""

from __future__ import annotations

import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))

import tree  # noqa: E402
import tree_state  # noqa: E402

NODE_MD = HERE.parent / ".nucleos" / "workflow" / "packets" / "node.md"


def _sections():
    _, sections = tree.split_sections(NODE_MD.read_text(encoding="utf-8"))
    return sections


def test_orchestrator_rules_do_not_tell_it_to_close_itself():
    rules = "\n".join(tree.role_rules(_sections(), "orchestrator"))
    assert "Close yourself" not in rules


def test_orchestrator_rules_still_have_content():
    assert tree.role_rules(_sections(), "orchestrator")


def test_report_format_has_a_template_subsection():
    subs = tree.subsections(_sections(), "Report format", ("template",))
    assert subs["template"]


def test_filled_template_passes_parse_report():
    subs = tree.subsections(_sections(), "Report format", ("template",))
    values = {"{max_lines}": "60", "{role}": "leaf", "{handoff}": "/tmp/handoff.md"}
    filled = "\n".join(subs["template"])
    for key, value in values.items():
        filled = filled.replace(key, value)
    assert "{max_lines}" not in filled and "{role}" not in filled and "{handoff}" not in filled
    report = tree_state.parse_report(filled)
    assert report["role"] == "leaf"
    assert report["outcome"] in tree_state.OUTCOME_STATUS
    assert report["status"] == tree_state.OUTCOME_STATUS[report["outcome"]]
