"""Tests for heavy_classify: the argv classifier shared by hook, broker and select_tests."""

import importlib.util
import os
import subprocess
import tempfile
import unittest
from pathlib import Path

HERE = Path(__file__).parent


def load():
    # Imported by path so the test does not depend on sys.path. A missing module
    # fails loudly here, which is the RED state.
    spec = importlib.util.spec_from_file_location("heavy_classify", HERE.parent / "heavy_classify.py")
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


class HeavyClassifyTests(unittest.TestCase):
    def setUp(self):
        self.hc = load()

    def v(self, *argv):
        return self.hc.classify(list(argv))

    def test_weights_and_kinds(self):
        cases = [
            (["cargo", "test"], 2, "cargo"),
            (["cargo", "build"], 2, "cargo"),
            (["cargo", "run"], 2, "cargo"),
            (["cargo", "doc"], 2, "cargo"),
            (["cargo", "tauri", "build"], 2, "cargo"),
            (["cargo", "+nightly", "test"], 2, "cargo"),
            (["cargo", "-p", "x", "build"], 2, "cargo"),
            (["C:/Projects/cargo/bin/cargo.exe", "build"], 2, "cargo"),
            (["npm", "run", "tauri", "build"], 2, "node"),
            (["npx", "tauri", "build"], 2, "node"),
            (["cargo", "check"], 1, "cargo"),
            (["cargo", "clippy"], 1, "cargo"),
            (["npm", "test"], 1, "node"),
            (["npx", "vitest"], 1, "node"),
            (["tsc"], 1, "node"),
            (["npx", "tsc"], 1, "node"),
            (["npm", "exec", "tsc"], 1, "node"),
            (["npm", "run", "build"], 1, "node"),
            (["go", "test"], 1, "go"),
            (["go", "build"], 1, "go"),
            (["go", "vet"], 1, "go"),
        ]
        for argv, weight, kind in cases:
            with self.subTest(argv=argv):
                r = self.v(*argv)
                self.assertTrue(r.heavy)
                self.assertEqual(r.weight, weight)
                self.assertEqual(r.kind, kind)
        for argv in (["ls"], ["git", "status"], ["cargo", "fmt"], ["go", "fmt"], ["npm", "install"], []):
            with self.subTest(argv=argv):
                r = self.v(*argv)
                self.assertFalse(r.heavy)
                self.assertIsNone(r.kind)

    def test_filtered_grammar_positive(self):
        cases = [
            ["cargo", "test", "-p", "nucleos-core", "the_name"],
            ["cargo", "test", "the_name"],
            ["cargo", "test", "--", "the_name"],
            ["cargo", "test", "-p", "x", "--", "the_name", "--nocapture"],
            ["cargo", "test", "--test", "module_map"],
            ["npm", "test", "--", "-t", "name"],
            ["npm", "test", "--", "src/foo.test.ts"],
            ["npx", "vitest", "run", "src/foo.test.ts"],
            ["npx", "vitest", "run", "-t", "name"],
            ["go", "test", "-run", "TestX", "./..."],
            ["go", "test", "-run=TestX", "./..."],
        ]
        for argv in cases:
            with self.subTest(argv=argv):
                r = self.v(*argv)
                self.assertTrue(r.heavy)
                self.assertTrue(r.filtered)

    def test_filtered_grammar_negative(self):
        cases = [
            ["cargo", "test"],
            ["cargo", "test", "-p", "x"],
            ["cargo", "test", "-p", "x", "--", "--nocapture"],
            ["cargo", "test", "--features", "foo"],
            ["cargo", "test", "--target-dir", "C:/t"],
            ["cargo", "build", "-p", "x"],
            ["npm", "test"],
            ["npx", "vitest", "run"],
            ["go", "test", "./..."],
            ["go", "test", "-race", "./..."],
        ]
        for argv in cases:
            with self.subTest(argv=argv):
                r = self.v(*argv)
                self.assertTrue(r.heavy)
                self.assertFalse(r.filtered)

    def test_gate_commands_recognised(self):
        for argv in (
            ["python", ".ai/scripts/select_tests.py", "--gate", "--run"],
            ["python3", "C:/Projects/nucleos/.ai/scripts/select_tests.py"],
            ["bash", "scripts/gates.sh", "core"],
            ["C:\\Program Files\\Git\\bin\\bash.exe", "scripts/gates.sh", "all"],
        ):
            with self.subTest(argv=argv):
                self.assertTrue(self.v(*argv).gate)
        r = self.v("bash", "scripts/build-slot.sh", "cargo", "test", "-p", "x")
        self.assertTrue(r.gate)
        self.assertEqual(r.inner, ["cargo", "test", "-p", "x"])
        self.assertFalse(self.v("python", "other.py").gate)
        self.assertFalse(self.v("cargo", "test").gate)

    def test_wrappers_and_env_prefix_peeled(self):
        env, rest = self.hc.split_env(["A=1", "B=two", "cargo", "test"])
        self.assertEqual(env, ["A=1", "B=two"])
        self.assertEqual(rest, ["cargo", "test"])
        env, rest = self.hc.split_env(["cargo", "test", "X=1"])
        self.assertEqual(env, [])
        self.assertEqual(rest, ["cargo", "test", "X=1"])

        r = self.v("python", "C:/Projects/nucleos/scripts/heavy.py", "--prio", "2", "--", "cargo", "build")
        self.assertTrue(r.wrapped)
        self.assertEqual(r.inner, ["cargo", "build"])
        r = self.v("python", "scripts/heavy.py", "--agent", "wf", "--kind", "cargo", "cargo", "check")
        self.assertTrue(r.wrapped)
        self.assertEqual(r.inner, ["cargo", "check"])
        self.assertFalse(self.v("cargo", "build").wrapped)

        # An env prefix does not hide the program.
        r = self.v("CARGO_TARGET_DIR=C:/t", "cargo", "test")
        self.assertTrue(r.heavy)
        self.assertEqual(r.kind, "cargo")

    def test_resolve_main_from_worktree_and_override(self):
        main = Path(tempfile.mkdtemp()).resolve()
        wt = Path(tempfile.mkdtemp()).resolve()
        outside = Path(tempfile.mkdtemp()).resolve()

        def git(*a, cwd):
            subprocess.run(["git", *a], cwd=cwd, check=True, capture_output=True)

        git("init", "-q", cwd=main)
        git("-c", "user.name=t", "-c", "user.email=t@t", "commit", "--allow-empty", "-q", "-m", "x", cwd=main)
        wt_dir = wt / "linked"
        git("worktree", "add", "-q", str(wt_dir), "-b", "side", cwd=main)

        got = self.hc.resolve_main(wt_dir, {})
        self.assertIsNotNone(got)
        self.assertEqual(Path(got).resolve(), main)
        got = self.hc.resolve_main(main, {})
        self.assertEqual(Path(got).resolve(), main)

        got = self.hc.resolve_main(wt_dir, {"NUCLEOS_HEAVY_MAIN": str(outside)})
        self.assertEqual(Path(got).resolve(), outside)

        # Not a repository and no override: None, never an exception.
        self.assertIsNone(self.hc.resolve_main(outside, {k: v for k, v in os.environ.items()
                                                         if k != "NUCLEOS_HEAVY_MAIN"} | {"GIT_CEILING_DIRECTORIES": str(outside.parent)}))


if __name__ == "__main__":
    unittest.main()
