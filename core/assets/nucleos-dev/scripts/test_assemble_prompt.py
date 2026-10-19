#!/usr/bin/env python3
"""Tests for assemble_prompt.py against a throwaway project laid out like a materialized bundle.

    python test_assemble_prompt.py -v
"""

from __future__ import annotations

import json
import os
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
BUNDLE = HERE.parent
SCRIPT = HERE / "assemble_prompt.py"

MEMORY = """# Memory

## Topic vocabulary

```
workflow dispatch
```

```
old-tag -> workflow
```

## Entries
- 2026-01-01 [workflow] first entry
- 2026-01-02 [workflow] second entry
- 2026-01-03 [dispatch] third entry
"""


class AssemblePrompt(unittest.TestCase):
    def setUp(self):
        self._tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self._tmp.cleanup)
        tmp = Path(self._tmp.name)
        self.root, self.state = tmp / "project", tmp / "state"
        wf = self.root / ".nucleos" / "workflow"
        wf.mkdir(parents=True)
        self.state.mkdir()
        shutil.copy(BUNDLE / ".nucleos" / "workflow" / "prompt-contract.yaml", wf)
        shutil.copytree(BUNDLE / ".nucleos" / "workflow" / "packets", wf / "packets")
        for skill in ("executor", "planner"):
            shutil.copytree(BUNDLE / ".claude" / "skills" / skill, self.root / ".claude" / "skills" / skill)
        (wf / "project.json").write_text(
            json.dumps({"id": "t", "state_dir": str(self.state), "bundle_dir": str(BUNDLE)}), encoding="utf-8")
        (self.state / "memory.md").write_text(MEMORY, encoding="utf-8")
        (self.state / "project.yaml").write_text("project_name: demo\n", encoding="utf-8")
        self.objective = tmp / "objective.md"
        self.objective.write_text("Do the thing.\n", encoding="utf-8")

    def run_script(self, *extra, phase="execute", slug="t1"):
        env = {**os.environ, "NUCLEOS_PROJECT_ROOT": str(self.root)}
        return subprocess.run(
            [sys.executable, str(SCRIPT), "--phase", phase, "--slug", slug, "--objective", str(self.objective),
             *extra], capture_output=True, text=True, env=env)

    def test_execute_archive_and_manifest_land_in_the_state_dir(self):
        r = self.run_script("--packet", ".nucleos/workflow/packets/execute.md", "--memory-tags", "workflow")
        self.assertEqual(r.returncode, 0, r.stderr)
        archive = self.state / "prompts" / "t1-execute-1.md"
        self.assertTrue(archive.is_file())
        manifest = json.loads(archive.with_suffix(".manifest.json").read_text(encoding="utf-8"))
        self.assertEqual(manifest["memory_lines"], 2)
        self.assertEqual(manifest["memory_tags"], ["workflow"])
        self.assertIn("memory_slice", manifest["prompt_sections"])
        self.assertIn("mcp__nucleos__record", r.stdout)
        self.assertNotIn("." + "ai/", r.stdout)

    def test_a_tag_outside_the_vocabulary_stops(self):
        r = self.run_script("--packet", ".nucleos/workflow/packets/execute.md", "--memory-tags", "nope")
        self.assertEqual(r.returncode, 2)
        self.assertIn("unknown memory tag", r.stderr)
        self.assertFalse((self.state / "prompts").exists())

    def test_a_legacy_tag_is_accepted(self):
        r = self.run_script("--packet", ".nucleos/workflow/packets/execute.md", "--memory-tags", "old-tag")
        self.assertEqual(r.returncode, 2)
        self.assertIn("slice empty", r.stderr)  # known to the vocabulary, but no entry carries it

    def test_a_section_the_phase_may_not_carry_stops(self):
        r = self.run_script("--packet", ".nucleos/workflow/packets/plan.md", "--memory-full", phase="execute")
        self.assertEqual(r.returncode, 2)
        self.assertIn("not allowed", r.stderr)

    def test_the_memory_index_names_the_state_dir_memory(self):
        r = self.run_script("--packet", ".nucleos/workflow/packets/plan.md", "--memory-index", phase="plan")
        self.assertEqual(r.returncode, 0, r.stderr)
        text = (self.state / "prompts" / "t1-plan-1.md").read_bytes().decode("utf-8")
        self.assertIn("INVENTORY", text)
        self.assertIn((self.state / "memory.md").as_posix(), text)

    def test_an_existing_archive_is_not_overwritten(self):
        args = ("--packet", ".nucleos/workflow/packets/execute.md", "--memory-tags", "workflow")
        self.assertEqual(self.run_script(*args).returncode, 0)
        r = self.run_script(*args)
        self.assertEqual(r.returncode, 2)
        self.assertIn("already exists", r.stderr)

    def test_no_project_json_stops_with_a_clear_message(self):
        (self.root / ".nucleos" / "workflow" / "project.json").unlink()
        r = self.run_script("--packet", ".nucleos/workflow/packets/execute.md", "--memory-tags", "workflow")
        self.assertEqual(r.returncode, 2)
        self.assertIn("project.json", r.stderr)

    def test_bootstrap_runs_before_project_yaml_exists(self):
        # Bootstrap is the phase that writes project.yaml, so a fresh project has none yet.
        shutil.copytree(BUNDLE / ".claude" / "skills" / "bootstrap", self.root / ".claude" / "skills" / "bootstrap")
        (self.state / "project.yaml").unlink()
        r = self.run_script(phase="bootstrap")
        self.assertEqual(r.returncode, 0, r.stderr)
        manifest = json.loads((self.state / "prompts" / "t1-bootstrap-1.manifest.json").read_text(encoding="utf-8"))
        self.assertNotIn("project_yaml", manifest["prompt_sections"])

    def test_orchestrate_node_phase_assembles_with_the_node_packet(self):
        shutil.copytree(BUNDLE / ".claude" / "skills" / "orchestrate-node",
                        self.root / ".claude" / "skills" / "orchestrate-node")
        r = self.run_script("--packet", ".nucleos/workflow/packets/node.md", "--memory-tags", "workflow",
                            phase="orchestrate_node")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn("wf-orchestrator", r.stdout)

    def test_a_missing_project_yaml_still_stops_a_phase_that_reads_it(self):
        (self.state / "project.yaml").unlink()
        r = self.run_script("--packet", ".nucleos/workflow/packets/execute.md", "--memory-tags", "workflow")
        self.assertEqual(r.returncode, 2)
        self.assertIn("project.yaml", r.stderr)


if __name__ == "__main__":
    unittest.main()
