//! §spec motor-de-workflows
//!
//! Make a library bundle out of a folder that already holds a workflow:
//! `nucleos-core --workflow-package <name> <version> --from <project root> --manifest <bundle.yaml>`.
//!
//! The library (`workflows.rs`) could list, pin, eject and drift bundles, and the only way one got
//! INTO it was the daemon seeding its own autopilot. A workflow a person already works by — this
//! repository's `.ai/` harness is the case in point — had no way in short of copying files by hand
//! into a directory whose layout nothing documented.
//!
//! # The manifest says what the workflow is, and nothing here second-guesses it
//!
//! The caller hands over a `bundle.yaml` whose `owns:` list names the project paths the workflow
//! consists of — the same list `workflow_materialize.rs` later puts back into a checkout. This
//! module copies exactly those paths out of the project, under the same relative names, and writes
//! the manifest beside them byte for byte, comments included. It does not know what a skill or a
//! packet is; which files are the workflow and which are one project's state is the manifest
//! author's to decide.
//!
//! # A version is written once
//!
//! Refused when `<name>/<version>/` already exists. A version is defined by its hash (`seed.rs`
//! makes the same argument for the autopilot), and packaging over one would change what every pin
//! to it means without any pin knowing.
//!
//! # The library keeps a history, because the workflow has no repository of its own
//!
//! `~/.nucleos/workflows/<name>/` becomes a git repository on first package, and every version is
//! one commit. A workflow extracted from a project has left that project's history; this is where
//! its next history starts. Local only — no remote, no network. When git is not there, or the
//! commit fails, the bundle is still packaged and the answer says why it has no commit: the files
//! are the product, and the history is a record of them.

use std::path::{Path, PathBuf};

use crate::workflows;

/// What packaging did.
#[derive(Debug, Clone, PartialEq)]
pub struct Packaged {
    /// The new version directory.
    pub dir: PathBuf,
    /// How many files were copied into it, the manifest not counted.
    pub files: usize,
    /// The bundle's hash, as a pin to it will record.
    pub hash: String,
    /// The commit that records this version, or why there is none.
    pub commit: Result<String, String>,
}

#[derive(serde::Deserialize, Default)]
struct Owns {
    #[serde(default)]
    owns: Vec<String>,
}

/// Copy one file or directory tree, files only, skipping symlinks for the reason
/// `workflows::copy_tree` gives: following one would copy whatever it points at into the library.
fn copy_into(from: &Path, to: &Path, count: &mut usize) -> std::io::Result<()> {
    let kind = std::fs::symlink_metadata(from)?.file_type();
    if kind.is_symlink() {
        return Ok(());
    }
    if kind.is_dir() {
        std::fs::create_dir_all(to)?;
        let mut entries: Vec<_> = std::fs::read_dir(from)?.collect::<Result<_, _>>()?;
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            copy_into(&entry.path(), &to.join(entry.file_name()), count)?;
        }
        return Ok(());
    }
    if let Some(parent) = to.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::copy(from, to)?;
    *count += 1;
    Ok(())
}

/// Package `project_root`'s workflow, as `manifest` describes it, into `library_root` as
/// `name@version`.
///
/// Everything is checked before anything is written: the names, the version not existing yet,
/// every `owns:` entry normalising and existing in the project. A half-written version directory
/// would be a bundle in the library that hashes to something nobody packaged, so a failure while
/// copying removes the directory again.
pub async fn package(
    library_root: &Path,
    name: &str,
    version: &str,
    project_root: &Path,
    manifest: &Path,
) -> Result<Packaged, String> {
    if !workflows::valid_name(name) {
        return Err(format!("`{name}` is not a usable workflow name"));
    }
    if !workflows::valid_name(version) {
        return Err(format!("`{version}` is not a usable version"));
    }
    let text = std::fs::read_to_string(manifest)
        .map_err(|error| format!("{}: {error}", manifest.display()))?;
    let owns = serde_yaml::from_str::<Option<Owns>>(&text)
        .map_err(|error| format!("{}: {error}", manifest.display()))?
        .unwrap_or_default()
        .owns;
    if owns.is_empty() {
        return Err(format!(
            "{} names no files under `owns:`, so there is nothing to package",
            manifest.display()
        ));
    }
    let mut entries = Vec::new();
    for entry in &owns {
        let Some(rel) = crate::ownership::normalise(entry) else {
            return Err(format!(
                "`{entry}` is not a relative path inside the project (no `..`, nothing absolute, \
                 forward slashes)"
            ));
        };
        let source = project_root.join(&rel);
        if std::fs::symlink_metadata(&source).is_err() {
            return Err(format!("`{rel}` is not in {}", project_root.display()));
        }
        entries.push(rel);
    }

    let home = library_root.join(name);
    let dir = home.join(version);
    if dir.exists() {
        return Err(format!(
            "{name}@{version} is already in the library; a version is written once — package a new one"
        ));
    }

    let copied = (|| -> std::io::Result<usize> {
        let mut count = 0;
        for rel in &entries {
            copy_into(&project_root.join(rel), &dir.join(rel), &mut count)?;
        }
        std::fs::write(dir.join(workflows::MANIFEST), &text)?;
        Ok(count)
    })();
    let files = match copied {
        Ok(files) => files,
        Err(error) => {
            let _ = std::fs::remove_dir_all(&dir);
            return Err(format!("copying into {}: {error}", dir.display()));
        }
    };
    let hash = workflows::digest_of(
        &workflows::file_hashes(&dir).map_err(|error| format!("{}: {error}", dir.display()))?,
    );
    let commit = commit_version(&home, name, version).await;
    Ok(Packaged {
        dir,
        files,
        hash,
        commit,
    })
}

/// Run git in `dir` through the daemon's own runner (`worktree::git`, which disables the
/// repository-named command strings a hostile config could smuggle in).
async fn git(dir: &Path, args: &[&str]) -> Result<String, String> {
    let output = crate::worktree::git()
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .await
        .map_err(|error| format!("git could not be run: {error}"))?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        Err(format!(
            "git {} failed: {}",
            args.first().copied().unwrap_or(""),
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}

/// Record `version` as one commit in `home`, initialising the repository the first time.
///
/// Only `<version>/` is staged, so anything else a person keeps under the workflow's folder stays
/// theirs to commit. An identity is supplied only when git has none configured, so a person's own
/// name goes on the history when there is one and a bare machine still gets a history.
async fn commit_version(home: &Path, name: &str, version: &str) -> Result<String, String> {
    if !home.join(".git").exists() {
        git(home, &["init", "--quiet"]).await?;
    }
    git(home, &["add", "--", version]).await?;
    let has_identity = git(home, &["config", "user.email"])
        .await
        .is_ok_and(|email| !email.is_empty());
    let message = format!("{name} {version}");
    let mut args: Vec<&str> = Vec::new();
    if !has_identity {
        args.extend([
            "-c",
            "user.name=NucleOS",
            "-c",
            "user.email=nucleos@localhost",
        ]);
    }
    args.extend(["commit", "--quiet", "-m", &message, "--", version]);
    git(home, &args).await?;
    git(home, &["rev-parse", "HEAD"]).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git_available() -> bool {
        std::process::Command::new("git")
            .arg("--version")
            .output()
            .is_ok_and(|output| output.status.success())
    }

    fn project() -> tempfile::TempDir {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("project");
        for (path, contents) in [
            (".ai/workflow/workflow.md", "the pipeline"),
            (".ai/workflow/dispatch.md", "dispatch"),
            (".ai/scripts/select_tests.py", "print()"),
            (".ai/scripts/private.py", "not the workflow's"),
            (".ai/memory.md", "this project's own"),
        ] {
            let target = root.join(path);
            std::fs::create_dir_all(target.parent().unwrap()).unwrap();
            std::fs::write(target, contents).unwrap();
        }
        std::fs::write(
            temp.path().join("bundle.yaml"),
            "# the dev workflow\ndescription: rules\nowns:\n  - .ai/workflow/\n  - .ai/scripts/select_tests.py\n",
        )
        .unwrap();
        temp
    }

    #[tokio::test]
    async fn packaging_copies_what_the_manifest_owns_and_nothing_else() {
        let temp = project();
        let library = temp.path().join("lib");
        let packaged = package(
            &library,
            "dev",
            "1.0.0",
            &temp.path().join("project"),
            &temp.path().join("bundle.yaml"),
        )
        .await
        .unwrap();

        let dir = library.join("dev").join("1.0.0");
        assert_eq!(packaged.dir, dir);
        assert_eq!(packaged.files, 3);
        assert!(dir.join(".ai/workflow/workflow.md").is_file());
        assert!(dir.join(".ai/scripts/select_tests.py").is_file());
        assert!(!dir.join(".ai/scripts/private.py").exists());
        assert!(!dir.join(".ai/memory.md").exists());
        // The manifest as it was written, comments included.
        assert!(
            std::fs::read_to_string(dir.join(workflows::MANIFEST))
                .unwrap()
                .starts_with("# the dev workflow")
        );
        // The library reads it back as a bundle, with the hash a pin would record.
        let bundle = workflows::read_bundle(&dir, "dev", "1.0.0")
            .unwrap()
            .unwrap();
        assert_eq!(bundle.hash, packaged.hash);
        assert_eq!(bundle.owns.len(), 2);

        if git_available() {
            let commit = packaged
                .commit
                .expect("git is here, so the version is committed");
            assert_eq!(commit.len(), 40);
            assert!(library.join("dev").join(".git").exists());
        }
    }

    #[tokio::test]
    async fn a_version_is_written_once() {
        let temp = project();
        let library = temp.path().join("lib");
        let root = temp.path().join("project");
        let manifest = temp.path().join("bundle.yaml");
        package(&library, "dev", "1.0.0", &root, &manifest)
            .await
            .unwrap();
        std::fs::write(root.join(".ai/workflow/workflow.md"), "changed").unwrap();

        let refused = package(&library, "dev", "1.0.0", &root, &manifest)
            .await
            .unwrap_err();
        assert!(refused.contains("already in the library"), "{refused}");
        assert_eq!(
            std::fs::read_to_string(library.join("dev/1.0.0/.ai/workflow/workflow.md")).unwrap(),
            "the pipeline"
        );

        // A second version is a second commit on the same history.
        let second = package(&library, "dev", "1.1.0", &root, &manifest)
            .await
            .unwrap();
        if git_available() {
            let log = git(&library.join("dev"), &["log", "--format=%s"])
                .await
                .unwrap();
            assert_eq!(log.lines().collect::<Vec<_>>(), ["dev 1.1.0", "dev 1.0.0"]);
            assert!(second.commit.is_ok());
        }
    }

    #[tokio::test]
    async fn f5_without_a_usable_history_the_bundle_still_lands_and_says_why() {
        let temp = project();
        let library = temp.path().join("lib");
        let home = library.join("dev");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(home.join(".git"), "not a git directory").unwrap();

        let packaged = package(
            &library,
            "dev",
            "1.0.0",
            &temp.path().join("project"),
            &temp.path().join("bundle.yaml"),
        )
        .await
        .unwrap();

        assert!(packaged.dir.is_dir());
        assert!(packaged.dir.join(".ai/workflow/workflow.md").is_file());
        assert!(packaged.commit.is_err());
        assert!(packaged.commit.unwrap_err().contains("git"));
    }

    #[tokio::test]
    async fn a_manifest_naming_a_path_outside_the_project_or_not_in_it_writes_nothing() {
        let temp = project();
        let library = temp.path().join("lib");
        let root = temp.path().join("project");
        for owns in ["../elsewhere", "/etc", ".ai/nope/"] {
            let manifest = temp.path().join("bad.yaml");
            std::fs::write(&manifest, format!("owns:\n  - {owns}\n")).unwrap();
            assert!(
                package(&library, "dev", "1.0.0", &root, &manifest)
                    .await
                    .is_err(),
                "{owns}"
            );
        }
        std::fs::write(temp.path().join("empty.yaml"), "owns: []\n").unwrap();
        assert!(
            package(
                &library,
                "dev",
                "1.0.0",
                &root,
                &temp.path().join("empty.yaml")
            )
            .await
            .is_err()
        );
        assert!(!library.exists());
        assert!(
            package(
                &library,
                "../x",
                "1.0.0",
                &root,
                &temp.path().join("bundle.yaml")
            )
            .await
            .is_err()
        );
    }
}
