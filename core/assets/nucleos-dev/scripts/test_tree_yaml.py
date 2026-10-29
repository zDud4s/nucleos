#!/usr/bin/env python3
"""Tests for tree_yaml.py, the minimal vendored YAML reader the tree engine uses.

    python -m pytest test_tree_yaml.py
"""

from __future__ import annotations

import sys
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
WORKFLOW = HERE.parent / ".nucleos" / "workflow"

import tree_yaml  # noqa: E402


def load(text):
    return tree_yaml.safe_load(text)


class Structure(unittest.TestCase):
    def test_nested_mappings(self):
        self.assertEqual(load("a:\n  b:\n    c: 1\n  d: x\ne: y\n"),
                         {"a": {"b": {"c": 1}, "d": "x"}, "e": "y"})

    def test_block_lists(self):
        self.assertEqual(load("items:\n  - x\n  - y\n"), {"items": ["x", "y"]})

    def test_block_list_at_the_key_indent(self):
        self.assertEqual(load("items:\n- x\n- y\nafter: 1\n"), {"items": ["x", "y"], "after": 1})

    def test_flow_lists(self):
        self.assertEqual(load("k: [a, b, 3]\n"), {"k": ["a", "b", 3]})
        self.assertEqual(load("k: []\n"), {"k": []})

    def test_simple_flow_mapping(self):
        self.assertEqual(load("k: {a: 1, b: two}\n"), {"k": {"a": 1, "b": "two"}})
        self.assertEqual(load("k: {}\n"), {"k": {}})

    def test_empty_file_is_none(self):
        self.assertIsNone(load(""))
        self.assertIsNone(load("\n\n"))
        self.assertIsNone(load("# only a comment\n"))


class Scalars(unittest.TestCase):
    def test_numbers(self):
        got = load("i: 15\nneg: -3\nf: 2.5\n")
        self.assertEqual(got, {"i": 15, "neg": -3, "f": 2.5})
        self.assertIsInstance(got["i"], int)
        self.assertIsInstance(got["f"], float)

    def test_booleans(self):
        got = load("a: true\nb: false\n")
        self.assertIs(got["a"], True)
        self.assertIs(got["b"], False)

    def test_null(self):
        self.assertEqual(load("a: null\nb: ~\nc:\n"), {"a": None, "b": None, "c": None})

    def test_quoted_strings_stay_strings(self):
        self.assertEqual(load("a: \"12\"\nb: 'true'\nc: \"x: y\"\nd: 'null'\n"),
                         {"a": "12", "b": "true", "c": "x: y", "d": "null"})

    def test_plain_strings(self):
        self.assertEqual(load("a: hello world\nb: v1.2.x\n"), {"a": "hello world", "b": "v1.2.x"})


class Comments(unittest.TestCase):
    def test_full_line_and_trailing_comments(self):
        text = "# head\na: 1   # trailing\n\n  # indented comment\nb: x\n"
        self.assertEqual(load(text), {"a": 1, "b": "x"})

    def test_hash_inside_quotes_is_kept(self):
        self.assertEqual(load("a: \"x # y\"\n"), {"a": "x # y"})


class Errors(unittest.TestCase):
    def test_yaml_error_is_an_exception_class(self):
        self.assertTrue(issubclass(tree_yaml.YAMLError, Exception))

    def test_duplicate_key_is_an_error(self):
        with self.assertRaises(tree_yaml.YAMLError):
            load("a: 1\na: 2\n")

    def test_duplicate_key_in_a_nested_mapping_is_an_error(self):
        with self.assertRaises(tree_yaml.YAMLError):
            load("t:\n  a: 1\n  a: 2\n")

    def test_tab_indentation_is_an_error(self):
        with self.assertRaises(tree_yaml.YAMLError):
            load("a:\n\tb: 1\n")


class Parity(unittest.TestCase):
    def test_speed_yaml_loads_like_pyyaml(self):
        real = __import__("pytest").importorskip("yaml")
        text = (WORKFLOW / "speed.yaml").read_text(encoding="utf-8")
        self.assertEqual(load(text), real.safe_load(text))

    def test_a_project_yaml_example_loads_like_pyyaml(self):
        real = __import__("pytest").importorskip("yaml")
        text = (
            "# project\n"
            "tree:\n"
            "  worktrees_dir: .claude/worktrees\n"
            "  base_branch: master\n"
            "  floor_gb: 20\n"
            "  links: [shell/node_modules, target]\n"
            "  heavy:\n"
            "    - scripts/heavy.py\n"
            "    - cargo\n"
            "  flag: false   # trailing\n"
        )
        self.assertEqual(load(text), real.safe_load(text))


if __name__ == "__main__":
    unittest.main()
