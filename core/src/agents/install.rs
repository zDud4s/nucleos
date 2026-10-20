//! Installs the agent packages embedded in the binary into the agents home
//! (`~/.nucleos/agents/<name>/<version>/`) and keeps the `source = 'package'` catalog rows in sync.
//!
//! A version directory is immutable once installed: the same `name@version` with different content
//! is an error (the author forgot a version bump), never an overwrite. Per-package failures are
//! collected in the report and never stop the other packages.

use std::collections::BTreeMap;
use std::path::Path;

use nucleos_base::agent::{PackageAgentRow, upsert_package_agent};
use sha2::{Digest, Sha256};
use sqlx::SqlitePool;

use super::package::{HASH_FILE, PackageRef, installed_root, load_installed, parse_package};

mod embedded {
    include!(concat!(env!("OUT_DIR"), "/agents_embedded.rs"));
}

/// One package that could not be installed or registered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallError {
    pub reference: String,
    pub message: String,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct InstallReport {
    /// Written to disk by this run.
    pub installed: Vec<PackageRef>,
    /// Already on disk with identical content.
    pub unchanged: Vec<PackageRef>,
    pub errors: Vec<InstallError>,
}

/// Lowercase hex sha256 over the files sorted by path (byte-wise, `/`-separated paths relative to
/// `<name>/<version>/`); each file contributes `path` UTF-8 bytes, `0u8`, raw content bytes, `0u8`.
/// Independent of the order the files are given in. `content.sha256` is written after hashing, with
/// this hex string and no trailing newline; a source package must not ship one (it would be hashed
/// as content, then overwritten).
pub fn content_hash(files: &[(String, Vec<u8>)]) -> String {
    let mut sorted: Vec<&(String, Vec<u8>)> = files.iter().collect();
    sorted.sort_by(|a, b| a.0.cmp(&b.0));
    let mut hasher = Sha256::new();
    for (path, bytes) in sorted {
        hasher.update(path.as_bytes());
        hasher.update([0u8]);
        hasher.update(bytes);
        hasher.update([0u8]);
    }
    format!("{:x}", hasher.finalize())
}

/// Installs the packages embedded in the binary.
pub async fn install_all(pool: &SqlitePool, home: &Path) -> InstallReport {
    install_from(pool, home, embedded::EMBEDDED).await
}

/// Installs a set of `<name>/<version>/<relative path>` files under `home`.
pub async fn install_from(
    pool: &SqlitePool,
    home: &Path,
    files: &[(&str, &[u8])],
) -> InstallReport {
    let mut report = InstallReport::default();

    // Group by the first two path segments; the rest is the path inside the package.
    let mut groups: BTreeMap<(String, String), Vec<(String, Vec<u8>)>> = BTreeMap::new();
    for (path, bytes) in files {
        let mut parts = path.splitn(3, '/');
        let (Some(name), Some(version), Some(rest)) = (parts.next(), parts.next(), parts.next())
        else {
            report.errors.push(InstallError {
                reference: (*path).to_string(),
                message: "embedded file is not under <name>/<version>/".into(),
            });
            continue;
        };
        groups
            .entry((name.to_string(), version.to_string()))
            .or_default()
            .push((rest.to_string(), bytes.to_vec()));
    }

    // Validated references, in (name, version) order so the highest version upserts last.
    let mut packages: Vec<(PackageRef, Vec<(String, Vec<u8>)>)> = Vec::new();
    for ((name, version), pkg_files) in groups {
        match PackageRef::parse(&format!("{name}@{version}")) {
            Ok(reference) => packages.push((reference, pkg_files)),
            Err(e) => report.errors.push(InstallError {
                reference: format!("{name}@{version}"),
                message: e.to_string(),
            }),
        }
    }
    packages.sort_by(|a, b| (&a.0.name, a.0.version_key()).cmp(&(&b.0.name, b.0.version_key())));

    let mut registrable: Vec<PackageRef> = Vec::new();
    for (reference, pkg_files) in packages {
        match install_one(home, &reference, &pkg_files) {
            Ok(true) => {
                report.installed.push(reference.clone());
                registrable.push(reference);
            }
            Ok(false) => {
                report.unchanged.push(reference.clone());
                registrable.push(reference);
            }
            Err(message) => report.errors.push(InstallError {
                reference: reference.to_string(),
                message,
            }),
        }
    }

    for reference in registrable {
        let package = match load_installed(home, &reference) {
            Ok(p) => p,
            Err(e) => {
                report.errors.push(InstallError {
                    reference: reference.to_string(),
                    message: e.to_string(),
                });
                continue;
            }
        };
        let row = PackageAgentRow {
            name: reference.name.clone(),
            speciality: package.description.clone(),
            prompt: package.body.clone(),
            capabilities: package
                .capabilities
                .iter()
                .map(|c| c.as_str().to_string())
                .collect(),
            package_ref: reference.to_string(),
        };
        if let Err(e) = upsert_package_agent(pool, &row).await {
            report.errors.push(InstallError {
                reference: reference.to_string(),
                message: format!("could not register the agent: {e}"),
            });
        }
    }

    report
}

/// Writes one package to disk. `Ok(true)` = newly installed, `Ok(false)` = already there and identical.
fn install_one(
    home: &Path,
    reference: &PackageRef,
    files: &[(String, Vec<u8>)],
) -> Result<bool, String> {
    let hash = content_hash(files);
    let target = installed_root(home, reference);

    if target.exists() {
        let on_disk = std::fs::read_to_string(target.join(HASH_FILE)).ok();
        return if on_disk.as_deref().map(str::trim) == Some(hash.as_str()) {
            Ok(false)
        } else {
            Err(format!("{reference} changed without a version bump"))
        };
    }

    // `<name>/<version>` like the target, because the parser checks the directory it is given.
    let staging = home
        .join(".staging")
        .join(&reference.name)
        .join(&reference.version);
    if staging.exists() {
        std::fs::remove_dir_all(&staging).map_err(|e| format!("could not clear staging: {e}"))?;
    }

    let result = stage_and_publish(&staging, &target, reference, files, &hash);
    if result.is_err() {
        let _ = std::fs::remove_dir_all(&staging);
    }
    result.map(|()| true)
}

fn stage_and_publish(
    staging: &Path,
    target: &Path,
    reference: &PackageRef,
    files: &[(String, Vec<u8>)],
    hash: &str,
) -> Result<(), String> {
    for (rel, bytes) in files {
        // A `..` here would escape the staging directory; the parser re-checks declared paths,
        // but a stray embedded file must never be written outside it.
        if rel
            .split('/')
            .any(|seg| seg.is_empty() || seg == "." || seg == "..")
            || rel.contains('\\')
        {
            return Err(format!("{reference}: unsafe file path {rel:?}"));
        }
        let dest = staging.join(rel);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("could not write {rel}: {e}"))?;
        }
        std::fs::write(&dest, bytes).map_err(|e| format!("could not write {rel}: {e}"))?;
    }
    std::fs::write(staging.join(HASH_FILE), hash)
        .map_err(|e| format!("could not write {HASH_FILE}: {e}"))?;

    parse_package(staging, reference).map_err(|e| e.to_string())?;

    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("could not create {parent:?}: {e}"))?;
    }
    std::fs::rename(staging, target).map_err(|e| format!("could not move into place: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn test_pool() -> SqlitePool {
        crate::testdb::fresh_pool().await
    }

    const OUTPUT_MD: &[u8] = b"Reply with a short summary.\n";

    fn agent_md(name: &str, version: &str, extra: &str) -> Vec<u8> {
        format!(
            "---\nname: {name}\nversion: {version}\ndescription: Does {name} things.\n\
             capabilities: [fs.read]\n{extra}---\nYou are {name}.\n"
        )
        .into_bytes()
    }

    async fn row_ref(pool: &SqlitePool, name: &str) -> Option<String> {
        sqlx::query_scalar::<_, Option<String>>("SELECT package_ref FROM agents WHERE name = ?")
            .bind(name)
            .fetch_optional(pool)
            .await
            .unwrap()
            .flatten()
    }

    #[tokio::test]
    async fn installs_a_package_and_upserts_its_row() {
        let pool = test_pool().await;
        let home = tempfile::tempdir().unwrap();
        let md = agent_md("alpha", "1.0.0", "");
        let files: &[(&str, &[u8])] = &[
            ("alpha/1.0.0/agent.md", &md),
            ("alpha/1.0.0/output.md", OUTPUT_MD),
        ];

        let report = install_from(&pool, home.path(), files).await;

        assert!(report.errors.is_empty(), "{:?}", report.errors);
        assert_eq!(
            report.installed,
            vec![PackageRef::parse("alpha@1.0.0").unwrap()]
        );
        assert!(home.path().join("alpha/1.0.0").join(HASH_FILE).is_file());
        assert!(!home.path().join(".staging/alpha/1.0.0").exists());
        assert_eq!(
            row_ref(&pool, "alpha").await.as_deref(),
            Some("alpha@1.0.0")
        );
    }

    #[tokio::test]
    async fn second_run_is_unchanged() {
        let pool = test_pool().await;
        let home = tempfile::tempdir().unwrap();
        let md = agent_md("alpha", "1.0.0", "");
        let files: &[(&str, &[u8])] = &[
            ("alpha/1.0.0/agent.md", &md),
            ("alpha/1.0.0/output.md", OUTPUT_MD),
        ];

        install_from(&pool, home.path(), files).await;
        let report = install_from(&pool, home.path(), files).await;

        assert!(report.errors.is_empty(), "{:?}", report.errors);
        assert!(report.installed.is_empty());
        assert_eq!(
            report.unchanged,
            vec![PackageRef::parse("alpha@1.0.0").unwrap()]
        );
        assert_eq!(
            row_ref(&pool, "alpha").await.as_deref(),
            Some("alpha@1.0.0")
        );
    }

    #[tokio::test]
    async fn changed_content_without_a_version_bump_is_an_error() {
        let pool = test_pool().await;
        let home = tempfile::tempdir().unwrap();
        let md = agent_md("alpha", "1.0.0", "");
        let first: &[(&str, &[u8])] = &[
            ("alpha/1.0.0/agent.md", &md),
            ("alpha/1.0.0/output.md", OUTPUT_MD),
        ];
        install_from(&pool, home.path(), first).await;

        let changed_md = agent_md("alpha", "1.0.0", "")
            .into_iter()
            .chain(*b"more\n")
            .collect::<Vec<u8>>();
        let beta_md = agent_md("beta", "1.0.0", "");
        let second: &[(&str, &[u8])] = &[
            ("alpha/1.0.0/agent.md", &changed_md),
            ("alpha/1.0.0/output.md", OUTPUT_MD),
            ("beta/1.0.0/agent.md", &beta_md),
            ("beta/1.0.0/output.md", OUTPUT_MD),
        ];
        let report = install_from(&pool, home.path(), second).await;

        assert_eq!(report.errors.len(), 1, "{:?}", report.errors);
        assert_eq!(report.errors[0].reference, "alpha@1.0.0");
        assert_eq!(
            report.errors[0].message,
            "alpha@1.0.0 changed without a version bump"
        );
        assert_eq!(
            report.installed,
            vec![PackageRef::parse("beta@1.0.0").unwrap()]
        );
        // The installed copy and the catalog row are untouched.
        let on_disk = std::fs::read(home.path().join("alpha/1.0.0/agent.md")).unwrap();
        assert_eq!(on_disk, md);
        assert_eq!(
            row_ref(&pool, "alpha").await.as_deref(),
            Some("alpha@1.0.0")
        );
        assert_eq!(row_ref(&pool, "beta").await.as_deref(), Some("beta@1.0.0"));
    }

    #[tokio::test]
    async fn a_bad_package_is_reported_and_the_others_install() {
        let pool = test_pool().await;
        let home = tempfile::tempdir().unwrap();
        let bad_md = agent_md("bad", "1.0.0", "surprise: yes\n");
        let good_md = agent_md("good", "1.0.0", "");
        let files: &[(&str, &[u8])] = &[
            ("bad/1.0.0/agent.md", &bad_md),
            ("bad/1.0.0/output.md", OUTPUT_MD),
            ("good/1.0.0/agent.md", &good_md),
            ("good/1.0.0/output.md", OUTPUT_MD),
        ];

        let report = install_from(&pool, home.path(), files).await;

        assert_eq!(report.errors.len(), 1, "{:?}", report.errors);
        assert_eq!(report.errors[0].reference, "bad@1.0.0");
        assert_eq!(
            report.installed,
            vec![PackageRef::parse("good@1.0.0").unwrap()]
        );
        assert!(!home.path().join("bad").join("1.0.0").exists());
        assert!(!home.path().join(".staging/bad/1.0.0").exists());
        assert_eq!(row_ref(&pool, "bad").await, None);
        assert_eq!(row_ref(&pool, "good").await.as_deref(), Some("good@1.0.0"));
    }

    #[tokio::test]
    async fn the_highest_version_wins_the_row() {
        let pool = test_pool().await;
        let home = tempfile::tempdir().unwrap();
        let v1 = agent_md("alpha", "1.0.0", "");
        let v2 = agent_md("alpha", "1.10.0", "");
        let v9 = agent_md("alpha", "1.9.0", "");
        let files: &[(&str, &[u8])] = &[
            ("alpha/1.10.0/agent.md", &v2),
            ("alpha/1.10.0/output.md", OUTPUT_MD),
            ("alpha/1.0.0/agent.md", &v1),
            ("alpha/1.0.0/output.md", OUTPUT_MD),
            ("alpha/1.9.0/agent.md", &v9),
            ("alpha/1.9.0/output.md", OUTPUT_MD),
        ];

        let report = install_from(&pool, home.path(), files).await;

        assert!(report.errors.is_empty(), "{:?}", report.errors);
        assert_eq!(
            row_ref(&pool, "alpha").await.as_deref(),
            Some("alpha@1.10.0")
        );
    }

    #[test]
    fn content_hash_ignores_input_order() {
        let a = ("a.md".to_string(), b"one".to_vec());
        let b = ("b/c.md".to_string(), b"two".to_vec());
        let forward = content_hash(&[a.clone(), b.clone()]);
        let backward = content_hash(&[b, a]);
        assert_eq!(forward, backward);
        assert_eq!(forward.len(), 64);
        assert_ne!(
            forward,
            content_hash(&[("a.md".to_string(), b"other".to_vec())])
        );
    }

    #[tokio::test]
    async fn the_embedded_set_installs_without_errors() {
        let pool = test_pool().await;
        let home = tempfile::tempdir().unwrap();

        let report = install_all(&pool, home.path()).await;

        assert!(report.errors.is_empty(), "{:?}", report.errors);
        assert!(!report.installed.is_empty());
    }
}
