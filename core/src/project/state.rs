//! Where NucleOS keeps what it knows about ONE project: `~/.nucleos/projects/<project_id>/`.
//!
//! [`crate::machine_config`] moved this machine's settings out of the daemon's working directory
//! and into `~/.nucleos/`. This module is the same move for the two files the núcleo keeps per
//! project — the autopilot rules and the workflow pins — and for the marker that says the project
//! was onboarded (`crate::onboarding`).
//!
//! # Why not a folder of the project's AI workflow
//!
//! They used to sit in the project's own `.ai/` folder. That folder belongs to an AI dev workflow,
//! not to this product: a project that does not use one has no reason to grow it, and a project
//! that does has its own files there that this app must not be confused with. NucleOS no longer
//! reads or copies anything from it.
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

/// What `workflows/materialize.rs` last wrote into the project's own checkout, file by file, per
/// bundle. The hash of every file it put there, so that a later run can tell a file it may replace
/// (unchanged since it wrote it) from one a person edited (never replaced). Written only by that
/// module.
pub const MATERIALIZED_FILE: &str = "materialized.yaml";

/// The directory beside [`MATERIALIZED_FILE`] holding one record per WORKTREE of the project, named
/// by `workflow_materialize::worktree_record`. A worktree is a second checkout with its own files,
/// so the edit protection has to be kept per checkout; the main checkout's record is the file
/// above.
pub const WORKTREE_RECORDS_DIR: &str = "materialized";

/// The two files this module places that hold a project's configuration.
pub const FILES: &[&str] = &[AUTOPILOT_FILE, PINS_FILE];

/// Every file a project's state directory holds: [`FILES`] plus the onboarding marker. What the
/// classifier guards by name, because each of them decides something about what a run may do.
///
/// The materialization record is here too: an agent that could rewrite it could make the next
/// materialization overwrite a file a person edited, by recording that edit as the app's own.
pub const ALL_FILES: &[&str] = &[AUTOPILOT_FILE, PINS_FILE, ONBOARDED_FILE, MATERIALIZED_FILE];

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
    write_bytes_atomically(target, contents.as_bytes())
}

/// [`write_atomically`] for bytes, which is what a bundle's files are: `workflows/materialize.rs`
/// copies them into a project unread, and a script or an image is not a `&str`.
///
/// The temporary is the target's name with `.nucleos-tmp` APPENDED rather than swapped for its
/// extension. Swapping is harmless for the state files, which are alone in their directory; a
/// bundle writes `plan.md` and `plan.py` side by side, and both would have raced for one
/// `plan.nucleos-tmp`.
pub fn write_bytes_atomically(target: &std::path::Path, contents: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut name = target.file_name().unwrap_or_default().to_os_string();
    name.push(".nucleos-tmp");
    let temp = target.with_file_name(name);
    std::fs::write(&temp, contents)?;
    std::fs::rename(&temp, target)
}

/// Writes `contents` as `name` in `project_id`'s state directory under `machine_root`, creating
/// the directories on the way. For tests, which point `machine_root` at a temporary directory
/// standing in for `~/.nucleos` — never at a real home.
#[cfg(any(test, feature = "testkit"))]
pub fn write_for_test(
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
}
