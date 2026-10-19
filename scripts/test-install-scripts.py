#!/usr/bin/env python3
"""Cover `scripts/install.sh` and `scripts/install.ps1`: download, verify, install.

The install scripts are the primary way onto macOS and Windows (a download through curl or
Invoke-WebRequest carries no quarantine flag or Mark-of-the-Web, so Gatekeeper and SmartScreen do
not block it), so the one thing they must never do is install bytes that do not match the release's
`SHA256SUMS`. A local `http.server` plays the release; `NUCLEOS_RELEASE_BASE` points the script at
it, `NUCLEOS_INSTALL_DRY_RUN=1` stops it after the verification, and `NUCLEOS_UNAME_S/M` (sh) and
`NUCLEOS_PROCESSOR_ARCHITECTURE` (ps1) stand in for the machine.

**Under a POSIX bash, never the `bash` on PATH on Windows** (there it is WSL). `GIT_BASH`
overrides the search. The PowerShell cases run when `powershell` (Windows) or `pwsh` exists and are
skipped, with a printed warning, when neither does.

Run:  python scripts/test-install-scripts.py
"""

import functools
import hashlib
import http.server
import json
import os
import re
import shutil
import subprocess
import sys
import tarfile
import tempfile
import threading
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
INSTALL_SH = ROOT / "scripts" / "install.sh"
INSTALL_PS1 = ROOT / "scripts" / "install.ps1"
WORKFLOW = ROOT / ".github" / "workflows" / "release.yml"
V = "1.2.3"


def posix_bash() -> str:
    override = os.environ.get("GIT_BASH")
    if override:
        return override
    if os.name != "nt":
        found = shutil.which("bash")
        if found:
            return found
    for cand in (r"C:\Program Files\Git\bin\bash.exe", r"C:\Program Files (x86)\Git\bin\bash.exe"):
        if os.path.exists(cand):
            return cand
    raise SystemExit("no POSIX bash found; set GIT_BASH")


def powershell():
    return shutil.which("powershell") if os.name == "nt" else shutil.which("pwsh")


class Release:
    """A directory served over HTTP that looks like a published release."""

    ASSETS = {
        f"NucleOS_{V}_x64-setup.exe": b"MZ fake windows installer",
        f"NucleOS_{V}_amd64.AppImage": b"#!/bin/sh\necho fake appimage\n",
        f"NucleOS_{V}_amd64.deb": b"fake deb",
        f"NucleOS_{V}_aarch64.dmg": b"fake dmg",
        "NucleOS_aarch64.app.tar.gz": None,  # built below: a real tarball holding NucleOS.app
        "NucleOS_aarch64.app.tar.gz.sig": b"sig",
        "latest.json": json.dumps({"version": V}).encode(),
    }

    def __init__(self, tamper: str = None, drop_sum_for: str = None):
        self.dir = Path(tempfile.mkdtemp(prefix="install-release-"))
        for name, data in self.ASSETS.items():
            if data is None:
                src = self.dir / "src" / "NucleOS.app" / "Contents"
                src.mkdir(parents=True)
                (src / "marker.txt").write_text("app")
                with tarfile.open(self.dir / name, "w:gz") as tf:
                    tf.add(self.dir / "src" / "NucleOS.app", arcname="NucleOS.app")
                shutil.rmtree(self.dir / "src")
            else:
                (self.dir / name).write_bytes(data)
        rows = []
        for p in sorted(self.dir.iterdir(), key=lambda q: q.name):
            if drop_sum_for and p.name.endswith(drop_sum_for):
                continue
            rows.append(f"{hashlib.sha256(p.read_bytes()).hexdigest()}  {p.name}\n")
        (self.dir / "SHA256SUMS").write_text("".join(rows), newline="\n")
        if tamper:  # change the bytes AFTER the checksums were written
            for p in self.dir.iterdir():
                if p.name.endswith(tamper):
                    p.write_bytes(p.read_bytes() + b"tampered")
        handler = functools.partial(_Quiet, directory=str(self.dir))
        self.server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), handler)
        self.url = f"http://127.0.0.1:{self.server.server_address[1]}"
        threading.Thread(target=self.server.serve_forever, daemon=True).start()

    def close(self):
        self.server.shutdown()
        self.server.server_close()
        shutil.rmtree(self.dir, ignore_errors=True)


class _Quiet(http.server.SimpleHTTPRequestHandler):
    def log_message(self, *args):
        pass


def run_sh(release_url, tmp: Path, extra_env=None, args=()):
    env = dict(os.environ)
    env.update(
        {
            "NUCLEOS_RELEASE_BASE": release_url,
            "HOME": str(tmp / "home"),
            "TMPDIR": str(tmp / "tmp"),
            "NUCLEOS_UNAME_S": "Linux",
            "NUCLEOS_UNAME_M": "x86_64",
        }
    )
    env.pop("XDG_DATA_HOME", None)
    env.update(extra_env or {})
    (tmp / "home").mkdir(exist_ok=True)
    (tmp / "tmp").mkdir(exist_ok=True)
    # MSYS translates a TMPDIR given in Windows form on its own; HOME as well.
    return subprocess.run([posix_bash(), str(INSTALL_SH), *args], capture_output=True, text=True, env=env)


def run_ps1(release_url, tmp: Path, extra_env=None):
    env = dict(os.environ)
    env.update(
        {
            "NUCLEOS_RELEASE_BASE": release_url,
            "TEMP": str(tmp / "tmp"),
            "TMP": str(tmp / "tmp"),
            "TMPDIR": str(tmp / "tmp"),
            "NUCLEOS_PROCESSOR_ARCHITECTURE": "AMD64",
        }
    )
    env.update(extra_env or {})
    (tmp / "tmp").mkdir(exist_ok=True)
    cmd = [powershell(), "-NoProfile", "-ExecutionPolicy", "Bypass", "-File", str(INSTALL_PS1)]
    return subprocess.run(cmd, capture_output=True, text=True, env=env)


def empty_dir(p: Path) -> bool:
    return not any(p.iterdir())


class Case:
    def __init__(self, **kw):
        self.release = Release(**kw)
        self.tmp = Path(tempfile.mkdtemp(prefix="install-run-"))

    def __enter__(self):
        return self

    def __exit__(self, *exc):
        self.release.close()
        shutil.rmtree(self.tmp, ignore_errors=True)


def a_matching_checksum_is_verified():
    with Case() as c:
        res = run_sh(c.release.url, c.tmp, {"NUCLEOS_INSTALL_DRY_RUN": "1"})
        assert res.returncode == 0, res.stderr
        assert "verified" in res.stdout, res.stdout
        assert empty_dir(c.tmp / "home"), "a dry run installed something"
        assert empty_dir(c.tmp / "tmp"), "a dry run left a download behind"
        res = run_sh(
            c.release.url, c.tmp, {"NUCLEOS_INSTALL_DRY_RUN": "1", "NUCLEOS_UNAME_S": "Darwin", "NUCLEOS_UNAME_M": "arm64"}
        )
        assert res.returncode == 0 and "NucleOS_aarch64.app.tar.gz" in res.stdout, res.stdout + res.stderr


def a_tampered_asset_is_refused_and_removed():
    with Case(tamper=".AppImage") as c:
        res = run_sh(c.release.url, c.tmp)
        assert res.returncode != 0, "a tampered asset was accepted"
        assert "checksum mismatch" in res.stderr, res.stderr
        assert empty_dir(c.tmp / "home"), "a tampered asset was installed"
        assert empty_dir(c.tmp / "tmp"), "a tampered download was left behind"


def a_missing_checksum_entry_is_refused():
    with Case(drop_sum_for=".AppImage") as c:
        res = run_sh(c.release.url, c.tmp)
        assert res.returncode != 0 and "SHA256SUMS" in res.stderr, res.stderr
        assert empty_dir(c.tmp / "home")


def an_unsupported_platform_is_named():
    with Case() as c:
        for s, m in (("FreeBSD", "amd64"), ("Darwin", "x86_64"), ("Linux", "aarch64")):
            res = run_sh(c.release.url, c.tmp, {"NUCLEOS_UNAME_S": s, "NUCLEOS_UNAME_M": m, "NUCLEOS_INSTALL_DRY_RUN": "1"})
            assert res.returncode == 2, f"{s}/{m}: exit {res.returncode}"
            assert s in res.stderr and m in res.stderr, res.stderr


def a_linux_install_places_the_appimage_and_a_desktop_entry():
    with Case() as c:
        res = run_sh(c.release.url, c.tmp)
        assert res.returncode == 0, res.stderr
        app = c.tmp / "home" / ".local" / "bin" / "NucleOS.AppImage"
        assert app.read_bytes() == Release.ASSETS[f"NucleOS_{V}_amd64.AppImage"]
        if os.name != "nt":
            assert os.access(app, os.X_OK), "the AppImage is not executable"
        desktop = c.tmp / "home" / ".local" / "share" / "applications" / "nucleos.desktop"
        text = desktop.read_text()
        assert "Name=NucleOS" in text and "NucleOS.AppImage" in text, text
        assert empty_dir(c.tmp / "tmp"), "the work directory was not cleaned"


def a_macos_install_extracts_the_app_over_an_old_one():
    with Case() as c:
        apps = c.tmp / "Applications"
        (apps / "NucleOS.app").mkdir(parents=True)
        (apps / "NucleOS.app" / "stale.txt").write_text("old")
        env = {"NUCLEOS_UNAME_S": "Darwin", "NUCLEOS_UNAME_M": "arm64", "NUCLEOS_APPLICATIONS_DIR": str(apps)}
        res = run_sh(c.release.url, c.tmp, env)
        assert res.returncode == 0, res.stderr
        assert (apps / "NucleOS.app" / "Contents" / "marker.txt").exists()
        assert not (apps / "NucleOS.app" / "stale.txt").exists(), "the old app was merged, not replaced"


def a_version_argument_selects_the_release_directory():
    with Case() as c:
        # With no NUCLEOS_RELEASE_BASE the script builds the GitHub URL; here we only check it is
        # the tagged one, through the dry-run line it prints.
        res = run_sh("", c.tmp, {"NUCLEOS_RELEASE_BASE": "", "NUCLEOS_INSTALL_DRY_RUN": "1", "NUCLEOS_PRINT_BASE": "1"}, args=("v" + V,))
        assert "releases/download/v" + V in res.stdout, res.stdout + res.stderr
        res = run_sh("", c.tmp, {"NUCLEOS_RELEASE_BASE": "", "NUCLEOS_INSTALL_DRY_RUN": "1", "NUCLEOS_PRINT_BASE": "1"})
        assert "releases/latest/download" in res.stdout, res.stdout + res.stderr


def ps1_verifies_a_matching_checksum():
    with Case() as c:
        res = run_ps1(c.release.url, c.tmp, {"NUCLEOS_INSTALL_DRY_RUN": "1"})
        assert res.returncode == 0, res.stdout + res.stderr
        assert "verified" in res.stdout, res.stdout
        assert empty_dir(c.tmp / "tmp"), "a dry run left a download behind"


def ps1_refuses_a_tampered_asset_and_removes_it():
    with Case(tamper="-setup.exe") as c:
        res = run_ps1(c.release.url, c.tmp)
        assert res.returncode != 0, "a tampered installer was accepted or run"
        assert "checksum mismatch" in (res.stdout + res.stderr), res.stdout + res.stderr
        assert empty_dir(c.tmp / "tmp"), "a tampered download was left behind"


def ps1_names_an_unsupported_architecture():
    with Case() as c:
        res = run_ps1(c.release.url, c.tmp, {"NUCLEOS_PROCESSOR_ARCHITECTURE": "x86", "NUCLEOS_INSTALL_DRY_RUN": "1"})
        assert res.returncode == 2, res.returncode
        assert "x86" in (res.stdout + res.stderr)


def the_scripts_look_for_the_suffixes_of_the_manifest_table():
    table = json.loads(
        subprocess.run([sys.executable, str(ROOT / "scripts" / "release-manifest.py"), "table"], capture_output=True, text=True, check=True).stdout
    )["assets"]
    sh = INSTALL_SH.read_text(encoding="utf-8")
    ps1 = INSTALL_PS1.read_text(encoding="utf-8")
    assert table["windows-x64"][0][0] in ps1, "install.ps1 does not look for the Windows suffix"
    assert table["linux-x64"][0][0] in sh and table["macos-arm64"][0][0] in sh, "install.sh and the table disagree"


def the_draft_carries_the_install_scripts():
    text = WORKFLOW.read_text(encoding="utf-8")
    publish = text[text.index("\n  publish:") :]
    assert "scripts/install.sh" in publish and "scripts/install.ps1" in publish, "publish does not attach the install scripts"
    assert publish.index("scripts/install.sh") < publish.index("--draft=false"), "the scripts are attached after publishing"
    assert re.search(r"\*\.ps1 text eol=crlf", (ROOT / ".gitattributes").read_text(encoding="utf-8")), ".gitattributes lacks the ps1 rule"


CASES = [
    a_matching_checksum_is_verified,
    a_tampered_asset_is_refused_and_removed,
    a_missing_checksum_entry_is_refused,
    an_unsupported_platform_is_named,
    a_linux_install_places_the_appimage_and_a_desktop_entry,
    a_macos_install_extracts_the_app_over_an_old_one,
    a_version_argument_selects_the_release_directory,
    the_scripts_look_for_the_suffixes_of_the_manifest_table,
    the_draft_carries_the_install_scripts,
]
PS_CASES = [ps1_verifies_a_matching_checksum, ps1_refuses_a_tampered_asset_and_removes_it, ps1_names_an_unsupported_architecture]

if __name__ == "__main__":
    cases = list(CASES)
    if powershell():
        cases += PS_CASES
    else:
        print("WARN no powershell/pwsh on this machine: the install.ps1 cases are skipped")
    failed = 0
    for case in cases:
        try:
            case()
            print(f"ok   {case.__name__}")
        except Exception as exc:  # noqa: BLE001 - report every failing case, not just the first
            failed += 1
            print(f"FAIL {case.__name__}: {type(exc).__name__}: {exc}")
    sys.exit(1 if failed else 0)
