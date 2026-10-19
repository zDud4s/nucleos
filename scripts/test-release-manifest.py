#!/usr/bin/env python3
"""Cover `scripts/release-manifest.py` and the shape of `.github/workflows/release.yml`.

A release is built on three runners that each upload their own installers to one draft. Before the
draft goes out, `release-manifest.py` has to prove that every platform contributed what the install
scripts and the updater expect, and write the `SHA256SUMS` those scripts verify against. These
cases build fake release directories and drive the script through a subprocess; the workflow
checks are structural, because nothing here can run GitHub Actions.

Run:  python scripts/test-release-manifest.py
"""

import hashlib
import json
import re
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
SCRIPT = ROOT / "scripts" / "release-manifest.py"
WORKFLOW = ROOT / ".github" / "workflows" / "release.yml"

V = "1.2.3"
# What tauri-action uploads per platform for a 1.2.3 build (the macOS tarball carries no version).
GOOD = [
    f"NucleOS_{V}_x64-setup.exe",
    f"NucleOS_{V}_x64-setup.exe.sig",
    f"NucleOS_{V}_amd64.AppImage",
    f"NucleOS_{V}_amd64.AppImage.sig",
    f"NucleOS_{V}_amd64.deb",
    f"NucleOS_{V}_aarch64.dmg",
    "NucleOS_aarch64.app.tar.gz",
    "NucleOS_aarch64.app.tar.gz.sig",
    "latest.json",
]


def release_dir(names=GOOD) -> Path:
    tmp = Path(tempfile.mkdtemp(prefix="release-manifest-"))
    for name in names:
        if name == "latest.json":
            (tmp / name).write_text(json.dumps({"version": V, "platforms": {}}))
        else:
            (tmp / name).write_bytes(f"bytes of {name}\n".encode())
    return tmp


def run(*args: str) -> subprocess.CompletedProcess:
    return subprocess.run([sys.executable, str(SCRIPT), *args], capture_output=True, text=True)


def verify_accepts_a_complete_release_and_names_what_is_missing():
    d = release_dir()
    try:
        res = run("verify", "--dir", str(d), "--version", V)
        assert res.returncode == 0, res.stderr
        # A missing signature, a missing platform installer and a duplicate each fail by name.
        (d / f"NucleOS_{V}_x64-setup.exe.sig").unlink()
        res = run("verify", "--dir", str(d), "--version", V)
        assert res.returncode == 1 and "-setup.exe.sig" in res.stderr, res.stderr
        (d / f"NucleOS_{V}_amd64.deb").unlink()
        res = run("verify", "--dir", str(d), "--version", V)
        assert ".deb" in res.stderr, res.stderr
        (d / "NucleOS_x.app.tar.gz").write_bytes(b"x")
        res = run("verify", "--dir", str(d), "--version", V)
        assert res.returncode == 1 and ".app.tar.gz" in res.stderr and "more than one" in res.stderr, res.stderr
    finally:
        shutil.rmtree(d, ignore_errors=True)


def a_foreign_version_in_a_versioned_name_is_refused():
    d = release_dir()
    try:
        (d / f"NucleOS_{V}_amd64.deb").rename(d / "NucleOS_9.9.9_amd64.deb")
        res = run("verify", "--dir", str(d), "--version", V)
        assert res.returncode == 1 and "9.9.9" in res.stderr, res.stderr
        # --platform narrows the proof to the legs that ran (a dry run on one runner).
        res = run("verify", "--dir", str(d), "--version", V, "--platforms", "windows-x64")
        assert res.returncode == 0, res.stderr
        res = run("verify", "--dir", str(d), "--version", V, "--platforms", "plan9")
        assert res.returncode == 2, res.returncode
    finally:
        shutil.rmtree(d, ignore_errors=True)


def sums_writes_sha256sums_for_every_file():
    d = release_dir()
    try:
        res = run("sums", "--dir", str(d), "--version", V)
        assert res.returncode == 0, res.stderr
        lines = (d / "SHA256SUMS").read_bytes().decode().splitlines()
        assert lines == sorted(lines, key=lambda l: l.split("  ", 1)[1]), "not sorted by name"
        got = {}
        for line in lines:
            digest, name = line.split("  ", 1)
            got[name] = digest
        assert set(got) == set(GOOD), set(got) ^ set(GOOD)  # SHA256SUMS never lists itself
        for name, digest in got.items():
            assert digest == hashlib.sha256((d / name).read_bytes()).hexdigest(), name
        assert b"\r" not in (d / "SHA256SUMS").read_bytes(), "must be LF"
        # Re-running replaces the file instead of listing the old one.
        assert run("sums", "--dir", str(d), "--version", V).returncode == 0
        assert "SHA256SUMS" not in (d / "SHA256SUMS").read_text()
    finally:
        shutil.rmtree(d, ignore_errors=True)


def sums_refuses_an_incomplete_release_and_writes_nothing():
    d = release_dir([n for n in GOOD if not n.endswith(".AppImage")])
    try:
        res = run("sums", "--dir", str(d), "--version", V)
        assert res.returncode == 1 and ".AppImage" in res.stderr, res.stderr
        assert not (d / "SHA256SUMS").exists()
    finally:
        shutil.rmtree(d, ignore_errors=True)


def latest_json_must_be_valid_json_when_present():
    d = release_dir()
    try:
        (d / "latest.json").write_text("{not json")
        res = run("verify", "--dir", str(d), "--version", V)
        assert res.returncode == 1 and "latest.json" in res.stderr, res.stderr
        (d / "latest.json").write_text(json.dumps({"version": "9.9.9", "platforms": {}}))
        res = run("verify", "--dir", str(d), "--version", V)
        assert res.returncode == 1 and "9.9.9" in res.stderr, res.stderr
        (d / "latest.json").write_text(json.dumps({"version": V, "platforms": {}}))
        assert run("verify", "--dir", str(d), "--version", V).returncode == 0
    finally:
        shutil.rmtree(d, ignore_errors=True)


def workflow_text() -> str:
    return WORKFLOW.read_text(encoding="utf-8")


def the_workflow_is_tag_only_and_signs_with_known_secrets():
    text = workflow_text()
    on_block = re.search(r"^on:\n((?:[ \t]+.*\n|\n)+)", text, re.M).group(1)
    assert "tags: ['v*']" in on_block and "pull_request" not in on_block and "branches" not in on_block, on_block
    allowed = {
        "GITHUB_TOKEN",
        "TAURI_SIGNING_PRIVATE_KEY",
        "TAURI_SIGNING_PRIVATE_KEY_PASSWORD",
        "APPLE_CERTIFICATE",
        "APPLE_CERTIFICATE_PASSWORD",
        "APPLE_SIGNING_IDENTITY",
    }
    used = set(re.findall(r"secrets\.([A-Z0-9_]+)", text))
    assert used <= allowed, used - allowed
    # The version in the tag, in VERSION and in every manifest is one number, checked before any build.
    assert re.search(r"bump-version\.py --check --expect \"\$\{GITHUB_REF_NAME#v\}\"", text), "no tag/VERSION guard"


def the_draft_is_proven_complete_before_it_is_published():
    text = workflow_text()
    publish = text[text.index("\n  publish:") :]
    order = [publish.find(s) for s in ("release-manifest.py verify", "release-manifest.py sums", "SHA256SUMS", "--draft=false")]
    assert all(i >= 0 for i in order), order
    assert order == sorted(order), f"publish steps are out of order: {order}"
    assert "release-manifest.py" in publish and "actions/checkout" in publish


def macos_is_signed_with_the_stable_certificate_or_not_at_all():
    text = workflow_text()
    assert "APPLE_CERTIFICATE" in text and "APPLE_SIGNING_IDENTITY" in text
    # Fail closed: an empty certificate on macOS is an error, never an ad-hoc signature.
    assert re.search(r"::error::.*APPLE_CERTIFICATE", text), "no fail-closed check for the macOS certificate"
    assert "certificate leaf = H" in text, "the signed app's designated requirement is not verified"


CASES = [
    verify_accepts_a_complete_release_and_names_what_is_missing,
    a_foreign_version_in_a_versioned_name_is_refused,
    sums_writes_sha256sums_for_every_file,
    sums_refuses_an_incomplete_release_and_writes_nothing,
    latest_json_must_be_valid_json_when_present,
    the_workflow_is_tag_only_and_signs_with_known_secrets,
    the_draft_is_proven_complete_before_it_is_published,
    macos_is_signed_with_the_stable_certificate_or_not_at_all,
]

if __name__ == "__main__":
    failed = 0
    for case in CASES:
        try:
            case()
            print(f"ok   {case.__name__}")
        except Exception as exc:  # noqa: BLE001 - report every failing case, not just the first
            failed += 1
            print(f"FAIL {case.__name__}: {type(exc).__name__}: {exc}")
    sys.exit(1 if failed else 0)
