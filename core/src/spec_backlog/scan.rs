//! stub: spec file scan

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spec_backlog::model::{
        spec_id, CriterionKind, CriterionStatus, NewCriterion, Spec, SpecKind, SpecStatus,
    };
    use crate::spec_backlog::store::{get_spec, list_criteria, replace_criteria};
    use sha2::{Digest, Sha256};
    use sqlx::SqlitePool;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::time::SystemTime;

    const PROJECT: Option<&str> = Some("proj-1");

    fn sha_hex(bytes: &[u8]) -> String {
        Sha256::digest(bytes)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    }

    fn write_file(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
        fs::create_dir_all(dir).unwrap();
        let file = dir.join(name);
        fs::write(&file, bytes).unwrap();
        file
    }

    fn project_specs(root: &Path) -> PathBuf {
        root.join(".ai").join("specs")
    }

    fn state_specs(state: &Path) -> PathBuf {
        state.join("specs")
    }

    fn roots_at(project: &Path, state: &Path) -> ScanRoots {
        ScanRoots {
            project_root: Some(project.to_path_buf()),
            state_dir: Some(state.to_path_buf()),
        }
    }

    fn by_path<'a>(report: &'a ScanReport, path: &str) -> &'a Spec {
        report
            .specs
            .iter()
            .find(|spec| spec.path == path)
            .unwrap_or_else(|| panic!("{path} is not in the report"))
    }

    async fn stored(pool: &SqlitePool, id: &str) -> Spec {
        get_spec(pool, id).await.unwrap().expect("spec row")
    }

    async fn mark_extracted(pool: &SqlitePool, id: &str) {
        sqlx::query("UPDATE specs SET status = 'extracted' WHERE id = ?")
            .bind(id.to_owned())
            .execute(pool)
            .await
            .unwrap();
    }

    fn open_items(texts: &[&str]) -> Vec<NewCriterion> {
        texts
            .iter()
            .map(|text| NewCriterion {
                text: (*text).to_owned(),
                kind: CriterionKind::Behavior,
                verify_hint: None,
                status: CriterionStatus::Open,
            })
            .collect()
    }

    /// Sets the file read-only and returns the permissions to restore, so the tempdir can drop.
    fn set_read_only(file: &Path) -> fs::Permissions {
        let original = fs::metadata(file).unwrap().permissions();
        let mut read_only = original.clone();
        read_only.set_readonly(true);
        fs::set_permissions(file, read_only).unwrap();
        assert!(fs::metadata(file).unwrap().permissions().readonly());
        original
    }

    #[derive(Debug, PartialEq)]
    struct Snapshot {
        bytes: Vec<u8>,
        modified: SystemTime,
        read_only: bool,
        listing: Vec<String>,
    }

    fn snapshot(file: &Path) -> Snapshot {
        let metadata = fs::metadata(file).unwrap();
        let mut listing: Vec<String> = fs::read_dir(file.parent().unwrap())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        listing.sort();
        Snapshot {
            bytes: fs::read(file).unwrap(),
            modified: metadata.modified().unwrap(),
            read_only: metadata.permissions().readonly(),
            listing,
        }
    }

    #[tokio::test]
    async fn scan_registers_md_files() {
        let pool = crate::testdb::fresh_pool().await;
        let project = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        // The `## ` line ahead of the title must not be taken for it.
        let alpha: &[u8] = b"Intro line\n## Not the title\n# Alpha title\nbody\n";
        let beta: &[u8] = b"No heading in this one\n";
        write_file(&project_specs(project.path()), "2026-01-01-alpha.md", alpha);
        write_file(&state_specs(state.path()), "2026-01-02-beta.md", beta);

        let report = scan(&pool, PROJECT, &roots_at(project.path(), state.path()))
            .await
            .unwrap();

        assert_eq!(report.new, 2);
        assert_eq!(report.changed, 0);
        assert_eq!(report.unchanged, 0);
        assert_eq!(report.specs.len(), 2);

        let alpha_path = ".ai/specs/2026-01-01-alpha.md";
        let alpha_spec = by_path(&report, alpha_path);
        assert_eq!(alpha_spec.id, spec_id(PROJECT, alpha_path));
        assert_eq!(alpha_spec.project_id.as_deref(), PROJECT);
        assert_eq!(alpha_spec.title, "Alpha title");
        assert_eq!(alpha_spec.content_sha, sha_hex(alpha));
        assert_eq!(alpha_spec.kind, SpecKind::Unknown);
        assert_eq!(alpha_spec.status, SpecStatus::Discovered);

        let beta_path = "<state_dir>/specs/2026-01-02-beta.md";
        let beta_spec = by_path(&report, beta_path);
        assert_eq!(beta_spec.id, spec_id(PROJECT, beta_path));
        assert_eq!(beta_spec.title, "2026-01-02-beta");
        assert_eq!(beta_spec.content_sha, sha_hex(beta));
        assert_eq!(beta_spec.kind, SpecKind::Unknown);

        // The rows are really in the registry, not only in the report.
        assert_eq!(
            stored(&pool, &alpha_spec.id).await.content_sha,
            sha_hex(alpha)
        );
        assert_eq!(stored(&pool, &beta_spec.id).await.path, beta_path);

        assert_eq!(content_sha(alpha), sha_hex(alpha));
        assert_eq!(
            content_sha(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    // The owner's rule: a spec file is never altered, by anything in the backlog.
    #[tokio::test]
    async fn scan_never_opens_a_file_for_writing() {
        let pool = crate::testdb::fresh_pool().await;
        let project = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        let alpha: &[u8] = b"# Alpha\nbody\n";
        let beta: &[u8] = b"# Beta\nbody\n";
        let alpha_file = write_file(&project_specs(project.path()), "alpha.md", alpha);
        let beta_file = write_file(&state_specs(state.path()), "beta.md", beta);
        let alpha_permissions = set_read_only(&alpha_file);
        let beta_permissions = set_read_only(&beta_file);
        let alpha_before = snapshot(&alpha_file);
        let beta_before = snapshot(&beta_file);
        let roots = roots_at(project.path(), state.path());

        let first = scan(&pool, PROJECT, &roots).await.unwrap();
        let second = scan(&pool, PROJECT, &roots).await.unwrap();

        assert_eq!(first.new, 2);
        assert_eq!(second.unchanged, 2);
        let alpha_id = spec_id(PROJECT, ".ai/specs/alpha.md");
        let beta_id = spec_id(PROJECT, "<state_dir>/specs/beta.md");
        assert_eq!(stored(&pool, &alpha_id).await.content_sha, sha_hex(alpha));
        assert_eq!(stored(&pool, &beta_id).await.content_sha, sha_hex(beta));
        assert_eq!(snapshot(&alpha_file), alpha_before);
        assert_eq!(snapshot(&beta_file), beta_before);

        fs::set_permissions(&alpha_file, alpha_permissions).unwrap();
        fs::set_permissions(&beta_file, beta_permissions).unwrap();

        // The cut at the test module is what keeps these needles from matching themselves.
        let source = include_str!("scan.rs");
        let production = source.split("#[cfg(test)]").next().unwrap();
        for needle in [
            "OpenOptions",
            "File::create",
            "fs::write",
            "create_dir",
            "remove_file",
            "rename(",
            "set_permissions",
        ] {
            assert!(
                !production.contains(needle),
                "the scan's production code contains {needle}"
            );
        }
        // `fs::read(` and not `fs::read`, which `fs::read_dir` would satisfy on its own.
        assert!(
            production.contains("fs::read("),
            "the scan's production code never reads a file"
        );
    }

    #[tokio::test]
    async fn scan_ignores_non_markdown() {
        let pool = crate::testdb::fresh_pool().await;
        let project = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        for specs in [project_specs(project.path()), state_specs(state.path())] {
            write_file(&specs, "notes.txt", b"# Text\n");
            write_file(&specs, "old.md.bak", b"# Backup\n");
            write_file(&specs.join("sub"), "nested.md", b"# Nested\n");
            write_file(&specs.join("x.md"), "inner.md", b"# Inside a directory\n");
        }
        write_file(&project_specs(project.path()), "real.md", b"# Real\n");

        let report = scan(&pool, PROJECT, &roots_at(project.path(), state.path()))
            .await
            .unwrap();

        let paths: Vec<&str> = report.specs.iter().map(|spec| spec.path.as_str()).collect();
        assert_eq!(paths, vec![".ai/specs/real.md"]);
        assert_eq!(report.new, 1);
        let rows = sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM specs")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(rows, 1);
    }

    #[tokio::test]
    async fn rescan_with_new_sha_marks_stale() {
        let pool = crate::testdb::fresh_pool().await;
        let project = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        let version_1: &[u8] = b"# Alpha\nfirst\n";
        let version_2: &[u8] = b"# Alpha\nsecond\n";
        let file = write_file(&project_specs(project.path()), "alpha.md", version_1);
        let roots = roots_at(project.path(), state.path());
        scan(&pool, PROJECT, &roots).await.unwrap();
        let id = spec_id(PROJECT, ".ai/specs/alpha.md");
        replace_criteria(&pool, &id, &sha_hex(version_1), &open_items(&["one", "two"]))
            .await
            .unwrap();
        mark_extracted(&pool, &id).await;

        fs::write(&file, version_2).unwrap();
        let report = scan(&pool, PROJECT, &roots).await.unwrap();

        assert_eq!(report.changed, 1);
        assert_eq!(report.new, 0);
        assert_eq!(report.unchanged, 0);
        let spec = stored(&pool, &id).await;
        assert_eq!(spec.status, SpecStatus::Discovered);
        assert_eq!(spec.content_sha, sha_hex(version_2));
        let criteria = list_criteria(&pool, &id).await.unwrap();
        assert_eq!(criteria.len(), 2);
        assert!(criteria
            .iter()
            .all(|criterion| criterion.status == CriterionStatus::Stale));
    }

    #[tokio::test]
    async fn rescan_unchanged_keeps_status() {
        let pool = crate::testdb::fresh_pool().await;
        let project = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        let bytes: &[u8] = b"# Alpha\nbody\n";
        write_file(&project_specs(project.path()), "alpha.md", bytes);
        let roots = roots_at(project.path(), state.path());
        scan(&pool, PROJECT, &roots).await.unwrap();
        let id = spec_id(PROJECT, ".ai/specs/alpha.md");
        replace_criteria(&pool, &id, &sha_hex(bytes), &open_items(&["one"]))
            .await
            .unwrap();
        mark_extracted(&pool, &id).await;
        let criteria_before = list_criteria(&pool, &id).await.unwrap();

        let report = scan(&pool, PROJECT, &roots).await.unwrap();

        assert_eq!(report.unchanged, 1);
        assert_eq!(report.new, 0);
        assert_eq!(report.changed, 0);
        assert_eq!(stored(&pool, &id).await.status, SpecStatus::Extracted);
        assert_eq!(list_criteria(&pool, &id).await.unwrap(), criteria_before);
    }

    #[tokio::test]
    async fn missing_directories_are_an_empty_scan() {
        let pool = crate::testdb::fresh_pool().await;
        let project = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        // `.ai` exists without `specs` under it, and the state dir has no `specs` at all.
        fs::create_dir_all(project.path().join(".ai")).unwrap();

        let cases = [
            ScanRoots {
                project_root: None,
                state_dir: None,
            },
            roots_at(project.path(), state.path()),
            roots_at(&project.path().join("nope"), &state.path().join("nope")),
        ];
        for roots in &cases {
            let report = scan(&pool, PROJECT, roots).await.unwrap();
            assert!(report.specs.is_empty());
            assert_eq!(report.new, 0);
            assert_eq!(report.changed, 0);
            assert_eq!(report.unchanged, 0);
        }
    }

    #[tokio::test]
    async fn non_utf8_file_is_registered() {
        let pool = crate::testdb::fresh_pool().await;
        let project = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        let bytes: &[u8] = &[0xff, 0xfe, 0x00, 0x80, b'\n'];
        assert!(std::str::from_utf8(bytes).is_err());
        write_file(&project_specs(project.path()), "binary-ish.md", bytes);

        let report = scan(&pool, PROJECT, &roots_at(project.path(), state.path()))
            .await
            .unwrap();

        assert_eq!(report.new, 1);
        let spec = by_path(&report, ".ai/specs/binary-ish.md");
        assert_eq!(spec.content_sha, sha_hex(bytes));
        assert_eq!(spec.title, "binary-ish");
    }

    #[test]
    fn resolve_stored_path_maps_both_roots() {
        let project_path = ".ai/specs/x.md";
        let state_path = "<state_dir>/specs/x.md";
        let project_file = Path::new("proj").join(".ai").join("specs").join("x.md");
        let state_file = Path::new("state").join("specs").join("x.md");

        let both = roots_at(Path::new("proj"), Path::new("state"));
        assert_eq!(resolve_stored_path(&both, project_path), Some(project_file.clone()));
        assert_eq!(resolve_stored_path(&both, state_path), Some(state_file.clone()));

        let no_project = ScanRoots {
            project_root: None,
            state_dir: Some(PathBuf::from("state")),
        };
        assert_eq!(resolve_stored_path(&no_project, project_path), None);
        assert_eq!(resolve_stored_path(&no_project, state_path), Some(state_file));

        let no_state = ScanRoots {
            project_root: Some(PathBuf::from("proj")),
            state_dir: None,
        };
        assert_eq!(resolve_stored_path(&no_state, state_path), None);
        assert_eq!(resolve_stored_path(&no_state, project_path), Some(project_file));
    }
}
