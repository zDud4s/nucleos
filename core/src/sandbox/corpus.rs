//! The D12 corpus: every owner-side table caller that touches a file the unit could have written
//! reads or writes it through the crossing, so a link planted in the unit's own tree is a
//! refusal instead of a read or overwrite of something else.
//!
//! Link creation can fail (no privilege, no `mklink`); a test that cannot make its link says so
//! and returns instead of passing silently on nothing.

use std::path::Path;

use super::artifacts::{ARTIFACT_CAP, copy_in, read_in, tampered_scripts, write_in};
use crate::execution::worktree::ARTIFACTS_DIR;

/// A directory junction (Windows) or symlink (unix) at `link` pointing to `target`; `false` when
/// the platform refused, so the caller can skip.
fn try_link_dir(link: &Path, target: &Path) -> bool {
    #[cfg(windows)]
    {
        std::process::Command::new("cmd")
            .arg("/C")
            .arg("mklink")
            .arg("/J")
            .arg(link)
            .arg(target)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|status| status.success())
            .unwrap_or(false)
    }
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(target, link).is_ok()
    }
}

/// A file symlink at `link` pointing to `target`; `false` when the platform refused.
fn try_link_file(link: &Path, target: &Path) -> bool {
    #[cfg(windows)]
    {
        std::os::windows::fs::symlink_file(target, link).is_ok()
    }
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(target, link).is_ok()
    }
}

fn skip(test: &str) {
    eprintln!("SKIPPED {test}: this account cannot create the link the test needs");
}

/// A worktree whose `.nucleos` is a junction to a directory holding `canary` files, or `None`
/// (after saying so) when the link cannot be made.
fn worktree_with_linked_artifacts(
    test: &str,
    canary_files: &[(&str, &str)],
) -> Option<(tempfile::TempDir, tempfile::TempDir)> {
    let worktree = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    for (name, body) in canary_files {
        std::fs::write(outside.path().join(name), body).unwrap();
    }
    if !try_link_dir(&worktree.path().join(ARTIFACTS_DIR), outside.path()) {
        skip(test);
        return None;
    }
    Some((worktree, outside))
}

#[test]
fn plan_json_through_a_junction_to_a_canary_is_refused() {
    let Some((worktree, _outside)) = worktree_with_linked_artifacts(
        "plan_json_through_a_junction_to_a_canary_is_refused",
        &[("plan.json", r#"{"canary":"owner secret"}"#)],
    ) else {
        return;
    };
    let path = worktree.path().join(ARTIFACTS_DIR).join("plan.json");

    let read = read_in(&path, worktree.path(), ARTIFACT_CAP);
    assert!(read.is_err(), "the canary must not be returned: {read:?}");

    let emission = crate::workflows::facts::read_emission(worktree.path(), "plan");
    assert!(
        emission.is_err(),
        "a linked artifacts directory is not 'not emitted' and not a plan: {emission:?}"
    );
}

#[test]
fn archive_plans_never_writes_through_a_link_in_nucleos_dir() {
    // `archive_plans` copies `plan.json` to `plan-N.json` with `copy_in`.
    let Some((worktree, outside)) = worktree_with_linked_artifacts(
        "archive_plans_never_writes_through_a_link_in_nucleos_dir",
        &[("plan.json", "{}")],
    ) else {
        return;
    };
    let directory = worktree.path().join(ARTIFACTS_DIR);
    let copied = copy_in(
        &directory.join("plan.json"),
        &directory.join("plan-1.json"),
        worktree.path(),
    );
    assert!(copied.is_err(), "{copied:?}");
    assert!(
        !outside.path().join("plan-1.json").exists(),
        "the copy landed outside the worktree"
    );
}

#[test]
fn council_md_write_through_a_link_lands_nowhere_outside() {
    let Some((worktree, outside)) = worktree_with_linked_artifacts(
        "council_md_write_through_a_link_lands_nowhere_outside",
        &[],
    ) else {
        return;
    };
    let path = worktree.path().join(ARTIFACTS_DIR).join("council.md");
    let written = write_in(&path, worktree.path(), b"synthesis");
    assert!(written.is_err(), "{written:?}");
    assert!(
        !outside.path().join("council.md").exists(),
        "council.md landed outside the worktree"
    );

    // A planted link as the file itself is refused too, where the account can make one.
    let plain = tempfile::tempdir().unwrap();
    let elsewhere = tempfile::tempdir().unwrap();
    let victim = elsewhere.path().join("victim.txt");
    std::fs::write(&victim, "owner data").unwrap();
    std::fs::create_dir(plain.path().join(ARTIFACTS_DIR)).unwrap();
    let link = plain.path().join(ARTIFACTS_DIR).join("council.md");
    if try_link_file(&link, &victim) {
        let written = write_in(&link, plain.path(), b"synthesis");
        assert!(written.is_err(), "{written:?}");
        assert_eq!(std::fs::read_to_string(&victim).unwrap(), "owner data");
    } else {
        skip("council_md_write_through_a_link_lands_nowhere_outside (file link)");
    }
}

#[test]
fn gate_script_comparison_refuses_a_reparse_point() {
    let project = tempfile::tempdir().unwrap();
    let worktree = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    // Bytes identical to the project root's copy: a byte comparison alone would trust them.
    std::fs::create_dir(project.path().join("tools")).unwrap();
    std::fs::write(project.path().join("tools/run.sh"), "echo ok").unwrap();
    std::fs::write(outside.path().join("run.sh"), "echo ok").unwrap();

    // The honest case first: a plain copy is trusted.
    std::fs::create_dir(worktree.path().join("tools")).unwrap();
    std::fs::write(worktree.path().join("tools/run.sh"), "echo ok").unwrap();
    assert_eq!(
        tampered_scripts(project.path(), worktree.path(), ["sh tools/run.sh"]),
        None
    );
    std::fs::remove_dir_all(worktree.path().join("tools")).unwrap();

    if !try_link_dir(&worktree.path().join("tools"), outside.path()) {
        skip("gate_script_comparison_refuses_a_reparse_point");
        return;
    }
    let refusal = tampered_scripts(project.path(), worktree.path(), ["sh tools/run.sh"]);
    assert!(
        refusal
            .as_deref()
            .is_some_and(|why| why.contains("not trustworthy")),
        "{refusal:?}"
    );
}

#[test]
fn sessions_read_is_capped_and_refuses_links() {
    let dir = tempfile::tempdir().unwrap();
    let transcript = dir.path().join("aaaa-1111.jsonl");
    std::fs::write(&transcript, "{\"type\":\"user\"}\n").unwrap();
    assert!(crate::sessions::transcript_bytes(&transcript).is_some());

    // The cap: a file over it is refused, not read whole.
    let capped = read_in(&transcript, dir.path(), 4);
    assert!(
        matches!(capped, Err(super::CrossingError::TooLarge { .. })),
        "{capped:?}"
    );

    // A link standing in for the transcript is refused.
    let victim_dir = tempfile::tempdir().unwrap();
    let victim = victim_dir.path().join("secret.jsonl");
    std::fs::write(&victim, "{\"type\":\"user\"}\n").unwrap();
    let link = dir.path().join("bbbb-2222.jsonl");
    if try_link_file(&link, &victim) {
        assert!(crate::sessions::transcript_bytes(&link).is_none());
    } else {
        skip("sessions_read_is_capped_and_refuses_links (file link)");
    }
}

#[test]
fn file_document_read_refuses_a_hard_linked_file() {
    let root_dir = tempfile::tempdir().unwrap();
    // The files root is canonical in production; `resolve_within` compares canonical paths.
    let root = std::fs::canonicalize(root_dir.path()).unwrap();
    let elsewhere = tempfile::tempdir().unwrap();
    let secret = elsewhere.path().join("secret.txt");
    std::fs::write(&secret, "owner data").unwrap();
    std::fs::write(root.join("plain.txt"), "fine").unwrap();
    assert_eq!(
        crate::files::read_document(&root, "plain.txt")
            .unwrap()
            .as_bytes(),
        b"fine"
    );

    if std::fs::hard_link(&secret, root.join("planted.txt")).is_err() {
        skip("file_document_read_refuses_a_hard_linked_file");
        return;
    }
    let read = crate::files::read_document(&root, "planted.txt");
    assert!(
        matches!(read, Err(crate::files::PathError::Escapes)),
        "{read:?}"
    );
}

/// The production half of a source file: everything before its first `#[cfg(test)]`.
fn production(source: &str) -> &str {
    source.split("#[cfg(test)]").next().unwrap_or(source)
}

#[test]
fn an_unmigrated_reader_is_a_test_failure() {
    // Files that read or write what the unit could have written must not use `std::fs` directly.
    // The one allowed line is the operator's own copy in the project root.
    let sources = [
        ("workflows/facts.rs", include_str!("../workflows/facts.rs")),
        (
            "execution/job/engine.rs",
            include_str!("../execution/job/engine.rs"),
        ),
        (
            "execution/job/ingest.rs",
            include_str!("../execution/job/ingest.rs"),
        ),
        (
            "execution/job/drive.rs",
            include_str!("../execution/job/drive.rs"),
        ),
        (
            "execution/job/dispatch.rs",
            include_str!("../execution/job/dispatch.rs"),
        ),
        (
            "execution/job/gate.rs",
            include_str!("../execution/job/gate.rs"),
        ),
        ("sessions.rs", include_str!("../sessions.rs")),
    ];
    let forbidden = [
        "fs::read(",
        "fs::read_to_string(",
        "fs::copy(",
        "fs::write(",
        "File::open(",
        "fs::write_atomic(",
    ];
    let mut offenders = Vec::new();
    for (name, source) in sources {
        for (number, line) in production(source).lines().enumerate() {
            let code = line.split("//").next().unwrap_or(line);
            if line.contains("project_root") {
                continue;
            }
            if forbidden.iter().any(|token| code.contains(token)) {
                offenders.push(format!("{name}:{}: {}", number + 1, line.trim()));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "readers that skip the D12 crossing (use sandbox::artifacts):\n{}",
        offenders.join("\n")
    );
}
