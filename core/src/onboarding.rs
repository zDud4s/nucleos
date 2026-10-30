//! Whether a project was onboarded to NucleOS, and the one act that onboards it.
//!
//! # Why a marker, and not a workflow's own file
//!
//! Autopilot activation used to require the AI dev workflow's own file inside the project: a
//! project "was onboarded" when it had adopted that workflow. That tied a harness to one way of
//! developing. NucleOS sits a layer above however a project is developed, and which workflow the
//! project uses is none of its business. What activation actually needs to know is that
//! a PERSON brought the project in: looked at what is in it, confirmed which command decides
//! *green*, and had the classifier hook installed. That is a decision, so it is recorded, as
//! [`MARKER_FILE`] in the project's state directory (`project/state.rs`), keyed by `project_id` and
//! found the same way from every worktree of the project.
//!
//! # What onboarding does, in order
//!
//! 1. **Checks the confirmed gate command before touching anything.** A command that could not be
//!    stored is refused while nothing has been written yet.
//! 2. **Detects** the harnesses already in the folder (`detect.rs`), to record what was there.
//! 3. **Installs the classifier hook** with the same `autopilot::wire_classifier_hook` every other
//!    door uses, so there is one hook and one way of wiring it.
//! 4. **Stores the confirmed gate** as `gate_command` in the project's `autopilot` settings
//!    document (`settings::update`, written by `daemon:onboarding`), whose other rules are kept. A
//!    stored document this cannot decode is refused as [`OnboardError::Rules`].
//! 5. **Writes the marker**, last, so it never claims a step that did not happen.
//!
//! Idempotent: a second onboarding re-wires a hook that is already there (a no-op), sets the same
//! gate line again, and rewrites the marker with the new time.
//!
//! # What the marker is not
//!
//! Configuration. Nothing reads it but the activation check and the page that shows it; the gate
//! is the `autopilot` document's, and editing `gate_command` there later does not make the marker wrong —
//! the marker records what was confirmed WHEN the project was onboarded.

use std::path::Path;

use serde::{Deserialize, Serialize};

/// The marker's name, in the project's state directory. Named once, in `project/state.rs`.
pub const MARKER_FILE: &str = crate::project::state::ONBOARDED_FILE;

/// The feed line the route leaves in the project's feed when a person onboards it.
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
    /// the `autopilot` settings document's; see the module header.
    #[serde(default)]
    pub gate_command: Option<String>,
    /// The harness folders detection found (`.claude`, `.agents`, ...), relative to the root.
    #[serde(default)]
    pub harnesses: Vec<String>,
    /// Whether onboarding installed the classifier hook. `false` on a migrated marker: nothing was
    /// installed, the project merely passed the old check.
    #[serde(default)]
    pub hook_installed: bool,
    /// Set on a marker an earlier daemon's startup migration wrote for a project that passed the
    /// old check, rather than by a person onboarding it. Nothing writes it any more; it is still
    /// read so such a marker keeps parsing.
    #[serde(default)]
    pub migrated: bool,
}

/// Why onboarding stopped. `BadId` and `Gate` are raised before anything is written; `Rules` after the hook
/// is wired (a hook is idempotent) and before the marker.
#[derive(Debug)]
pub enum OnboardError {
    /// The project id cannot name a state directory (`project_state::valid_id`).
    BadId,
    /// The confirmed gate command cannot be stored as one.
    Gate(String),
    /// The project's stored `autopilot` document cannot be decoded or edited.
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
    let path = crate::project::state::file(machine_root, project_id, MARKER_FILE)?;
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
         itself is gate_command in the project's autopilot settings document.\n{body}"
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

/// Onboards `project_id`, rooted at `project_root`, with the gate command a person confirmed.
///
/// `gate_command: None` means nobody confirmed one, and leaves the stored `autopilot` document
/// exactly as it was — it never removes a gate somebody set elsewhere. `now` is the timestamp to
/// record, an argument so a test can pin it. See the module header for the order, which is the
/// design.
pub async fn onboard(
    pool: &sqlx::SqlitePool,
    machine_root: &Path,
    project_id: &str,
    project_root: &Path,
    gate_command: Option<&str>,
    now: &str,
) -> Result<Marker, OnboardError> {
    let dir = crate::project::state::dir(machine_root, project_id).ok_or(OnboardError::BadId)?;

    if let Some(command) = gate_command {
        validate_gate(command).map_err(OnboardError::Gate)?;
    }

    let harnesses = crate::detect::inspect_folder(project_root)
        .harnesses
        .into_iter()
        .map(|harness| harness.path)
        .collect();

    crate::governance::autopilot::wire_classifier_hook(project_root).map_err(OnboardError::Hook)?;

    if let Some(command) = gate_command {
        let command = command.to_string();
        crate::settings::update(
            pool,
            &crate::settings::Scope::project(project_id),
            "daemon:onboarding",
            |rules: &mut crate::config::AutopilotRules| rules.gate_command = Some(command),
        )
        .await
        .map_err(|error| match error {
            crate::settings::SettingsError::Database(error) => {
                OnboardError::Io(std::io::Error::other(error))
            }
            other => OnboardError::Rules(format!(
                "the project's autopilot rules cannot be edited ({other}); fix them before                  onboarding sets a gate in them"
            )),
        })?;
    }

    let marker = Marker {
        onboarded_at: now.to_string(),
        project_root: project_root.to_string_lossy().into_owned(),
        gate_command: gate_command.map(str::to_string),
        harnesses,
        hook_installed: true,
        migrated: false,
    };
    crate::project::state::write_atomically(
        &dir.join(MARKER_FILE),
        &render(&marker).map_err(OnboardError::Io)?,
    )
    .map_err(OnboardError::Io)?;
    Ok(marker)
}

/// The workflow a project brought in for the first time is pinned to, when the library has it.
pub const DEFAULT_WORKFLOW: &str = "nucleos-dev";

/// Pin [`DEFAULT_WORKFLOW`], newest version in `library_root`, for a project being rostered.
///
/// The caller decides that the project is new (`POST /projects/{id}/onboard` asks the roster);
/// this only refuses to overrule a decision already made: a project with a stored `workflow_pins`
/// document has chosen its workflows, even if the choice was none, and is left alone. A library
/// without the bundle is skipped with a log line rather than an error — onboarding is about the hook
/// and the gate, and a machine that never packaged the workflow still onboards. Returns the pinned
/// bundle.
pub async fn pin_default_workflow(
    pool: &sqlx::SqlitePool,
    machine_root: &Path,
    library_root: &Path,
    project_id: &str,
) -> Result<Option<crate::workflows::Bundle>, String> {
    if crate::project::state::dir(machine_root, project_id).is_none() {
        return Err(format!("`{project_id}` cannot name a state directory"));
    }
    if crate::project::rules::is_stored(project_id, "workflow_pins") {
        return Ok(None);
    }
    // Newest first within a name (`workflows::library`), so the first match is the latest.
    let library = library_root.to_path_buf();
    let shelf = tokio::task::spawn_blocking(move || crate::workflows::library(&library))
        .await
        .map_err(|error| error.to_string())?
        .map_err(|error| format!("{}: {error}", library_root.display()))?;
    let newest = shelf
        .into_iter()
        .find(|bundle| bundle.name == DEFAULT_WORKFLOW);
    let Some(bundle) = newest else {
        tracing::info!(
            project_id,
            workflow = DEFAULT_WORKFLOW,
            "the library has no default workflow, so the new project pins none"
        );
        return Ok(None);
    };
    crate::workflows::install(pool, project_id, &bundle)
        .await
        .map_err(|refused| format!("{refused:?}"))?;
    Ok(Some(bundle))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// A library holding `nucleos-dev` at `versions`, under `root/lib`.
    fn library_with(root: &Path, versions: &[&str]) -> PathBuf {
        let library = root.join("lib");
        for version in versions {
            write(
                &library
                    .join(DEFAULT_WORKFLOW)
                    .join(version)
                    .join("bundle.yaml"),
                "owns:\n  - .nucleos/workflow/\n",
            );
        }
        library
    }

    #[tokio::test]
    async fn a_new_project_pins_the_newest_default_workflow() {
        let _settings = crate::project::rules::testing::Home::new();
        let pool = crate::testdb::fresh_pool().await;
        let temp = tempfile::tempdir().unwrap();
        let library = library_with(temp.path(), &["1.9.0", "2.0.0", "2.0.0-rc1"]);
        let home = temp.path().join("home");

        let pinned = pin_default_workflow(&pool, &home, &library, "alpha")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(pinned.version, "2.0.0");
        let pins = crate::project::rules::pins("alpha").unwrap();
        assert_eq!(pins.workflows.len(), 1);
        assert_eq!(pins.workflows[0].name, DEFAULT_WORKFLOW);
        assert_eq!(pins.workflows[0].version, "2.0.0");
        assert_eq!(pins.workflows[0].hash, pinned.hash);
    }

    #[tokio::test]
    async fn a_project_that_chose_its_workflows_or_a_library_without_the_default_pins_nothing() {
        let _settings = crate::project::rules::testing::Home::new();
        let pool = crate::testdb::fresh_pool().await;
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");

        // No such bundle: skipped, not refused, and no pins document appears.
        let empty = library_with(temp.path(), &[]);
        assert!(
            pin_default_workflow(&pool, &home, &empty, "alpha")
                .await
                .unwrap()
                .is_none()
        );
        assert!(!crate::project::rules::is_stored("alpha", "workflow_pins"));

        // A pins document, even an empty one, is a choice already made.
        let library = library_with(temp.path(), &["2.0.0"]);
        crate::project::rules::testing::publish_pins("beta", "workflows: []\n");
        assert!(
            pin_default_workflow(&pool, &home, &library, "beta")
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            crate::project::rules::pins("beta")
                .unwrap()
                .workflows
                .is_empty()
        );

        assert!(
            pin_default_workflow(&pool, &home, &library, "a:b")
                .await
                .is_err()
        );
    }

    const NOW: &str = "2026-09-27T12:00:00+00:00";

    fn write(path: &Path, contents: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contents).unwrap();
    }

    /// No marker, or one that is not a marker, is "not onboarded" — never an error.
    #[test]
    fn a_missing_or_unreadable_marker_is_not_onboarded() {
        let home = tempfile::tempdir().unwrap();
        assert!(!is_onboarded(Some(home.path()), "alpha"));
        assert!(!is_onboarded(None, "alpha"));
        crate::project::state::write_for_test(home.path(), "alpha", MARKER_FILE, "[not a marker");
        assert!(!is_onboarded(Some(home.path()), "alpha"));
    }

    #[test]
    fn a_gate_that_cannot_be_one_is_refused() {
        assert!(validate_gate("").is_err());
        assert!(validate_gate("  ").is_err());
        assert!(validate_gate("cargo test\nrm -rf /").is_err());
        assert!(validate_gate(&"x".repeat(MAX_GATE_COMMAND + 1)).is_err());
        assert!(validate_gate("cargo test").is_ok());
    }

    /// The whole act on injected directories: hook wired, gate stored in the settings store, the
    /// marker last, and a second run is the same state rather than a second copy of anything.
    #[tokio::test]
    async fn onboarding_installs_the_hook_stores_the_gate_and_writes_the_marker() {
        let home = crate::project::rules::testing::Home::new();
        let pool = crate::testdb::fresh_pool().await;
        let project = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(project.path().join(".claude")).unwrap();
        let id = "onboard-whole-act";

        let marker = onboard(
            &pool,
            home.path(),
            id,
            project.path(),
            Some("cargo test"),
            NOW,
        )
        .await
        .unwrap();

        assert!(crate::governance::autopilot::classifier_hook_is_wired(
            project.path()
        ));
        assert_eq!(marker.harnesses, vec![".claude".to_string()]);
        assert!(marker.hook_installed && !marker.migrated);
        assert_eq!(read_marker(Some(home.path()), id), Some(marker));
        let rules = crate::project::rules::autopilot(id).unwrap();
        assert_eq!(rules.gate_command.as_deref(), Some("cargo test"));

        onboard(
            &pool,
            home.path(),
            id,
            project.path(),
            Some("cargo test"),
            NOW,
        )
        .await
        .unwrap();
        assert_eq!(
            crate::project::rules::autopilot(id)
                .unwrap()
                .gate_command
                .as_deref(),
            Some("cargo test"),
            "onboarding again is the same state"
        );
        assert!(is_onboarded(Some(home.path()), id));
    }

    #[tokio::test]
    async fn onboard_stores_the_gate_command_in_the_autopilot_document() {
        let home = crate::project::rules::testing::Home::new();
        let pool = crate::testdb::fresh_pool().await;
        let project = tempfile::tempdir().unwrap();
        let id = "onboard-gate-doc";
        let scope = crate::settings::Scope::project(id);

        onboard(
            &pool,
            home.path(),
            id,
            project.path(),
            Some("cargo test"),
            NOW,
        )
        .await
        .unwrap();

        assert_eq!(
            crate::project::rules::autopilot(id)
                .unwrap()
                .gate_command
                .as_deref(),
            Some("cargo test")
        );
        let history = crate::settings::history(&pool, &scope, "autopilot")
            .await
            .unwrap();
        assert_eq!(history.len(), 1, "one write, one history row");
        assert_eq!(history[0].written_by, "daemon:onboarding");
    }

    /// Nobody confirmed a gate: the stored rules are not touched, and a gate set elsewhere survives.
    #[tokio::test]
    async fn no_confirmed_gate_leaves_the_rules_alone() {
        let home = crate::project::rules::testing::Home::new();
        let pool = crate::testdb::fresh_pool().await;
        let project = tempfile::tempdir().unwrap();
        let id = "onboard-no-gate";
        crate::project::rules::testing::publish_rules(id, "gate_command: make ci\n");

        let marker = onboard(&pool, home.path(), id, project.path(), None, NOW)
            .await
            .unwrap();

        assert_eq!(marker.gate_command, None);
        assert_eq!(
            crate::project::rules::autopilot(id)
                .unwrap()
                .gate_command
                .as_deref(),
            Some("make ci")
        );
        assert!(is_onboarded(Some(home.path()), id));
    }

    /// A bad gate is refused first: nothing is written to the store, the hook or the marker.
    #[tokio::test]
    async fn onboard_with_a_bad_gate_writes_nothing() {
        let home = crate::project::rules::testing::Home::new();
        let pool = crate::testdb::fresh_pool().await;
        let project = tempfile::tempdir().unwrap();
        let id = "onboard-bad-gate";
        let scope = crate::settings::Scope::project(id);

        let outcome = onboard(&pool, home.path(), id, project.path(), Some("a\nb"), NOW).await;

        assert!(matches!(outcome, Err(OnboardError::Gate(_))), "{outcome:?}");
        assert!(!crate::project::rules::is_stored(id, "autopilot"));
        assert!(
            crate::settings::history(&pool, &scope, "autopilot")
                .await
                .unwrap()
                .is_empty()
        );
        assert!(
            !project.path().join(".claude").exists(),
            "no hook was installed"
        );
        assert!(!is_onboarded(Some(home.path()), id));
    }

    #[tokio::test]
    async fn onboard_keeps_every_other_rule_when_the_document_exists() {
        let home = crate::project::rules::testing::Home::new();
        let pool = crate::testdb::fresh_pool().await;
        let project = tempfile::tempdir().unwrap();
        let id = "onboard-keeps-rules";
        crate::project::rules::testing::publish_rules(
            id,
            "max_turns: 7\nhealth_breach_intent: true\ngate_command: old\n",
        );

        onboard(
            &pool,
            home.path(),
            id,
            project.path(),
            Some("cargo test"),
            NOW,
        )
        .await
        .unwrap();

        let rules = crate::project::rules::autopilot(id).unwrap();
        assert_eq!(rules.gate_command.as_deref(), Some("cargo test"));
        assert_eq!(rules.max_turns, Some(7));
        assert!(rules.health_breach_intent);
    }

    /// An unreadable stored document, or an id that cannot name a state directory, is refused.
    #[tokio::test]
    async fn a_refused_gate_or_unreadable_rules_writes_nothing() {
        let home = crate::project::rules::testing::Home::new();
        let pool = crate::testdb::fresh_pool().await;
        let project = tempfile::tempdir().unwrap();
        let id = "onboard-unreadable";

        crate::project::rules::testing::publish_unreadable(id, "autopilot", "schedules: [unclosed");
        assert!(matches!(
            onboard(
                &pool,
                home.path(),
                id,
                project.path(),
                Some("cargo test"),
                NOW
            )
            .await,
            Err(OnboardError::Rules(_))
        ));
        assert!(matches!(
            onboard(&pool, home.path(), "../escape", project.path(), None, NOW).await,
            Err(OnboardError::BadId)
        ));
        assert!(!is_onboarded(Some(home.path()), id));
    }

    /// A settings file this daemon cannot parse is the project saying no; nothing is marked.
    #[tokio::test]
    async fn a_hook_that_cannot_be_installed_leaves_the_project_not_onboarded() {
        let home = crate::project::rules::testing::Home::new();
        let pool = crate::testdb::fresh_pool().await;
        let project = tempfile::tempdir().unwrap();
        let id = "onboard-bad-hook";
        write(&project.path().join(".claude/settings.json"), "{ not json");

        assert!(matches!(
            onboard(
                &pool,
                home.path(),
                id,
                project.path(),
                Some("cargo test"),
                NOW
            )
            .await,
            Err(OnboardError::Hook(_))
        ));
        assert!(!is_onboarded(Some(home.path()), id));
        assert!(!crate::project::rules::is_stored(id, "autopilot"));
    }

    #[tokio::test]
    async fn pin_default_workflow_skips_when_a_pins_document_is_stored() {
        let _home = crate::project::rules::testing::Home::new();
        let pool = crate::testdb::fresh_pool().await;
        let temp = tempfile::tempdir().unwrap();
        let library = library_with(temp.path(), &["2.0.0"]);
        let home = temp.path().join("home");
        let id = "pin-skips-stored";
        crate::project::rules::testing::publish_pins(id, "workflows: []\n");

        let pinned = pin_default_workflow(&pool, &home, &library, id)
            .await
            .unwrap();

        assert!(pinned.is_none());
        assert!(
            crate::project::rules::pins(id)
                .unwrap()
                .workflows
                .is_empty(),
            "the stored choice is left alone"
        );
        let scope = crate::settings::Scope::project(id);
        assert!(
            crate::settings::history(&pool, &scope, "workflow_pins")
                .await
                .unwrap()
                .is_empty(),
            "and nothing was written"
        );
    }

    /// Spec 11, the gate variant: the onboarding write bumped the version, so an editor that read
    /// the document before it is stale.
    #[tokio::test]
    async fn a_put_carrying_the_pre_internal_write_version_is_stale() {
        let home = crate::project::rules::testing::Home::new();
        let pool = crate::testdb::fresh_pool().await;
        let project = tempfile::tempdir().unwrap();
        let id = "onboard-stale-put";
        let scope = crate::settings::Scope::project(id);
        // Stored through the real write path: a snapshot-only fixture has no database row, so the
        // onboarding write would land as version 1 and not exceed the fixture's version 1.
        let before = crate::settings::put(
            &pool,
            &scope,
            "autopilot",
            &serde_json::json!({ "max_turns": 3 }),
            "test",
            Some(0),
        )
        .await
        .expect("the fixture is stored");

        onboard(
            &pool,
            home.path(),
            id,
            project.path(),
            Some("cargo test"),
            NOW,
        )
        .await
        .unwrap();
        let after = crate::settings::global()
            .snapshot()
            .version(&scope, "autopilot")
            .unwrap();
        assert!(after > before, "the internal write bumped the version");

        let outcome = crate::settings::put(
            &pool,
            &scope,
            "autopilot",
            &serde_json::json!({ "max_turns": 9 }),
            "test",
            Some(before),
        )
        .await;
        assert!(
            matches!(outcome, Err(crate::settings::SettingsError::Stale { .. })),
            "a put with the old version must be stale, got {outcome:?}"
        );
    }
}
