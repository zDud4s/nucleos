#!/usr/bin/env python3
"""Cover `scripts/check_invariants.py`: the same cases core/tests/file_size.rs and module_map.rs hold.

Hermetic: the module-map checks run on a throwaway tree, and the counting rule on strings.

Run:  python scripts/test-check-invariants.py
"""

import contextlib
import importlib.util
import io
import tempfile
import unittest
from pathlib import Path

SCRIPT = Path(__file__).resolve().parent / "check_invariants.py"
spec = importlib.util.spec_from_file_location("check_invariants", SCRIPT)
ci = importlib.util.module_from_spec(spec)
spec.loader.exec_module(ci)


class CodeLines(unittest.TestCase):
    def test_only_code_is_counted(self):
        source = (
            "//! module doc\nuse std::fs;\n\n/// item doc\npub mod child;\n#[cfg(test)]\nmod fixture;\n\n"
            "fn a() {\n    // a comment\n    /* one line */\n    /*\n       a block\n    */\n"
            "    *value = 1; // a trailing comment stays code\n}\n\n#[cfg(test)]\nfn helper() {}\n\n"
            "#[cfg(test)]\npub(crate) mod tests {\n    fn not_counted() {}\n}\n"
        )
        self.assertEqual(ci.code_lines(source), 6)

    def test_code_after_a_mid_file_test_module_is_counted(self):
        source = "fn a() {}\n\n#[cfg(test)]\nmod tests {\n    fn not_counted() {\n        let x = 1;\n    }\n}\n\nfn b() {}\n"
        self.assertEqual(ci.code_lines(source), 2)

    def test_crlf_counts_like_lf(self):
        self.assertEqual(ci.code_lines("fn a() {}\r\n\r\nfn b() {}\r\n"), 2)


class Ceilings(unittest.TestCase):
    def test_a_file_may_shrink_under_its_ceiling_but_not_grow_past_it(self):
        ceilings = {"src/big.rs": 1500}
        self.assertEqual(ci.size_problems_for({"src/big.rs": 1400}, ceilings), [])
        self.assertEqual(len(ci.size_problems_for({"src/big.rs": 1501}, ceilings)), 1)
        self.assertEqual(len(ci.size_problems_for({"src/big.rs": 900}, ceilings)), 1)

    def test_a_new_file_past_the_limit_fails_and_a_stale_line_fails(self):
        found = ci.size_problems_for({"src/new.rs": ci.LIMIT + 1}, {"src/gone.rs": 2000})
        self.assertEqual(len(found), 2, found)

    def test_baseline_skips_comments_and_blank_lines(self):
        self.assertEqual(ci.baseline("# c\n\nsrc/a.rs 1200\n"), {"src/a.rs": 1200})


class ModuleMap(unittest.TestCase):
    def tree(self, rows, files):
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        core = Path(tmp.name) / "core"
        for name in files:
            path = core / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text("fn a() {}\n")
        body = "".join(f"| `{row}` | does something |\n" for row in rows)
        (core / "MODULE_MAP.md").write_text(f"## Module map\n\n| File | R |\n|---|---|\n{body}")
        return core

    def test_a_matching_map_is_clean(self):
        core = self.tree(
            ["main.rs", "verify/batch.rs", "crates/nucleos-base/src/lib.rs"],
            ["src/main.rs", "src/verify/batch.rs", "crates/nucleos-base/src/lib.rs"],
        )
        self.assertEqual(ci.module_map_problems(core), [])

    def test_a_file_without_a_row_and_a_row_without_a_file_both_fail(self):
        core = self.tree(["main.rs", "gone.rs"], ["src/main.rs", "src/new.rs"])
        found = ci.module_map_problems(core)
        self.assertEqual(len(found), 2, found)
        self.assertTrue(any("new.rs" in line and "no row" in line for line in found))
        self.assertTrue(any("gone.rs" in line for line in found))

    def test_a_row_outside_the_map_section_does_not_count(self):
        core = self.tree(["main.rs"], ["src/main.rs", "src/other.rs"])
        with open(core / "MODULE_MAP.md", "a") as f:
            f.write("\n## Elsewhere\n\n| `other.rs` | not the map |\n")
        self.assertEqual(len(ci.module_map_problems(core)), 1)


class RealRepo(unittest.TestCase):
    def test_this_checkout_passes(self):
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            code = ci.main()
        self.assertEqual((code, out.getvalue()), (0, ""))


if __name__ == "__main__":
    unittest.main()
