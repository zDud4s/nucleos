"""Tests: the broker must not inject CARGO_TARGET_DIR where it overrides a target-dir the tree
already chose (src-tauri's own .cargo/config.toml). Everything else that is eligible, daemon-owned
run/*, job/*, integration-* branches included, gets a POOL dir
(`.cargo-target-pool-<k>`), never a per-branch one.

Asked black-box, through the broker: the eligibility rules are pinned, not the name of the
function that implements them."""

import importlib.util
import unittest

from pathlib import Path

HERE = Path(__file__).parent
_spec = importlib.util.spec_from_file_location("test_heavy_targetdir", HERE / "test_heavy_targetdir.py")
base = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(base)
HEAVY = base.HEAVY


class ScopeTests(base.TargetDirTests):
    """Reuses the parent's hermetic setUp/helpers; its own tests are blanked below."""

    def run_on(self, cargo_args=None, branch=None, cwd=None):
        if branch:
            base.git(self.repo, "checkout", "-q", "-B", branch)
        return self.run_broker(cargo_args, cwd=cwd)

    def test_manifest_path_into_src_tauri_gets_no_injection(self):
        rc, seen, _ = self.run_on(["test", "--manifest-path", "shell/src-tauri/Cargo.toml"])
        self.assertEqual(rc, 0)
        self.assertTrue(seen.get("argv"), "the child never ran")
        self.assertFalse(seen.get("td"), f"CARGO_TARGET_DIR leaked into the child: {seen}")

    def test_crate_with_own_target_dir_config_gets_no_injection(self):
        crate = self.repo / "tauri-like"
        (crate / ".cargo").mkdir(parents=True)
        (crate / ".cargo" / "config.toml").write_text('[build]\ntarget-dir = "target"\n')
        rc, seen, _ = self.run_broker(cwd=crate)
        self.assertEqual(rc, 0)
        self.assertFalse(seen.get("td"), f"CARGO_TARGET_DIR leaked into the child: {seen}")

    def test_daemon_branches_are_pooled(self):
        for branch in ("run/123", "job/abc", "integration-merge-1"):
            with self.subTest(branch=branch):
                rc, seen, _ = self.run_on(["test"], branch=branch)
                self.assertEqual(rc, 0)
                self.assertTrue(seen.get("argv"), "the child never ran")
                self.assert_pool(seen)
                slug = branch.replace("/", "-")
                self.assertFalse((self.td_root / f".cargo-target-{slug}").exists(),
                                 f"{branch} must not get a per-branch dir")

    def test_ordinary_branch_still_injected(self):
        rc, seen, _ = self.run_on(["test"], branch="feat/x")
        self.assertEqual(rc, 0)
        self.assert_pool(seen)
        self.assertFalse((self.td_root / ".cargo-target-feat-x").exists())


# The parent's own tests must not run again under this module.
for _n in [n for n in dir(base.TargetDirTests) if n.startswith("test_")]:
    setattr(ScopeTests, _n, None)

if __name__ == "__main__":
    unittest.main()
