//! Whether a project was onboarded to NucleOS, and the one act that onboards it.
//!
//! # Why a marker, and not `.ai/workflow/workflow.md`
//!
//! Autopilot activation used to require the AI dev workflow's own file inside the project: a
//! project "was onboarded" when it had adopted that workflow. That tied a harness to one way of
//! developing. NucleOS sits a layer above however a project is developed, and whether the project
//! uses the `.ai` workflow is none of its business. What activation actually needs to know is that
//! a PERSON brought the project in: looked at what is in it, confirmed which command decides
//! *green*, and had the classifier hook installed. That is a decision, so it is recorded, as
//! [`MARKER_FILE`] in the project's state directory (`project_state.rs`), keyed by `project_id` and
//! found the same way from every worktree of the project.
//!
//! # What onboarding does, in order
//!
//! 1. **Checks the confirmed gate command before touching anything.** A command that could not be
//!    stored, or a rules file this cannot edit, is refused while nothing has been written yet.
//! 2. **Detects** the harnesses already in the folder (`detect.rs`), to record what was there.
//! 3. **Installs the classifier hook** with the same `autopilot::wire_classifier_hook` every other
//!    door uses, so there is one hook and one way of wiring it.
//! 4. **Stores the confirmed gate** as `gate_command` in `autopilot.yaml` — the one place the gate
//!    already lives, beside the schedules and triggers, whose lines are all kept.
//! 5. **Writes the marker**, last, so it never claims a step that did not happen.
//!
//! Idempotent: a second onboarding re-wires a hook that is already there (a no-op), sets the same
//! gate line again, and rewrites the marker with the new time.
//!
//! # What the marker is not
//!
//! Configuration. Nothing reads it but the activation check and the page that shows it; the gate
//! is `autopilot.yaml`'s, and editing `gate_command` there later does not make the marker wrong —
//! the marker records what was confirmed WHEN the project was onboarded.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The marker's name, in the project's state directory. Named once, in `project_state.rs`.
pub const MARKER_FILE: &str = crate::project_state::ONBOARDED_FILE;

/// What "onboarded" meant before the marker existed, relative to the project root. Read by
/// [`migrate_legacy`] and nothing else, so no project that was activatable yesterday stops being so.
const LEGACY_MARKER: &str = ".ai/workflow/workflow.md";

/// The feed line both writers of a marker leave in the project's feed: the route when a person
/// onboards it, and the startup migration when it passed the old check.
pub const FEED_KIND: &str = "project_onboarded";

/// The longest gate command this stores. A gate is one command line; anything longer is a paste
/// that went wrong, and a rules file is not the place to find out.
pub const MAX_GATE_COMMAND: usize = 1024;

/// One project's onboarding, as it was recorded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Marker {
    /// RFC 3339, UTC.
    pub onboarded_at: String,
    /// The folder that was onboarded, as it was given. Informational: the project may move, and
    /// its state directory — and so this marker — moves with its id, not with its folder.
    pub project_root: String,
    /// The gate command a person confirmed, or `None` when none was. Not the gate itself, which is
    /// `autopilot.yaml`'s; see the module header.
    #[serde(default)]
    pub gate_command: Option<String>,
    /// The harness folders detection found (`.ai`, `.claude`, ...), relative to the root.
    #[serde(default)]
    pub harnesses: Vec<String>,
    /// Whether onboarding installed the classifier hook. `false` on a migrated marker: nothing was
    /// installed, the project merely passed the old check.
    #[serde(default)]
    pub hook_installed: bool,
    /// Written by [`migrate_legacy`] for a project that passed the old `.ai/workflow/workflow.md`
    /// check, rather than by a person onboarding it.
    #[serde(default)]
    pub migrated: bool,
}

/// Why onboarding stopped. Every variant but `Io` is raised before anything is written.
#[derive(Debug)]
pub enum OnboardError {
    /// The project id cannot name a state directory (`project_state::valid_id`).
    BadId,
    /// The confirmed gate command cannot be stored as one.
    Gate(String),
    /// The project's existing `autopilot.yaml` cannot be read or edited without losing something.
    Rules(String),
    /// The classifier hook could not be installed — usually a `.claude/settings.json` this daemon
    /// cannot parse, which it leaves alone.
    Hook(String),
    Io(std::io::Error),
}

impl std::fmt::Display for OnboardError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BadId => write!(formatter, "the project id cannot name a state directory"),
            Self::Gate(detail) | Self::Rules(detail) | Self::Hook(detail) => {
                write!(formatter, "{detail}")
            }
            Self::Io(error) => write!(formatter, "{error}"),
        }
    }
}

/// The marker for `project_id`, or `None` when there is none, or none this can read.
///
/// An unreadable marker is "not onboarded" rather than an error: onboarding again is idempotent and
/// rewrites it, which is the one repair anybody needs.
pub fn read_marker(machine_root: Option<&Path>, project_id: &str) -> Option<Marker> {
    let path = crate::project_state::file(machine_root, project_id, MARKER_FILE)?;
    let text = std::fs::read_to_string(path).ok()?;
    serde_yaml::from_str(&text).ok()
}

/// Whether `project_id` was onboarded. The activation prerequisite (`autopilot.rs`).
pub fn is_onboarded(machine_root: Option<&Path>, project_id: &str) -> bool {
    read_marker(machine_root, project_id).is_some()
}

/// The marker as the file holds it: a one-line header saying what it is, then the YAML.
fn render(marker: &Marker) -> std::io::Result<String> {
    let body = serde_yaml::to_string(marker)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
    Ok(format!(
        "# Written by NucleOS: this project was onboarded, and what was confirmed then. The gate \
         itself is gate_command in autopilot.yaml beside this file.\n{body}"
    ))
}

/// Whether `command` can be stored as a gate: one line, not empty, not absurdly long.
pub fn validate_gate(command: &str) -> Result<(), String> {
    if command.trim().is_empty() {
        return Err("a gate command cannot be empty".to_string());
    }
    if command.contains(['\n', '\r']) {
        return Err(
            "a gate command is one line: it is spawned directly, never through a shell".to_string(),
        );
    }
    if command.len() > MAX_GATE_COMMAND {
        return Err(format!(
            "a gate command is at most {MAX_GATE_COMMAND} bytes"
        ));
    }
    Ok(())
}

/// `existing` rules text with its `gate_command` set to `command`, every other line kept.
///
/// Edited as text rather than parsed and re-serialised, because the file is a person's: a
/// round-trip through a YAML value would drop every comment in it. A top-level `gate_command:` line
/// (and any indented continuation under it) is taken out and the new one appended; the result is
/// then held to [`crate::config::parse_schedule_rules`] and must read back as exactly `command`,
/// so an edit this got wrong is refused rather than written.
pub fn with_gate_command(existing: &str, command: &str) -> Result<String, String> {
    let mut kept = Vec::new();
    let mut in_gate = false;
    for line in existing.lines() {
        if in_gate && line.starts_with([' ', '\t']) && !line.trim().is_empty() {
            continue;
        }
        in_gate = line.starts_with("gate_command:");
        if !in_gate {
            kept.push(line);
        }
    }

    // `serde_yaml` decides the quoting: a command that opens with a quote, as Git's bash by its
    // absolute path does, is not a plain scalar.
    let scalar = serde_yaml::to_string(command)
        .map_err(|error| format!("the gate command cannot be written as YAML: {error}"))?;
    let mut text = kept.join("\n");
    if !text.is_empty() {
        text.push('\n');
    }
    text.push_str("gate_command: ");
    text.push_str(scalar.trim_end());
    text.push('\n');

    let rules = crate::config::parse_schedule_rules(&text).map_err(|error| {
        format!("setting the gate would leave autopilot.yaml unreadable: {error}")
    })?;
    if rules.gate_command.as_deref() != Some(command) {
        return Err(
            "autopilot.yaml would not read back the confirmed gate command; it was left alone"
                .to_string(),
        );
    }
    Ok(text)
}

/// Onboards `project_id`, rooted at `project_root`, with the gate command a person confirmed.
///
/// `gate_command: None` means nobody confirmed one, and leaves `autopilot.yaml` exactly as it was —
/// it never removes a gate somebody set elsewhere. `now` is the timestamp to record, an argument so
/// a test can pin it. See the module header for the order, which is the design.
pub fn onboard(
    machine_root: &Path,
    project_id: &str,
    project_root: &Path,
    gate_command: Option<&str>,
    now: &str,
) -> Result<Marker, OnboardError> {
    let dir = crate::project_state::dir(machine_root, project_id).ok_or(OnboardError::BadId)?;
    let rules_path = dir.join(crate::project_state::AUTOPILOT_FILE);

    let rules = match gate_command {
        Some(command) => {
            validate_gate(command).map_err(OnboardError::Gate)?;
            let existing = match std::fs::read_to_string(&rules_path) {
                Ok(text) => text,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
                Err(error) => return Err(OnboardError::Io(error)),
            };
            crate::config::parse_schedule_rules(&existing).map_err(|error| {
                OnboardError::Rules(format!(
                    "{} is unreadable ({error}); fix it before onboarding sets a gate in it",
                    crate::project_state::display_path(
                        project_id,
                        crate::project_state::AUTOPILOT_FILE
                    )
                ))
            })?;
            Some(with_gate_command(&existing, command).map_err(OnboardError::Rules)?)
        }
        None => None,
    };

    let harnesses = crate::detect::inspect_folder(project_root)
        .harnesses
        .into_iter()
        .map(|harness| harness.path)
        .collect();

    crate::autopilot::wire_classifier_hook(project_root).map_err(OnboardError::Hook)?;

    if let Some(text) = rules {
        crate::project_state::write_atomically(&rules_path, &text).map_err(OnboardError::Io)?;
    }

    let marker = Marker {
        onboarded_at: now.to_string(),
        project_root: project_root.to_string_lossy().into_owned(),
        gate_command: gate_command.map(str::to_string),
        harnesses,
        hook_installed: true,
        migrated: false,
    };
    crate::project_state::write_atomically(
        &dir.join(MARKER_FILE),
        &render(&marker).map_err(OnboardError::Io)?,
    )
    .map_err(OnboardError::Io)?;
    Ok(marker)
}

/* --------------------------------------------------------------- migration -- */

/// Writes a `migrated: true` marker for every rostered project that passes the OLD check — it has
/// `.ai/workflow/workflow.md` — and has no marker yet. Returns the ids it wrote one for.
///
/// So that nobody loses autopilot on update: a project that could be activated yesterday can be
/// today, without anybody having to onboard it again. The rules are [`crate::project_state::
/// migrate_legacy`]'s: never over an existing marker (opened `create_new`, so that holds even
/// against the route racing this), the old file is only read, and a failure is logged and skipped,
/// project by project — such a project simply reads as not onboarded until somebody onboards it.
///
/// Both directories are arguments and `projects` is the roster as `(project_id, project_root)`, so
/// a test points all of it at temporary directories.
pub fn migrate_legacy(
    machine_root: &Path,
    projects: &[(String, PathBuf)],
    now: &str,
) -> Vec<String> {
    let mut written = Vec::new();
    for (project_id, project_root) in projects {
        if !project_root.join(LEGACY_MARKER).is_file() {
            continue;
        }
        let Some(dir) = crate::project_state::dir(machine_root, project_id) else {
            tracing::warn!(
                project_id = %project_id,
                "this project's id cannot name a directory, so it was not marked onboarded and \
                 cannot be activated until its id is changed"
            );
            continue;
        };
        let target = dir.join(MARKER_FILE);
        if target.exists() {
            continue;
        }
        let marker = Marker {
            onboarded_at: now.to_string(),
            project_root: project_root.to_string_lossy().into_owned(),
            gate_command: None,
            harnesses: Vec::new(),
            hook_installed: false,
            migrated: true,
        };
        let write = || -> std::io::Result<()> {
            let text = render(&marker)?;
            std::fs::create_dir_all(&dir)?;
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&target)?;
            std::io::Write::write_all(&mut file, text.as_bytes())
        };
        match write() {
            Ok(()) => {
                tracing::info!(
                    project_id = %project_id,
                    to = %crate::project_state::display_path(project_id, MARKER_FILE),
                    "marked a project onboarded because it had .ai/workflow/workflow.md, which is \
                     what onboarded meant before the marker"
                );
                written.push(project_id.clone());
            }
            Err(error) => tracing::warn!(
                %error,
                project_id = %project_id,
                "could not mark a project onboarded; it cannot be activated until it is onboarded"
            ),
        }
    }
    written
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: &str = "2026-09-27T12:00:00+00:00";

    fn write(path: &Path, contents: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contents).unwrap();
    }

    fn rules_path(home: &Path, id: &str) -> PathBuf {
        home.join("projects")
            .join(id)
            .join(crate::project_state::AUTOPILOT_FILE)
    }

    /// No marker, or one that is not a marker, is "not onboarded" — never an error.
    #[test]
    fn a_missing_or_unreadable_marker_is_not_onboarded() {
        let home = tempfile::tempdir().unwrap();
        assert!(!is_onboarded(Some(home.path()), "alpha"));
        assert!(!is_onboarded(None, "alpha"));
        crate::project_state::write_for_test(home.path(), "alpha", MARKER_FILE, "[not a marker");
        assert!(!is_onboarded(Some(home.path()), "alpha"));
    }

    #[test]
    fn a_gate_line_is_set_and_every_other_line_is_kept() {
        let existing = "# the owner's note\nschedules: []\ngate_command: old\nrepo_triggers: []\n";
        let text = with_gate_command(existing, "cargo test").unwrap();
        assert!(text.contains("# the owner's note"), "{text}");
        assert!(text.contains("schedules: []") && text.contains("repo_triggers: []"));
        assert!(!text.contains("gate_command: old"), "{text}");
        assert_eq!(
            crate::config::parse_schedule_rules(&text)
                .unwrap()
                .gate_command
                .as_deref(),
            Some("cargo test")
        );

        // From nothing, and with a command that has to be quoted to survive as YAML.
        let quoted = r#""C:/Program Files/Git/bin/bash.exe" scripts/gates.sh core"#;
        let text = with_gate_command("", quoted).unwrap();
        assert_eq!(
            crate::config::parse_schedule_rules(&text)
                .unwrap()
                .gate_command
                .as_deref(),
            Some(quoted)
        );

        // A folded gate spread over indented lines is taken out whole, not left dangling.
        let folded = "gate_command: >\n  cargo\n  test\nschedules: []\n";
        let text = with_gate_command(folded, "make check").unwrap();
        assert_eq!(text, "schedules: []\ngate_command: make check\n");
    }

    #[test]
    fn a_gate_that_cannot_be_one_is_refused() {
        assert!(validate_gate("").is_err());
        assert!(validate_gate("  ").is_err());
        assert!(validate_gate("cargo test\nrm -rf /").is_err());
        assert!(validate_gate(&"x".repeat(MAX_GATE_COMMAND + 1)).is_err());
        assert!(validate_gate("cargo test").is_ok());
    }

    /// The whole act on injected directories: hook wired, gate stored beside what was there, the
    /// marker last — and a second run is the same state, not a second copy of anything.
    #[test]
    fn onboarding_installs_the_hook_stores_the_gate_and_writes_the_marker() {
        let home = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(project.path().join(".claude")).unwrap();
        write(&rules_path(home.path(), "alpha"), "schedules: []\n");

        let marker = onboard(
            home.path(),
            "alpha",
            project.path(),
            Some("cargo test"),
            NOW,
        )
        .unwrap();

        assert!(crate::autopilot::classifier_hook_is_wired(project.path()));
        assert_eq!(marker.harnesses, vec![".claude".to_string()]);
        assert!(marker.hook_installed && !marker.migrated);
        assert_eq!(read_marker(Some(home.path()), "alpha"), Some(marker));
        let rules = std::fs::read_to_string(rules_path(home.path(), "alpha")).unwrap();
        assert_eq!(rules, "schedules: []\ngate_command: cargo test\n");

        onboard(
            home.path(),
            "alpha",
            project.path(),
            Some("cargo test"),
            NOW,
        )
        .unwrap();
        assert_eq!(
            std::fs::read_to_string(rules_path(home.path(), "alpha")).unwrap(),
            rules,
            "onboarding again is the same state"
        );
        assert!(is_onboarded(Some(home.path()), "alpha"));
    }

    /// Nobody confirmed a gate: the rules file is not touched, and a gate set elsewhere survives.
    #[test]
    fn no_confirmed_gate_leaves_the_rules_alone() {
        let home = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        write(&rules_path(home.path(), "alpha"), "gate_command: make ci\n");

        let marker = onboard(home.path(), "alpha", project.path(), None, NOW).unwrap();

        assert_eq!(marker.gate_command, None);
        assert_eq!(
            std::fs::read_to_string(rules_path(home.path(), "alpha")).unwrap(),
            "gate_command: make ci\n"
        );
        assert!(is_onboarded(Some(home.path()), "alpha"));
    }

    /// A refusal on the gate or the rules happens before the hook is installed or anything written.
    #[test]
    fn a_refused_gate_or_unreadable_rules_writes_nothing() {
        let home = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();

        assert!(matches!(
            onboard(home.path(), "alpha", project.path(), Some("a\nb"), NOW),
            Err(OnboardError::Gate(_))
        ));
        write(&rules_path(home.path(), "alpha"), "schedules: [unclosed\n");
        assert!(matches!(
            onboard(
                home.path(),
                "alpha",
                project.path(),
                Some("cargo test"),
                NOW
            ),
            Err(OnboardError::Rules(_))
        ));
        assert!(matches!(
            onboard(home.path(), "../escape", project.path(), None, NOW),
            Err(OnboardError::BadId)
        ));

        assert!(
            !project.path().join(".claude").exists(),
            "no hook was installed"
        );
        assert!(!is_onboarded(Some(home.path()), "alpha"));
    }

    /// A settings file this daemon cannot parse is the project saying no; nothing is marked.
    #[test]
    fn a_hook_that_cannot_be_installed_leaves_the_project_not_onboarded() {
        let home = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        write(&project.path().join(".claude/settings.json"), "{ not json");

        assert!(matches!(
            onboard(
                home.path(),
                "alpha",
                project.path(),
                Some("cargo test"),
                NOW
            ),
            Err(OnboardError::Hook(_))
        ));
        assert!(!is_onboarded(Some(home.path()), "alpha"));
        assert!(!rules_path(home.path(), "alpha").exists());
    }

    /// The migration's whole contract: old check passed and no marker is a `migrated` marker; an
    /// existing marker is never overwritten; no old file is nothing; a second start writes nothing.
    #[test]
    fn a_project_that_passed_the_old_check_is_marked_once() {
        let home = tempfile::tempdir().unwrap();
        let old = tempfile::tempdir().unwrap();
        let onboarded = tempfile::tempdir().unwrap();
        let plain = tempfile::tempdir().unwrap();
        write(&old.path().join(LEGACY_MARKER), "# Workflow");
        write(&onboarded.path().join(LEGACY_MARKER), "# Workflow");
        let existing = onboard(home.path(), "beta", onboarded.path(), None, "earlier").unwrap();

        let roster = vec![
            ("alpha".to_string(), old.path().to_path_buf()),
            ("beta".to_string(), onboarded.path().to_path_buf()),
            ("gamma".to_string(), plain.path().to_path_buf()),
        ];
        assert_eq!(migrate_legacy(home.path(), &roster, NOW), vec!["alpha"]);

        let alpha = read_marker(Some(home.path()), "alpha").unwrap();
        assert!(alpha.migrated && !alpha.hook_installed);
        assert_eq!(alpha.onboarded_at, NOW);
        assert_eq!(
            read_marker(Some(home.path()), "beta"),
            Some(existing),
            "a marker already there is never overwritten"
        );
        assert!(!is_onboarded(Some(home.path()), "gamma"));
        assert!(
            old.path().join(LEGACY_MARKER).is_file(),
            "the old file is read, never moved"
        );

        assert!(migrate_legacy(home.path(), &roster, NOW).is_empty());
    }

    #[test]
    fn an_id_that_cannot_name_a_directory_is_not_migrated() {
        let home = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        write(&project.path().join(LEGACY_MARKER), "# Workflow");
        assert!(
            migrate_legacy(
                home.path(),
                &[("../escape".to_string(), project.path().to_path_buf())],
                NOW
            )
            .is_empty()
        );
        assert!(std::fs::read_dir(home.path()).unwrap().next().is_none());
    }
}
