"""`tree.py doctor`: one diagnostic pass over what the orchestration tree depends on.

Five checks (project, tree_yaml, mcp_tools, daemon_health, registry). A failing check carries a
one-line cause and a repair command; a failure never stops the other checks. Every dependency
is injected into `run_checks` so tests use fakes; `run` wires the real stdlib defaults.
"""
from __future__ import annotations

import importlib
import json
import os
import subprocess
import sys
import urllib.error
import urllib.request
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

DEFAULT_DAEMON_URL = "http://127.0.0.1:8791"
CORE = "nucleos-core"


def _line(text):
    """Collapse any text to a single trimmed line."""
    return " ".join(str(text).split())[:300]


def _check(name, ok, cause="", repair=""):
    return {"name": name, "ok": ok, "cause": "" if ok else _line(cause), "repair": "" if ok else repair}


def _check_project(root, load_project, dir_exists):
    try:
        project = load_project(root)
    except Exception as exc:  # noqa: BLE001 - any failure is a diagnosis, not a crash
        return _check("project", False, f"project.json: {exc}",
                      "restart the nucleos daemon so it materializes .nucleos/workflow/project.json"), None
    for key in ("bundle_dir", "state_dir"):
        path = project.get(key)
        if not path or not dir_exists(path):
            return _check("project", False, f"{key} does not exist: {path}",
                          f"run `{CORE} --workflow-sync {root}` to rematerialize the workflow"), project
    return _check("project", True), project


def _check_yaml(import_module):
    try:
        import_module("tree_yaml")
    except Exception as exc:  # noqa: BLE001
        return _check("tree_yaml", False, f"cannot import tree_yaml: {exc}",
                      "reinstall the bundle: run `nucleos-core --workflow-sync <root>`")
    return _check("tree_yaml", True)


def _check_mcp(run_command, token):
    tok = token()
    if not tok:
        return _check("mcp_tools", False, "no daemon token available",
                      f"set NUCLEOS_DAEMON_TOKEN or run `{CORE} --print-token`")
    try:
        code, out, err = run_command([CORE, "--mcp-tools"], {"NUCLEOS_DAEMON_TOKEN": tok})
    except Exception as exc:  # noqa: BLE001
        return _check("mcp_tools", False, f"cannot run {CORE} --mcp-tools: {exc}",
                      "stage the daemon with scripts/run-daemon.ps1")
    if code != 0:
        return _check("mcp_tools", False, f"{CORE} --mcp-tools exited {code}: {err or out}",
                      "start the daemon with scripts/run-daemon.ps1 and retry")
    return _check("mcp_tools", True)


def _check_health(http_get, token, url):
    tok = token()
    headers = {"Authorization": f"Bearer {tok}"} if tok else {}
    try:
        status, _body = http_get(url.rstrip("/") + "/health", headers)
    except Exception as exc:  # noqa: BLE001
        return _check("daemon_health", False, f"daemon unreachable at {url}: {exc}",
                      "start the daemon with scripts/run-daemon.ps1")
    if status != 200:
        return _check("daemon_health", False, f"GET /health returned {status}",
                      "restart the daemon with scripts/run-daemon.ps1")
    return _check("daemon_health", True)


def _check_registry(registry, project):
    pid = (project or {}).get("id")
    if not pid:
        return _check("registry", False, "no_project: project id unknown (project.json unusable)",
                      "fix the project check first")
    try:
        ids = list(registry())
    except Exception as exc:  # noqa: BLE001
        return _check("registry", False, f"no_project: registry unreadable: {exc}",
                      "start the daemon with scripts/run-daemon.ps1")
    if pid not in ids:
        return _check("registry", False, f"no_project: {pid} is not registered with the daemon",
                      f"register it: `{CORE} --workflow-sync {pid}` or add the project in the shell")
    return _check("registry", True)


def run_checks(root, *, load_project, dir_exists, import_module, run_command, token, http_get,
               registry, daemon_url=DEFAULT_DAEMON_URL):
    project_check, project = _check_project(root, load_project, dir_exists)
    checks = [
        project_check,
        _check_yaml(import_module),
        _check_mcp(run_command, token),
        _check_health(http_get, token, daemon_url),
        _check_registry(registry, project),
    ]
    return {"verb": "doctor", "ok": all(c["ok"] for c in checks), "checks": checks}


# --- real defaults -------------------------------------------------------------------------------

def _daemon_url():
    return os.environ.get("NUCLEOS_DAEMON_URL") or DEFAULT_DAEMON_URL


def _real_token():
    tok = os.environ.get("NUCLEOS_DAEMON_TOKEN")
    if tok:
        return tok
    try:
        proc = subprocess.run([CORE, "--print-token"], capture_output=True, text=True, timeout=20)
    except (OSError, subprocess.SubprocessError):
        return None
    out = proc.stdout.strip()
    return out if proc.returncode == 0 and out else None


def _real_run_command(argv, env):
    try:
        proc = subprocess.run(argv, env={**os.environ, **env}, capture_output=True, text=True,
                              timeout=30)
    except subprocess.TimeoutExpired:
        return 1, "", "timed out after 30s"
    return proc.returncode, proc.stdout, proc.stderr


def _real_http_get(url, headers):
    req = urllib.request.Request(url, headers=headers)
    try:
        with urllib.request.urlopen(req, timeout=10) as resp:
            return resp.status, resp.read().decode("utf-8", "replace")
    except urllib.error.HTTPError as exc:
        return exc.code, ""


def _real_registry():
    tok = _real_token()
    headers = {"Authorization": f"Bearer {tok}"} if tok else {}
    status, body = _real_http_get(_daemon_url().rstrip("/") + "/projects", headers)
    if status != 200:
        raise RuntimeError(f"GET /projects returned {status}")
    data = json.loads(body)
    if isinstance(data, dict):
        data = data.get("projects", [])
    return [p.get("id") if isinstance(p, dict) else p for p in data]


def add_arguments(parser):
    """`doctor` takes no arguments."""


def run(args):
    import nucleos_paths

    root = nucleos_paths.project_root()
    return run_checks(
        root,
        load_project=nucleos_paths.load_project,
        dir_exists=lambda p: Path(p).is_dir(),
        import_module=importlib.import_module,
        run_command=_real_run_command,
        token=_real_token,
        http_get=_real_http_get,
        registry=_real_registry,
        daemon_url=_daemon_url(),
    )


def register(verbs):
    verbs["doctor"] = (add_arguments, run)
