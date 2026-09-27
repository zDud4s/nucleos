//! Where NucleOS keeps what it knows about ONE project: `~/.nucleos/projects/<project_id>/`.
//!
//! [`crate::machine_config`] moved this machine's settings out of the daemon's working directory
//! and into `~/.nucleos/`. This module is the same move for the two files the núcleo keeps per
//! project — the autopilot rules and the workflow pins — which used to sit in the project's own
//! `.ai/` folder, and for the marker that says the project was onboarded (`crate::onboarding`).
//!
//! # Why not the project's `.ai/`
//!
//! `.ai/` belongs to the AI dev workflow, not to this product. A project that does not use that
//! workflow has no reason to grow one, and a project that does has its own files there that this
//! app must not be confused with — the same confusion `ownership.rs`'s header describes for
//! `.ai/models.yaml`.
//!
//! # Why not a `.nucleos/` folder inside the project either
//!
//! A worktree carries only TRACKED files. Gitignored, a project-local folder would be missing in
//! every `job-*`, `run-*` and `integration-*` tree this daemon opens; tracked, it would put this
//! app's configuration into somebody's repository history. Keyed by `project_id` under the home
//! directory, the file is found the same way from the main checkout and from any of its worktrees,
//! and it survives the project's folder being moved: `project_id` is the roster's primary key in
//! `autopilot_state`, and moving a folder only rewrites that row's `project_root`.
//!
//! # What the files are
//!
//! YAML, with exactly the contents they always had. Only the location changed. The parsers are
//! still `config::parse_schedule_rules` and `workflows::parse_pins`, and the write door in
//! `http.rs` still holds a candidate to them before a byte is written.
//!
//! Ejected workflow copies (`workflows::EJECTED_DIR`) are NOT here and never will be: an ejected
//! copy is the project's own files by definition, and it stays in the project.

use std::path::{Path, PathBuf};

/// The directory under [`crate::machine_config::root`] that holds one directory per project.
const PROJECTS_DIR: &str = "projects";

/// What a project does on its own: schedules, repository triggers, and the gate command.
/// Parsed by [`crate::config::parse_schedule_rules`].
pub const AUTOPILOT_FILE: &str = "autopilot.yaml";

/// Which workflows a project uses, at which version, and what it overrides on their nodes.
/// Parsed by [`crate::workflows::parse_pins`].
pub const PINS_FILE: &str = "workflows.yaml";

/// That a person onboarded this project to NucleOS, when, and what they confirmed. Written only by
/// [`crate::onboarding`] — the onboarding route and its one-time migration — and never copied from
/// anywhere: it has no older home, because it records a decision this app did not ask for before.
pub const ONBOARDED_FILE: &str = "onboarded.yaml";

/// Every file this module places, in the order [`migrate_legacy`] copies them.
pub const FILES: &[&str] = &[AUTOPILOT_FILE, PINS_FILE];

/// Every file a project's state directory holds: [`FILES`] plus the onboarding marker. What the
/// classifier guards by name, because each of them decides something about what a run may do.
pub const ALL_FILES: &[&str] = &[AUTOPILOT_FILE, PINS_FILE, ONBOARDED_FILE];

/// Where every row of [`FILES`] used to live, relative to the project's root.
const LEGACY_DIR: &str = ".ai";

/// Whether `project_id` can name a directory without meaning something else.
///
/// A project id is whatever somebody typed when they added the project — `autopilot_state` has no
/// CHECK on it and the route that writes it has never validated it. Joined onto a path, an id with
/// a separator, a `..`, a drive letter or a Windows-reserved character would name a different
/// directory from the one it reads as, so an id outside this set simply has no state directory:
/// its rules read as absent and the write door refuses it. Every id the shell has ever suggested is
/// inside the set.
///
/// Wider than [`crate::workflows::valid_name`] on purpose — a space is a legitimate character in a
/// project's name and is harmless in a directory name — and narrower than the filesystem, because
/// the filesystem's own rules differ between the two platforms this runs on.
pub fn valid_id(project_id: &str) -> bool {
    !project_id.is_empty()
        && project_id.len() <= 128
        && project_id != "."
        && project_id != ".."
        // Windows drops a trailing dot or space when it creates a directory, so `alpha.` and
        // `alpha` would be one directory under two ids.
        && !project_id.ends_with('.')
        && !project_id.ends_with(' ')
        && !project_id.starts_with(' ')
        && project_id.chars().all(|c| {
            !c.is_control() && !matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|')
        })
}

/// One project's state directory under `machine_root`, or `None` for an id that cannot name one.
///
/// `machine_root` is an argument and never looked up here, for the reason
/// `AppState::machine_config_root` gives: it is what lets a test point this somewhere that is not a
/// real home directory.
pub fn dir(machine_root: &Path, project_id: &str) -> Option<PathBuf> {
    valid_id(project_id).then(|| machine_root.join(PROJECTS_DIR).join(project_id))
}

/// One file of one project's state, or `None` when there is no root or the id cannot name a
/// directory. Callers read `None` as "the file is absent", which is what it has always meant for a
/// project that has none.
pub fn file(machine_root: Option<&Path>, project_id: &str, name: &str) -> Option<PathBuf> {
    machine_root
        .and_then(|root| dir(root, project_id))
        .map(|dir| dir.join(name))
}

/// `name` for `project_id` as a person is shown it: `~/.nucleos/projects/<id>/<name>`.
///
/// Never the absolute path, for the reason [`crate::machine_config::ROOT_DISPLAY`] gives: a
/// sentence somebody can be told on any machine without it carrying a username.
pub fn display_path(project_id: &str, name: &str) -> String {
    format!(
        "{}/{PROJECTS_DIR}/{project_id}/{name}",
        crate::machine_config::ROOT_DISPLAY
    )
}

/// Write through a temporary file in the same directory, then rename over the target.
///
/// Every writer of a state file goes through this: the write door (`http.rs`) and onboarding.
///
/// A plain truncate-and-write leaves the rules file half-written if anything goes wrong mid-write,
/// and a half-written `autopilot.yaml` is not a smaller file — it is an *unreadable* one, which
/// `gate.rs` reports as `gate errored` on every completed run from then on. Rename is atomic on both
/// platforms and replaces an existing file on both, so the file is either wholly the old one or
/// wholly the new one.
///
/// The temporary lives beside the target because rename is only atomic within a filesystem. Two
/// writes racing would collide on it; they would be writing the same class of content to the same
/// file, and the loser is a request the caller is watching.
pub fn write_atomically(target: &std::path::Path, contents: &str) -> std::io::Result<()> {
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let temp = target.with_extension("nucleos-tmp");
    std::fs::write(&temp, contents)?;
    std::fs::rename(&temp, target)
}

/* --------------------------------------------------------------- migration -- */

/// Copies each project's state files from its old `<project root>/.ai/` into its directory under
/// `machine_root`, once, and never over one that is already there. Returns `(project_id, file)`
/// for every file it copied.
///
/// The same three rules as [`crate::machine_config::migrate_legacy`], and for the same reasons:
///
/// - **Copy, never move.** The old file may be tracked in the project's repository, and a daemon
///   that deleted a file out of somebody's working tree at startup would be changing a repository
///   nobody asked it to change. After the copy the old one is simply no longer read.
/// - **Never overwrite.** A file already in the state directory is the project's current state,
///   and the copy is opened with `create_new` so that holds even against a writer racing this one.
/// - **Failures are logged and skipped, file by file.** A project whose rules could not be copied
///   behaves as a project with no rules, which is what an absent file has always meant, and it must
///   not stop the daemon from starting.
///
/// Both directories are arguments, and `projects` is the roster as `(project_id, project_root)`,
/// so a test can point all of it at temporary directories rather than at a real home.
pub fn migrate_legacy(
    machine_root: &Path,
    projects: &[(String, PathBuf)],
) -> Vec<(String, &'static str)> {
    let mut copied = Vec::new();
    for (project_id, project_root) in projects {
        let Some(target_dir) = dir(machine_root, project_id) else {
            if FILES
                .iter()
                .any(|name| project_root.join(LEGACY_DIR).join(name).is_file())
            {
                tracing::warn!(
                    project_id = %project_id,
                    "this project's id cannot name a directory, so its .ai/ state files were not \
                     copied and it runs as though it had none"
                );
            }
            continue;
        };
        for name in FILES {
            let target = target_dir.join(name);
            let source = project_root.join(LEGACY_DIR).join(name);
            if target.exists() || !source.is_file() {
                continue;
            }
            let copy = || -> std::io::Result<()> {
                let contents = std::fs::read(&source)?;
                std::fs::create_dir_all(&target_dir)?;
                let mut file = std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&target)?;
                std::io::Write::write_all(&mut file, &contents)
            };
            match copy() {
                Ok(()) => {
                    tracing::info!(
                        project_id = %project_id,
                        file = name,
                        to = %display_path(project_id, name),
                        "copied a project state file from the project's .ai/; the old file was left \
                         where it was and is no longer read"
                    );
                    copied.push((project_id.clone(), *name));
                }
                Err(error) => tracing::warn!(
                    %error,
                    project_id = %project_id,
                    file = name,
                    "could not copy a project state file from the project's .ai/; the project runs \
                     as though it had none"
                ),
            }
        }
    }
    copied
}

/// Writes `contents` as `name` in `project_id`'s state directory under `machine_root`, creating
/// the directories on the way. For tests, which point `machine_root` at a temporary directory
/// standing in for `~/.nucleos` — never at a real home.
#[cfg(test)]
pub(crate) fn write_for_test(
    machine_root: &Path,
    project_id: &str,
    name: &str,
    contents: &str,
) -> PathBuf {
    let path = file(Some(machine_root), project_id, name).expect("a test id names a directory");
    std::fs::create_dir_all(path.parent().expect("a state file sits in a directory"))
        .expect("create the project's state directory");
    std::fs::write(&path, contents).expect("write the project's state file");
    path
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, contents: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contents).unwrap();
    }

    /// The ids that would name some other directory have no state directory at all.
    #[test]
    fn an_id_that_would_name_another_directory_has_no_state_directory() {
        let root = Path::new("/home/me/.nucleos");
        for id in [
            "",
            ".",
            "..",
            "a/b",
            "a\\b",
            "c:",
            "a:b",
            "x*",
            "trailing.",
            "trailing ",
            " leading",
            "tab\there",
        ] {
            assert!(dir(root, id).is_none(), "{id:?} must not name a directory");
        }
        for id in ["alpha", "my project", "nucleos-2", "Proj_3.v2"] {
            assert_eq!(
                dir(root, id),
                Some(root.join("projects").join(id)),
                "{id:?} is an ordinary id"
            );
        }
    }

    #[test]
    fn a_file_is_shown_under_the_home_spelling() {
        assert_eq!(
            display_path("alpha", AUTOPILOT_FILE),
            "~/.nucleos/projects/alpha/autopilot.yaml"
        );
    }

    /// No root, or an id that cannot name a directory, is "absent" — never a guess at another place.
    #[test]
    fn no_root_or_a_bad_id_is_no_file() {
        assert!(file(None, "alpha", AUTOPILOT_FILE).is_none());
        assert!(file(Some(Path::new("/r")), "../escape", AUTOPILOT_FILE).is_none());
        assert_eq!(
            file(Some(Path::new("/r")), "alpha", PINS_FILE),
            Some(
                Path::new("/r")
                    .join("projects")
                    .join("alpha")
                    .join("workflows.yaml")
            )
        );
    }

    /// The whole contract of the migration, on injected directories: absent here and present there
    /// is copied; present here is never overwritten; the source is never removed.
    #[test]
    fn the_old_files_are_copied_once_and_never_over_a_newer_one() {
        let home = tempfile::tempdir().unwrap();
        let alpha = tempfile::tempdir().unwrap();
        let beta = tempfile::tempdir().unwrap();
        write(
            &alpha.path().join(".ai").join(AUTOPILOT_FILE),
            "gate_command: old\n",
        );
        write(&alpha.path().join(".ai").join(PINS_FILE), "workflows: []\n");
        write(
            &beta.path().join(".ai").join(AUTOPILOT_FILE),
            "gate_command: stale\n",
        );
        // Beta already has current rules in its state directory.
        let beta_rules = home
            .path()
            .join("projects")
            .join("beta")
            .join(AUTOPILOT_FILE);
        write(&beta_rules, "gate_command: current\n");

        let roster = vec![
            ("alpha".to_string(), alpha.path().to_path_buf()),
            ("beta".to_string(), beta.path().to_path_buf()),
        ];
        let copied = migrate_legacy(home.path(), &roster);

        assert_eq!(
            copied,
            vec![
                ("alpha".to_string(), AUTOPILOT_FILE),
                ("alpha".to_string(), PINS_FILE),
            ]
        );
        let alpha_dir = home.path().join("projects").join("alpha");
        assert_eq!(
            std::fs::read_to_string(alpha_dir.join(AUTOPILOT_FILE)).unwrap(),
            "gate_command: old\n"
        );
        assert_eq!(
            std::fs::read_to_string(&beta_rules).unwrap(),
            "gate_command: current\n",
            "a file already in the state directory is never overwritten"
        );
        assert!(
            alpha.path().join(".ai").join(AUTOPILOT_FILE).is_file()
                && beta.path().join(".ai").join(AUTOPILOT_FILE).is_file(),
            "the old files are copied, never moved"
        );

        // A second start finds everything in place and copies nothing.
        assert!(migrate_legacy(home.path(), &roster).is_empty());
    }

    /// A project with nothing to copy leaves nothing behind — not even an empty directory.
    #[test]
    fn a_project_with_no_old_files_creates_nothing() {
        let home = tempfile::tempdir().unwrap();
        let bare = tempfile::tempdir().unwrap();
        let copied = migrate_legacy(
            home.path(),
            &[("bare".to_string(), bare.path().to_path_buf())],
        );
        assert!(copied.is_empty());
        assert!(!home.path().join("projects").exists());
    }

    /// An id that cannot name a directory is skipped, and nothing is written anywhere for it.
    #[test]
    fn an_id_that_cannot_name_a_directory_is_not_migrated() {
        let home = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        write(
            &project.path().join(".ai").join(AUTOPILOT_FILE),
            "gate_command: x\n",
        );
        let copied = migrate_legacy(
            home.path(),
            &[("../escape".to_string(), project.path().to_path_buf())],
        );
        assert!(copied.is_empty());
        assert!(std::fs::read_dir(home.path()).unwrap().next().is_none());
    }
}
