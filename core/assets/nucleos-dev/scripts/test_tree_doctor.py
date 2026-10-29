"""Tests for tree_doctor.py: the `doctor` verb (plan F1-DOCTOR). Fakes only: no network, no daemon."""
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import tree_doctor  # noqa: E402

PROJECT = {"id": "p1", "state_dir": "/s", "bundle_dir": "/b"}


def make_deps(**over):
    """All-green fakes; a test overrides one dependency to break one check."""
    deps = {
        "load_project": lambda root: dict(PROJECT),
        "dir_exists": lambda path: True,
        "import_module": lambda name: object(),
        "run_command": lambda argv, env: (0, '[{"name":"node_tree"}]', ""),
        "token": lambda: "tok",
        "http_get": lambda url, headers: (200, '{"ok":true}'),
        "registry": lambda: ["p1", "other"],
    }
    deps.update(over)
    return deps


def by_name(report):
    return {c["name"]: c for c in report["checks"]}


def raiser(exc):
    def fn(*_a, **_k):
        raise exc
    return fn


class DoctorTests(unittest.TestCase):
    def test_all_green(self):
        report = tree_doctor.run_checks(Path("/r"), **make_deps())
        self.assertEqual(report["verb"], "doctor")
        self.assertTrue(report["ok"])
        self.assertEqual(
            set(by_name(report)), {"project", "tree_yaml", "mcp_tools", "daemon_health", "registry"}
        )
        for check in report["checks"]:
            self.assertTrue(check["ok"])

    def test_failure_has_one_cause_line_and_repair(self):
        report = tree_doctor.run_checks(
            Path("/r"), **make_deps(dir_exists=lambda p: p != "/b")
        )
        self.assertFalse(report["ok"])
        check = by_name(report)["project"]
        self.assertFalse(check["ok"])
        self.assertIn("/b", check["cause"])
        self.assertNotIn("\n", check["cause"])
        self.assertTrue(check["repair"])

    def test_project_json_missing(self):
        report = tree_doctor.run_checks(
            Path("/r"), **make_deps(load_project=raiser(RuntimeError("project.json not found")))
        )
        check = by_name(report)["project"]
        self.assertFalse(check["ok"])
        self.assertIn("project.json", check["cause"])

    def test_tree_yaml_import_fails(self):
        report = tree_doctor.run_checks(
            Path("/r"), **make_deps(import_module=raiser(ImportError("no yaml")))
        )
        self.assertFalse(by_name(report)["tree_yaml"]["ok"])
        self.assertTrue(by_name(report)["tree_yaml"]["repair"])

    def test_import_checks_tree_yaml_by_name(self):
        seen = []
        tree_doctor.run_checks(Path("/r"), **make_deps(import_module=lambda n: seen.append(n)))
        self.assertIn("tree_yaml", seen)

    def test_mcp_tools_receives_token_and_flag(self):
        calls = []

        def run(argv, env):
            calls.append((argv, env))
            return 0, "[]", ""

        tree_doctor.run_checks(Path("/r"), **make_deps(run_command=run))
        argv, env = calls[0]
        self.assertIn("--mcp-tools", argv)
        self.assertIn("tok", " ".join(argv) + " " + " ".join(map(str, env.values())))

    def test_mcp_tools_nonzero_exit(self):
        report = tree_doctor.run_checks(
            Path("/r"), **make_deps(run_command=lambda a, e: (1, "", "boom\nmore"))
        )
        check = by_name(report)["mcp_tools"]
        self.assertFalse(check["ok"])
        self.assertNotIn("\n", check["cause"])

    def test_missing_token(self):
        report = tree_doctor.run_checks(Path("/r"), **make_deps(token=lambda: None))
        self.assertFalse(by_name(report)["mcp_tools"]["ok"])

    def test_health_unreachable_and_bad_status(self):
        report = tree_doctor.run_checks(
            Path("/r"), **make_deps(http_get=raiser(OSError("refused")))
        )
        self.assertFalse(by_name(report)["daemon_health"]["ok"])
        report = tree_doctor.run_checks(
            Path("/r"), **make_deps(http_get=lambda u, h: (503, ""))
        )
        self.assertFalse(by_name(report)["daemon_health"]["ok"])

    def test_health_url_ends_with_health(self):
        urls = []
        tree_doctor.run_checks(
            Path("/r"), **make_deps(http_get=lambda u, h: (urls.append(u), (200, "{}"))[1])
        )
        self.assertTrue(urls[0].endswith("/health"))

    def test_project_not_in_registry(self):
        report = tree_doctor.run_checks(Path("/r"), **make_deps(registry=lambda: ["other"]))
        check = by_name(report)["registry"]
        self.assertFalse(check["ok"])
        self.assertIn("no_project", check["cause"])
        self.assertTrue(check["repair"])

    def test_one_failure_does_not_stop_others(self):
        report = tree_doctor.run_checks(
            Path("/r"),
            **make_deps(import_module=raiser(ImportError("x")), registry=lambda: []),
        )
        self.assertEqual(len(report["checks"]), 5)
        self.assertEqual([c["ok"] for c in report["checks"]].count(False), 2)

    def test_register_adds_doctor_verb(self):
        verbs = {}
        tree_doctor.register(verbs)
        configure, run = verbs["doctor"]
        self.assertTrue(callable(configure) and callable(run))


if __name__ == "__main__":
    unittest.main()
