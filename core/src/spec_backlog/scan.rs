//! Discovery of spec files. Specs are read-only inputs to the backlog.

use std::fs;
use std::path::PathBuf;

use sha2::{Digest, Sha256};
use sqlx::SqlitePool;

use super::model::{Spec, SpecStatus};
use super::store::{StoreError, upsert_by_path};

/// Start of the reason a scan gives a spec whose file is gone; a spec carrying it is found again
/// when the file comes back, an owner's own abandon is not.
pub const GONE_REASON_PREFIX: &str = "spec file is gone: ";

pub struct ScanRoots {
    pub project_root: Option<PathBuf>,
    pub state_dir: Option<PathBuf>,
}

pub struct ScanReport {
    pub specs: Vec<Spec>,
    pub new: usize,
    pub changed: usize,
    pub unchanged: usize,
    /// `<state_dir>/specs/X` paths whose legacy `.ai/specs/X` row (or one of its `spec-item:`
    /// claims) could not be re-keyed because the new key was already taken.
    pub rekey_conflicts: Vec<String>,
}

#[derive(Debug)]
pub enum ScanError {
    Io(std::io::Error),
    Store(StoreError),
}

impl From<std::io::Error> for ScanError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<StoreError> for ScanError {
    fn from(error: StoreError) -> Self {
        Self::Store(error)
    }
}

impl From<sqlx::Error> for ScanError {
    fn from(error: sqlx::Error) -> Self {
        Self::Store(StoreError::from(error))
    }
}

const LEGACY_SPECS_PREFIX: &str = ".ai/specs/";
const STATE_SPECS_PREFIX: &str = "<state_dir>/specs/";
const LEGACY_CLAIM_PREFIX: &str = "spec-item:.ai/specs/";
const UNRESOLVED_CLAIM_STATUSES: [&str; 3] = ["probing", "open", "claimed"];

fn push_conflict(conflicts: &mut Vec<String>, path: &str) {
    if !conflicts.iter().any(|known| known == path) {
        conflicts.push(path.to_owned());
    }
}

/// Moves every legacy `.ai/specs/X` row of the project to the `<state_dir>/specs/X` id and path,
/// together with its criteria, deps (both sides), questions, extraction runs and `spec-item:`
/// claims, in one transaction. A spec is moved only once its file lives in the state dir and the
/// legacy file is gone. An existing new-path row with no children is replaced; one with children
/// is left alone, as is the legacy row, and the new path is reported as a conflict. Returns the
/// conflicts. A moved row no longer has a legacy path, so running it again moves nothing twice.
async fn rekey_legacy_specs(
    pool: &SqlitePool,
    project_id: Option<&str>,
    roots: &ScanRoots,
) -> Result<Vec<String>, ScanError> {
    let mut conflicts = Vec::new();
    if roots.state_dir.is_none() {
        return Ok(conflicts);
    }
    // Candidates are read through the pool before the transaction opens: the test pool has a
    // single connection, which the transaction then holds.
    let mut candidates = Vec::new();
    for spec in super::store::list_specs_by_project(pool, project_id).await? {
        let Some(name) = spec.path.strip_prefix(LEGACY_SPECS_PREFIX) else {
            continue;
        };
        let new_path = format!("{STATE_SPECS_PREFIX}{name}");
        let state_file_exists = resolve_stored_path(roots, &new_path).is_some_and(|p| p.is_file());
        let legacy_file_exists =
            resolve_stored_path(roots, &spec.path).is_some_and(|p| p.is_file());
        if state_file_exists && !legacy_file_exists {
            candidates.push((spec.id.clone(), name.to_owned(), new_path));
        }
    }
    if candidates.is_empty() {
        return Ok(conflicts);
    }

    let mut tx = pool.begin_with("BEGIN IMMEDIATE").await?;
    for (old_id, name, new_path) in candidates {
        // Re-check under the write lock: an overlapping scan may already have moved this row,
        // and deleting the "taken" new-path row would then delete the moved spec itself.
        let still_legacy: Option<String> =
            sqlx::query_scalar("SELECT id FROM specs WHERE id = ? AND path = ?")
                .bind(&old_id)
                .bind(format!("{LEGACY_SPECS_PREFIX}{name}"))
                .fetch_optional(&mut *tx)
                .await?;
        if still_legacy.is_none() {
            continue;
        }
        let new_id = super::model::spec_id(project_id, &new_path);
        let taken: Option<String> = sqlx::query_scalar("SELECT id FROM specs WHERE id = ?")
            .bind(&new_id)
            .fetch_optional(&mut *tx)
            .await?;
        if taken.is_some() {
            let children: i64 = sqlx::query_scalar(
                "SELECT (SELECT COUNT(*) FROM spec_criteria WHERE spec_id = ?1) \
                 + (SELECT COUNT(*) FROM spec_deps WHERE spec_id = ?1 OR depends_on = ?1) \
                 + (SELECT COUNT(*) FROM spec_questions WHERE spec_id = ?1) \
                 + (SELECT COUNT(*) FROM spec_extraction_runs WHERE spec_id = ?1)",
            )
            .bind(&new_id)
            .fetch_one(&mut *tx)
            .await?;
            if children > 0 {
                tracing::warn!(
                    legacy = %format!("{LEGACY_SPECS_PREFIX}{name}"),
                    new = %new_path,
                    "spec re-key skipped: the state_dir row already has children"
                );
                push_conflict(&mut conflicts, &new_path);
                continue;
            }
            sqlx::query("DELETE FROM specs WHERE id = ?")
                .bind(&new_id)
                .execute(&mut *tx)
                .await?;
        }

        sqlx::query("UPDATE specs SET id = ?, path = ? WHERE id = ?")
            .bind(&new_id)
            .bind(&new_path)
            .bind(&old_id)
            .execute(&mut *tx)
            .await?;
        for statement in [
            "UPDATE spec_criteria SET spec_id = ? WHERE spec_id = ?",
            "UPDATE spec_questions SET spec_id = ? WHERE spec_id = ?",
            "UPDATE spec_extraction_runs SET spec_id = ? WHERE spec_id = ?",
            "UPDATE spec_deps SET spec_id = ? WHERE spec_id = ?",
            "UPDATE spec_deps SET depends_on = ? WHERE depends_on = ?",
        ] {
            sqlx::query(statement)
                .bind(&new_id)
                .bind(&old_id)
                .execute(&mut *tx)
                .await?;
        }

        // Claims carry a NOT NULL project, so a project-less scan has none to move.
        let Some(project) = project_id else {
            continue;
        };
        let claims: Vec<(i64, String, String)> = sqlx::query_as(
            "SELECT id, task_key, status FROM task_claims \
             WHERE project_id = ? AND task_key LIKE 'spec-item:.ai/specs/%'",
        )
        .bind(project)
        .fetch_all(&mut *tx)
        .await?;
        let own_prefix = format!("{LEGACY_CLAIM_PREFIX}{name}#");
        for (claim_id, task_key, status) in claims {
            let Some(item) = task_key.strip_prefix(own_prefix.as_str()) else {
                continue;
            };
            let new_key = format!("spec-item:{new_path}#{item}");
            if UNRESOLVED_CLAIM_STATUSES.contains(&status.as_str()) {
                let holder: Option<i64> = sqlx::query_scalar(
                    "SELECT id FROM task_claims WHERE project_id = ? AND task_key = ? \
                     AND status IN ('probing', 'open', 'claimed') AND id != ?",
                )
                .bind(project)
                .bind(&new_key)
                .bind(claim_id)
                .fetch_optional(&mut *tx)
                .await?;
                if holder.is_some() {
                    tracing::warn!(
                        claim = %task_key,
                        new = %new_key,
                        "claim re-key skipped: an unresolved claim already holds the new key"
                    );
                    push_conflict(&mut conflicts, &new_path);
                    continue;
                }
            }
            sqlx::query("UPDATE task_claims SET task_key = ? WHERE id = ?")
                .bind(&new_key)
                .bind(claim_id)
                .execute(&mut *tx)
                .await?;
        }
    }
    tx.commit().await?;
    Ok(conflicts)
}

pub fn content_sha(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

pub fn resolve_stored_path(roots: &ScanRoots, stored: &str) -> Option<PathBuf> {
    if let Some(path) = stored.strip_prefix(".ai/specs/") {
        return roots
            .project_root
            .as_ref()
            .map(|root| root.join(".ai").join("specs").join(path));
    }
    stored.strip_prefix("<state_dir>/specs/").and_then(|path| {
        roots
            .state_dir
            .as_ref()
            .map(|root| root.join("specs").join(path))
    })
}

fn title(bytes: &[u8], fallback: &str) -> String {
    std::str::from_utf8(bytes)
        .ok()
        .and_then(|text| {
            text.lines()
                .find_map(|line| line.strip_prefix("# ").map(str::to_owned))
        })
        .unwrap_or_else(|| fallback.to_owned())
}

pub async fn scan(
    pool: &SqlitePool,
    project_id: Option<&str>,
    roots: &ScanRoots,
) -> Result<ScanReport, ScanError> {
    // Ahead of discovery and of the abandon pass, so a moved spec keeps its row instead of being
    // abandoned under the old path and registered afresh under the new one.
    let rekey_conflicts = rekey_legacy_specs(pool, project_id, roots).await?;
    let mut files = Vec::new();
    if let Some(root) = &roots.project_root {
        files.push((root.join(".ai").join("specs"), ".ai/specs/".to_owned()));
    }
    if let Some(root) = &roots.state_dir {
        files.push((root.join("specs"), "<state_dir>/specs/".to_owned()));
    }
    let mut discovered = Vec::new();
    // Prefixes whose directory was read, including empty directories: only specs under these can
    // be told to be gone.
    let mut read_prefixes: Vec<String> = Vec::new();
    for (directory, prefix) in files {
        let entries = match fs::read_dir(directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        };
        read_prefixes.push(prefix.clone());
        for entry in entries {
            let entry = entry?;
            let path = entry.path();
            if !path.is_file() || path.extension().and_then(|ext| ext.to_str()) != Some("md") {
                continue;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            discovered.push((format!("{prefix}{name}"), path, name));
        }
    }
    discovered.sort_by(|left, right| left.0.cmp(&right.0));
    let mut report = ScanReport {
        specs: Vec::new(),
        new: 0,
        changed: 0,
        unchanged: 0,
        rekey_conflicts,
    };
    let found: std::collections::HashSet<String> = discovered
        .iter()
        .map(|(stored, _, _)| stored.clone())
        .collect();
    for (stored_path, path, name) in discovered {
        let bytes = fs::read(&path);
        let bytes = bytes?;
        let sha = content_sha(&bytes);
        let previous =
            super::store::get_spec(pool, &super::model::spec_id(project_id, &stored_path)).await?;
        let spec = upsert_by_path(
            pool,
            project_id,
            &stored_path,
            &title(&bytes, name.trim_end_matches(".md")),
            &sha,
        )
        .await?;
        let spec = if spec.status == SpecStatus::Abandoned
            && spec
                .status_reason
                .as_deref()
                .is_some_and(|reason| reason.starts_with(GONE_REASON_PREFIX))
        {
            super::store::set_status(pool, &spec.id, SpecStatus::Discovered, None).await?;
            super::store::get_spec(pool, &spec.id)
                .await?
                .unwrap_or(spec)
        } else {
            spec
        };
        match previous {
            None => report.new += 1,
            Some(old) if old.content_sha == sha => report.unchanged += 1,
            Some(_) => report.changed += 1,
        }
        report.specs.push(spec);
    }
    if !read_prefixes.is_empty() {
        for spec in super::store::list_specs_by_project(pool, project_id).await? {
            let read = read_prefixes
                .iter()
                .any(|prefix| spec.path.starts_with(prefix.as_str()));
            if read
                && !found.contains(&spec.path)
                && !matches!(spec.status, SpecStatus::Done | SpecStatus::Abandoned)
            {
                let reason = format!("{GONE_REASON_PREFIX}{}", spec.path);
                super::store::set_status(pool, &spec.id, SpecStatus::Abandoned, Some(&reason))
                    .await?;
            }
        }
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spec_backlog::model::{
        CriterionKind, CriterionStatus, NewCriterion, Spec, SpecKind, SpecStatus, spec_id,
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
        replace_criteria(
            &pool,
            &id,
            &sha_hex(version_1),
            &open_items(&["one", "two"]),
        )
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
        assert!(
            criteria
                .iter()
                .all(|criterion| criterion.status == CriterionStatus::Stale)
        );
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
        let owned: Vec<u8> = vec![0xff, 0xfe, 0x00, 0x80, b'\n'];
        let bytes: &[u8] = &owned;
        assert!(String::from_utf8(owned.clone()).is_err());
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
        assert_eq!(
            resolve_stored_path(&both, project_path),
            Some(project_file.clone())
        );
        assert_eq!(
            resolve_stored_path(&both, state_path),
            Some(state_file.clone())
        );

        let no_project = ScanRoots {
            project_root: None,
            state_dir: Some(PathBuf::from("state")),
        };
        assert_eq!(resolve_stored_path(&no_project, project_path), None);
        assert_eq!(
            resolve_stored_path(&no_project, state_path),
            Some(state_file)
        );

        let no_state = ScanRoots {
            project_root: Some(PathBuf::from("proj")),
            state_dir: None,
        };
        assert_eq!(resolve_stored_path(&no_state, state_path), None);
        assert_eq!(
            resolve_stored_path(&no_state, project_path),
            Some(project_file)
        );
    }

    #[tokio::test]
    async fn a_deleted_spec_is_set_aside_and_says_why() {
        let pool = crate::testdb::fresh_pool().await;
        let project = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        let file = write_file(&project_specs(project.path()), "gone.md", b"# Gone\n");
        let roots = roots_at(project.path(), state.path());
        scan(&pool, PROJECT, &roots).await.unwrap();
        let id = spec_id(PROJECT, ".ai/specs/gone.md");

        fs::remove_file(file).unwrap();
        scan(&pool, PROJECT, &roots).await.unwrap();

        let spec = stored(&pool, &id).await;
        assert_eq!(spec.status, SpecStatus::Abandoned);
        assert!(
            spec.status_reason
                .as_deref()
                .unwrap_or_default()
                .contains(&spec.path)
        );
        assert_eq!(get_spec(&pool, &id).await.unwrap(), Some(spec));
    }

    #[tokio::test]
    async fn a_missing_specs_directory_abandons_nothing() {
        let pool = crate::testdb::fresh_pool().await;
        let project = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        write_file(&project_specs(project.path()), "a.md", b"# A\n");
        let roots = roots_at(project.path(), state.path());
        scan(&pool, PROJECT, &roots).await.unwrap();
        let id = spec_id(PROJECT, ".ai/specs/a.md");
        mark_extracted(&pool, &id).await;

        fs::remove_dir_all(project_specs(project.path())).unwrap();
        scan(&pool, PROJECT, &roots).await.unwrap();

        let spec = stored(&pool, &id).await;
        assert_eq!(spec.status, SpecStatus::Extracted);
        assert_ne!(spec.status, SpecStatus::Abandoned);
    }

    #[tokio::test]
    async fn an_empty_specs_directory_still_means_the_spec_is_gone() {
        let pool = crate::testdb::fresh_pool().await;
        let project = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        let file = write_file(&project_specs(project.path()), "a.md", b"# A\n");
        let roots = roots_at(project.path(), state.path());
        scan(&pool, PROJECT, &roots).await.unwrap();
        let id = spec_id(PROJECT, ".ai/specs/a.md");

        fs::remove_file(file).unwrap();
        scan(&pool, PROJECT, &roots).await.unwrap();

        assert_eq!(stored(&pool, &id).await.status, SpecStatus::Abandoned);
    }

    #[tokio::test]
    async fn a_spec_that_comes_back_is_discovered_again() {
        let pool = crate::testdb::fresh_pool().await;
        let project = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        let bytes = b"# Back\n";
        let file = write_file(&project_specs(project.path()), "back.md", bytes);
        let roots = roots_at(project.path(), state.path());
        scan(&pool, PROJECT, &roots).await.unwrap();
        let id = spec_id(PROJECT, ".ai/specs/back.md");

        fs::remove_file(&file).unwrap();
        scan(&pool, PROJECT, &roots).await.unwrap();
        fs::write(&file, bytes).unwrap();
        scan(&pool, PROJECT, &roots).await.unwrap();

        assert_eq!(stored(&pool, &id).await.status, SpecStatus::Discovered);
    }

    #[tokio::test]
    async fn a_finished_spec_whose_file_is_gone_stays_done() {
        let pool = crate::testdb::fresh_pool().await;
        let project = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        let file = write_file(&project_specs(project.path()), "done.md", b"# Done\n");
        let roots = roots_at(project.path(), state.path());
        scan(&pool, PROJECT, &roots).await.unwrap();
        let id = spec_id(PROJECT, ".ai/specs/done.md");
        sqlx::query("UPDATE specs SET status = 'done' WHERE id = ?")
            .bind(&id)
            .execute(&pool)
            .await
            .unwrap();

        fs::remove_file(file).unwrap();
        scan(&pool, PROJECT, &roots).await.unwrap();

        assert_eq!(stored(&pool, &id).await.status, SpecStatus::Done);
    }

    #[tokio::test]
    async fn a_scan_without_a_state_dir_leaves_state_dir_specs_alone() {
        let pool = crate::testdb::fresh_pool().await;
        let project = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        write_file(&project_specs(project.path()), "project.md", b"# Project\n");
        write_file(&state_specs(state.path()), "state.md", b"# State\n");
        let roots = roots_at(project.path(), state.path());
        scan(&pool, PROJECT, &roots).await.unwrap();
        let state_id = spec_id(PROJECT, "<state_dir>/specs/state.md");
        let project_id = spec_id(PROJECT, ".ai/specs/project.md");
        mark_extracted(&pool, &state_id).await;
        mark_extracted(&pool, &project_id).await;

        scan(
            &pool,
            PROJECT,
            &ScanRoots {
                project_root: Some(project.path().to_path_buf()),
                state_dir: None,
            },
        )
        .await
        .unwrap();
        assert_eq!(stored(&pool, &state_id).await.status, SpecStatus::Extracted);
        scan(
            &pool,
            PROJECT,
            &ScanRoots {
                project_root: None,
                state_dir: Some(state.path().to_path_buf()),
            },
        )
        .await
        .unwrap();
        assert_eq!(
            stored(&pool, &project_id).await.status,
            SpecStatus::Extracted
        );
    }

    #[tokio::test]
    async fn a_missing_spec_of_another_project_is_left_alone() {
        let pool = crate::testdb::fresh_pool().await;
        let project_a = tempfile::tempdir().unwrap();
        let project_b = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        write_file(&project_specs(project_b.path()), "other.md", b"# Other\n");
        scan(
            &pool,
            Some("project-b"),
            &roots_at(project_b.path(), state.path()),
        )
        .await
        .unwrap();
        let other = spec_id(Some("project-b"), ".ai/specs/other.md");
        mark_extracted(&pool, &other).await;

        scan(
            &pool,
            Some("project-a"),
            &roots_at(project_a.path(), state.path()),
        )
        .await
        .unwrap();

        assert_eq!(stored(&pool, &other).await.status, SpecStatus::Extracted);
    }

    // ---- re-key of legacy `.ai/specs/X` rows to `<state_dir>/specs/X` ----

    use crate::spec_backlog::model::DepSource;
    use crate::spec_backlog::store::{
        add_dep, add_question, list_deps, list_questions, list_specs_by_project,
    };

    const SPEC_BODY: &[u8] = b"# Alpha\nbody\n";
    const OTHER_PROJECT: Option<&str> = Some("proj-2");

    fn legacy_path(name: &str) -> String {
        format!(".ai/specs/{name}")
    }

    fn state_path(name: &str) -> String {
        format!("<state_dir>/specs/{name}")
    }

    /// Writes `.ai/specs/<name>`, scans, and returns the id of the legacy row.
    async fn seed_legacy(
        pool: &SqlitePool,
        project: &Path,
        state: &Path,
        owner: Option<&str>,
        name: &str,
    ) -> String {
        write_file(&project_specs(project), name, SPEC_BODY);
        scan(pool, owner, &roots_at(project, state)).await.unwrap();
        spec_id(owner, &legacy_path(name))
    }

    /// The migration: the legacy file is gone and the same-named file sits in the state dir.
    fn move_to_state(project: &Path, state: &Path, name: &str) {
        fs::remove_file(project_specs(project).join(name)).unwrap();
        write_file(&state_specs(state), name, SPEC_BODY);
    }

    async fn insert_claim(pool: &SqlitePool, project_id: &str, key: &str, status: &str) {
        sqlx::query(
            "INSERT INTO task_claims (project_id, task_key, kind, title, status, created_at) \
             VALUES (?, ?, 'declared', 'item', ?, '2026-11-09T00:00:00Z')",
        )
        .bind(project_id.to_owned())
        .bind(key.to_owned())
        .bind(status.to_owned())
        .execute(pool)
        .await
        .unwrap();
    }

    async fn claim_keys(pool: &SqlitePool, project_id: &str) -> Vec<String> {
        sqlx::query_scalar(
            "SELECT task_key FROM task_claims WHERE project_id = ? ORDER BY task_key",
        )
        .bind(project_id.to_owned())
        .fetch_all(pool)
        .await
        .unwrap()
    }

    async fn insert_run(pool: &SqlitePool, id: &str, run_id: &str) {
        sqlx::query(
            "INSERT INTO spec_extraction_runs (spec_id, run_id, created_at) \
             VALUES (?, ?, '2026-11-09T00:00:00Z')",
        )
        .bind(id.to_owned())
        .bind(run_id.to_owned())
        .execute(pool)
        .await
        .unwrap();
    }

    async fn run_count(pool: &SqlitePool, id: &str) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM spec_extraction_runs WHERE spec_id = ?")
            .bind(id.to_owned())
            .fetch_one(pool)
            .await
            .unwrap()
    }

    async fn rows_named(pool: &SqlitePool, owner: Option<&str>, name: &str) -> usize {
        list_specs_by_project(pool, owner)
            .await
            .unwrap()
            .iter()
            .filter(|spec| spec.path.ends_with(&format!("/specs/{name}")))
            .count()
    }

    #[tokio::test]
    async fn rekey_moves_a_legacy_spec_with_all_its_children_to_the_state_dir_id() {
        let pool = crate::testdb::fresh_pool().await;
        let project = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        let old = seed_legacy(&pool, project.path(), state.path(), PROJECT, "alpha.md").await;
        let other = seed_legacy(&pool, project.path(), state.path(), PROJECT, "beta.md").await;
        mark_extracted(&pool, &old).await;
        replace_criteria(&pool, &old, "sha", &open_items(&["one", "two"]))
            .await
            .unwrap();
        add_dep(&pool, &old, &other, DepSource::Owner)
            .await
            .unwrap();
        add_dep(&pool, &other, &old, DepSource::Owner)
            .await
            .unwrap();
        add_question(&pool, &old, "why?").await.unwrap();
        insert_run(&pool, &old, "run-1").await;
        insert_claim(&pool, "proj-1", "spec-item:.ai/specs/alpha.md#1", "open").await;
        insert_claim(&pool, "proj-1", "spec-item:.ai/specs/beta.md#1", "open").await;
        move_to_state(project.path(), state.path(), "alpha.md");

        let report = scan(&pool, PROJECT, &roots_at(project.path(), state.path()))
            .await
            .unwrap();

        let new = spec_id(PROJECT, &state_path("alpha.md"));
        assert_ne!(new, old);
        assert!(report.rekey_conflicts.is_empty());
        assert!(get_spec(&pool, &old).await.unwrap().is_none());
        let moved = stored(&pool, &new).await;
        assert_eq!(moved.path, state_path("alpha.md"));
        assert_eq!(moved.status, SpecStatus::Extracted, "not abandoned");
        assert_eq!(list_criteria(&pool, &new).await.unwrap().len(), 2);
        assert!(list_criteria(&pool, &old).await.unwrap().is_empty());
        let as_spec: Vec<String> = list_deps(&pool, &new)
            .await
            .unwrap()
            .into_iter()
            .map(|dep| dep.depends_on)
            .collect();
        assert_eq!(as_spec, vec![other.clone()]);
        let as_dependency: Vec<String> = list_deps(&pool, &other)
            .await
            .unwrap()
            .into_iter()
            .map(|dep| dep.depends_on)
            .collect();
        assert_eq!(as_dependency, vec![new.clone()]);
        assert_eq!(list_questions(&pool, &new).await.unwrap().len(), 1);
        assert!(list_questions(&pool, &old).await.unwrap().is_empty());
        assert_eq!(run_count(&pool, &new).await, 1);
        assert_eq!(run_count(&pool, &old).await, 0);
        assert_eq!(
            claim_keys(&pool, "proj-1").await,
            vec![
                "spec-item:.ai/specs/beta.md#1".to_owned(),
                "spec-item:<state_dir>/specs/alpha.md#1".to_owned(),
            ]
        );
        let untouched = stored(&pool, &other).await;
        assert_eq!(untouched.path, legacy_path("beta.md"));
        assert_eq!(untouched.status, SpecStatus::Discovered);
    }

    #[tokio::test]
    async fn rekey_replaces_an_empty_row_already_at_the_new_path() {
        let pool = crate::testdb::fresh_pool().await;
        let project = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        // Both files exist for the first scan, so both rows are created.
        write_file(&state_specs(state.path()), "alpha.md", SPEC_BODY);
        let old = seed_legacy(&pool, project.path(), state.path(), PROJECT, "alpha.md").await;
        let new = spec_id(PROJECT, &state_path("alpha.md"));
        assert_eq!(rows_named(&pool, PROJECT, "alpha.md").await, 2);
        mark_extracted(&pool, &old).await;
        replace_criteria(&pool, &old, "sha", &open_items(&["one", "two"]))
            .await
            .unwrap();
        fs::remove_file(project_specs(project.path()).join("alpha.md")).unwrap();

        let report = scan(&pool, PROJECT, &roots_at(project.path(), state.path()))
            .await
            .unwrap();

        assert!(report.rekey_conflicts.is_empty());
        assert_eq!(rows_named(&pool, PROJECT, "alpha.md").await, 1);
        assert!(get_spec(&pool, &old).await.unwrap().is_none());
        let kept = stored(&pool, &new).await;
        assert_eq!(kept.path, state_path("alpha.md"));
        assert_eq!(kept.status, SpecStatus::Extracted);
        assert_eq!(list_criteria(&pool, &new).await.unwrap().len(), 2);
    }

    #[tokio::test]
    async fn rekey_leaves_both_rows_when_the_new_path_row_has_children() {
        let pool = crate::testdb::fresh_pool().await;
        let project = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        write_file(&state_specs(state.path()), "alpha.md", SPEC_BODY);
        let old = seed_legacy(&pool, project.path(), state.path(), PROJECT, "alpha.md").await;
        let new = spec_id(PROJECT, &state_path("alpha.md"));
        replace_criteria(&pool, &old, "sha", &open_items(&["one", "two"]))
            .await
            .unwrap();
        replace_criteria(&pool, &new, "sha", &open_items(&["only"]))
            .await
            .unwrap();
        fs::remove_file(project_specs(project.path()).join("alpha.md")).unwrap();

        let report = scan(&pool, PROJECT, &roots_at(project.path(), state.path()))
            .await
            .unwrap();

        assert_eq!(report.rekey_conflicts, vec![state_path("alpha.md")]);
        assert_eq!(stored(&pool, &old).await.path, legacy_path("alpha.md"));
        assert_eq!(stored(&pool, &new).await.path, state_path("alpha.md"));
        assert_eq!(list_criteria(&pool, &old).await.unwrap().len(), 2);
        assert_eq!(list_criteria(&pool, &new).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn rekey_is_idempotent_across_scans() {
        let pool = crate::testdb::fresh_pool().await;
        let project = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        let old = seed_legacy(&pool, project.path(), state.path(), PROJECT, "alpha.md").await;
        replace_criteria(&pool, &old, "sha", &open_items(&["one"]))
            .await
            .unwrap();
        insert_claim(&pool, "proj-1", "spec-item:.ai/specs/alpha.md#1", "open").await;
        move_to_state(project.path(), state.path(), "alpha.md");
        let roots = roots_at(project.path(), state.path());

        scan(&pool, PROJECT, &roots).await.unwrap();
        let rows = list_specs_by_project(&pool, PROJECT).await.unwrap().len();
        let second = scan(&pool, PROJECT, &roots).await.unwrap();

        let new = spec_id(PROJECT, &state_path("alpha.md"));
        assert!(second.rekey_conflicts.is_empty());
        assert_eq!(second.specs.len(), 1);
        assert_eq!(second.specs[0].id, new);
        assert_eq!(
            list_specs_by_project(&pool, PROJECT).await.unwrap().len(),
            rows
        );
        assert_eq!(list_criteria(&pool, &new).await.unwrap().len(), 1);
        assert_eq!(
            claim_keys(&pool, "proj-1").await,
            vec!["spec-item:<state_dir>/specs/alpha.md#1".to_owned()]
        );
    }

    #[tokio::test]
    async fn rekey_skips_a_spec_with_no_file_in_the_state_dir() {
        let pool = crate::testdb::fresh_pool().await;
        let project = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        let old = seed_legacy(&pool, project.path(), state.path(), PROJECT, "alpha.md").await;
        insert_claim(&pool, "proj-1", "spec-item:.ai/specs/alpha.md#1", "open").await;
        fs::remove_file(project_specs(project.path()).join("alpha.md")).unwrap();
        fs::create_dir_all(state_specs(state.path())).unwrap();

        let report = scan(&pool, PROJECT, &roots_at(project.path(), state.path()))
            .await
            .unwrap();

        assert!(report.rekey_conflicts.is_empty());
        let row = stored(&pool, &old).await;
        assert_eq!(row.path, legacy_path("alpha.md"));
        assert_eq!(row.id, old);
        assert!(
            get_spec(&pool, &spec_id(PROJECT, &state_path("alpha.md")))
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(
            claim_keys(&pool, "proj-1").await,
            vec!["spec-item:.ai/specs/alpha.md#1".to_owned()]
        );
    }

    #[tokio::test]
    async fn rekey_skips_when_the_legacy_file_still_exists() {
        let pool = crate::testdb::fresh_pool().await;
        let project = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        let old = seed_legacy(&pool, project.path(), state.path(), PROJECT, "alpha.md").await;
        replace_criteria(&pool, &old, "sha", &open_items(&["one"]))
            .await
            .unwrap();
        insert_claim(&pool, "proj-1", "spec-item:.ai/specs/alpha.md#1", "open").await;
        // The state-dir copy exists but the `.ai` file has not been moved away yet.
        write_file(&state_specs(state.path()), "alpha.md", SPEC_BODY);

        let report = scan(&pool, PROJECT, &roots_at(project.path(), state.path()))
            .await
            .unwrap();

        assert!(report.rekey_conflicts.is_empty());
        assert_eq!(stored(&pool, &old).await.path, legacy_path("alpha.md"));
        assert_eq!(list_criteria(&pool, &old).await.unwrap().len(), 1);
        let new = spec_id(PROJECT, &state_path("alpha.md"));
        assert_eq!(stored(&pool, &new).await.path, state_path("alpha.md"));
        assert!(list_criteria(&pool, &new).await.unwrap().is_empty());
        assert_eq!(
            claim_keys(&pool, "proj-1").await,
            vec!["spec-item:.ai/specs/alpha.md#1".to_owned()]
        );
    }

    #[tokio::test]
    async fn rekey_leaves_another_projects_rows_and_claims_alone() {
        let pool = crate::testdb::fresh_pool().await;
        let project = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        let mine = seed_legacy(&pool, project.path(), state.path(), PROJECT, "alpha.md").await;
        let theirs = seed_legacy(
            &pool,
            project.path(),
            state.path(),
            OTHER_PROJECT,
            "alpha.md",
        )
        .await;
        assert_ne!(mine, theirs);
        replace_criteria(&pool, &theirs, "sha", &open_items(&["one"]))
            .await
            .unwrap();
        insert_claim(&pool, "proj-1", "spec-item:.ai/specs/alpha.md#1", "open").await;
        insert_claim(&pool, "proj-2", "spec-item:.ai/specs/alpha.md#1", "open").await;
        move_to_state(project.path(), state.path(), "alpha.md");

        scan(&pool, PROJECT, &roots_at(project.path(), state.path()))
            .await
            .unwrap();

        assert_eq!(
            stored(&pool, &theirs).await.path,
            legacy_path("alpha.md"),
            "the other project's row keeps its legacy path and id"
        );
        assert_eq!(list_criteria(&pool, &theirs).await.unwrap().len(), 1);
        assert_eq!(
            claim_keys(&pool, "proj-2").await,
            vec!["spec-item:.ai/specs/alpha.md#1".to_owned()]
        );
        assert_eq!(
            claim_keys(&pool, "proj-1").await,
            vec!["spec-item:<state_dir>/specs/alpha.md#1".to_owned()]
        );
    }

    #[tokio::test]
    async fn rekey_keeps_a_claim_whose_new_key_is_already_unresolved() {
        let pool = crate::testdb::fresh_pool().await;
        let project = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        let old = seed_legacy(&pool, project.path(), state.path(), PROJECT, "alpha.md").await;
        insert_claim(&pool, "proj-1", "spec-item:.ai/specs/alpha.md#1", "open").await;
        insert_claim(
            &pool,
            "proj-1",
            "spec-item:<state_dir>/specs/alpha.md#1",
            "open",
        )
        .await;
        move_to_state(project.path(), state.path(), "alpha.md");

        let report = scan(&pool, PROJECT, &roots_at(project.path(), state.path()))
            .await
            .unwrap();

        assert_eq!(report.rekey_conflicts, vec![state_path("alpha.md")]);
        assert_eq!(
            claim_keys(&pool, "proj-1").await,
            vec![
                "spec-item:.ai/specs/alpha.md#1".to_owned(),
                "spec-item:<state_dir>/specs/alpha.md#1".to_owned(),
            ]
        );
        let new = spec_id(PROJECT, &state_path("alpha.md"));
        assert!(get_spec(&pool, &old).await.unwrap().is_none());
        assert_eq!(stored(&pool, &new).await.path, state_path("alpha.md"));
    }
}
