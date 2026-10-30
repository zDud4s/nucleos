//! One-time import of the YAML configuration files into the database.
//!
//! Until the settings tables existed, every pillar read its own file under `~/.nucleos/` (and, per project,
//! `~/.nucleos/projects/<id>/`). On the primary instance, at startup, each file that has no document yet is parsed by the
//! SAME function the old loader used, stored as the document's first version (`written_by = "import"`), and renamed to
//! `<file>.imported`. After that the file is never read again.
//!
//! Failure is per document and never fatal: a file that does not parse, or that the strict decode refuses, stays where
//! it is, a feed line says why, and nothing is stored: readers use the defaults, and a pillar whose opt-in is the
//! document (github, email) stays off. It is tried again on every start.
//!
//! RED stubs: `yaml_to_body` and `run` are `todo!()` until the GREEN phase of chunk 2.

use std::path::Path;

use serde_json::Value;
use sqlx::SqlitePool;

use super::{Scope, Settings};

/// What a file is renamed to once its document exists.
pub const IMPORTED_SUFFIX: &str = ".imported";

/// The prefix of the error `yaml_to_body` answers for a document it has no arm for; a test holds the registry to it.
#[cfg_attr(not(test), allow(dead_code))]
const NO_IMPORTER: &str = "no importer for";

/// What one run did, for the startup log and for tests. Scopes are cloned, names are the registry's.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Outcome {
    pub imported: Vec<(Scope, &'static str)>,
    pub refused: Vec<(Scope, &'static str)>,
}

/// The document body a legacy YAML file means, by the legacy parser's own rules.
pub fn yaml_to_body(name: &str, text: &str) -> Result<Value, String> {
    let _ = (name, text);
    todo!("yaml_to_body")
}

/// The startup import. See the module doc. `root` is `machine_config::root()`.
pub async fn run(
    pool: &SqlitePool,
    settings: &Settings,
    root: Option<&Path>,
    is_primary: bool,
) -> Outcome {
    let _ = (pool, settings, root, is_primary);
    todo!("settings::import::run")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::{Scope, Settings, SettingsError};
    use std::path::{Path, PathBuf};

    /// A throwaway `~/.nucleos`.
    struct Home {
        _dir: tempfile::TempDir,
        root: PathBuf,
    }

    fn home() -> Home {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        Home { _dir: dir, root }
    }

    fn write(root: &Path, file: &str, text: &str) -> PathBuf {
        let path = root.join(file);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, text).unwrap();
        path
    }

    fn imported(path: &Path) -> PathBuf {
        let mut name = path.file_name().unwrap().to_os_string();
        name.push(IMPORTED_SUFFIX);
        path.with_file_name(name)
    }

    async fn feed_lines(pool: &sqlx::SqlitePool, kind: &str) -> Vec<String> {
        sqlx::query_scalar("SELECT summary FROM feed WHERE kind = ? ORDER BY id")
            .bind(kind)
            .fetch_all(pool)
            .await
            .unwrap()
    }

    const EMAIL: &str = "enabled: false\nhost: imap.example.com\n";
    const MIXED_COUNCIL: &str = "chairman: { kind: cloud, ref: claude-opus-4-8 }\n\
                                 members:\n\
                                 \x20\x20- { kind: cloud, ref: claude-opus-4-8 }\n\
                                 \x20\x20- { kind: local, ref: qwen3.5:4b }\n";

    /// Every registered document type has a conversion. A new document added to the registry without an arm in
    /// `yaml_to_body` would otherwise be silently skipped by the import.
    #[test]
    fn every_registered_document_has_a_conversion() {
        for ty in crate::settings::documents() {
            let answer = yaml_to_body(ty.name, "");
            if let Err(why) = answer {
                assert!(
                    !why.starts_with(NO_IMPORTER),
                    "`{}` is registered but `yaml_to_body` has no arm for it",
                    ty.name
                );
            }
        }
        let unknown = yaml_to_body("nonsense", "").unwrap_err();
        assert!(unknown.starts_with(NO_IMPORTER), "{unknown}");
    }

    #[tokio::test]
    async fn a_valid_file_is_imported_and_renamed() {
        let pool = crate::testdb::fresh_pool().await;
        let settings = Settings::new();
        let home = home();
        let path = write(&home.root, "email.yaml", EMAIL);

        let outcome = run(&pool, &settings, Some(&home.root), true).await;

        assert_eq!(outcome.imported, vec![(Scope::Machine, "email")]);
        assert!(outcome.refused.is_empty());
        let email = settings.snapshot().get::<crate::config::EmailConfig>(&Scope::Machine);
        assert_eq!(email.host, "imap.example.com");
        assert_eq!(settings.snapshot().version(&Scope::Machine, "email"), Some(1));
        assert!(!path.exists(), "the original is renamed away");
        assert_eq!(std::fs::read_to_string(imported(&path)).unwrap(), EMAIL, "contents are kept byte for byte");
        let lines = feed_lines(&pool, "config_migrated").await;
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert!(lines[0].contains("~/.nucleos/email.yaml"), "{lines:?}");
        assert!(!lines[0].contains("imap.example.com"), "a feed line never carries the body: {lines:?}");
        assert_eq!(feed_lines(&pool, "config_written").await.len(), 1, "the store's own line");
    }

    #[tokio::test]
    async fn an_invalid_file_stays_and_the_document_starts_on_defaults() {
        let pool = crate::testdb::fresh_pool().await;
        let settings = Settings::new();
        let home = home();
        let path = write(&home.root, "email.yaml", "enabled: [oops\n");
        // A neighbour that is fine is not held up by it.
        write(&home.root, "github.yaml", "enabled: false\n");

        let outcome = run(&pool, &settings, Some(&home.root), true).await;

        assert_eq!(outcome.refused, vec![(Scope::Machine, "email")]);
        assert_eq!(outcome.imported, vec![(Scope::Machine, "github")]);
        assert!(path.exists(), "a refused file is not renamed");
        assert!(!imported(&path).exists());
        assert!(settings.snapshot().entry(&Scope::Machine, "email").is_none());
        assert!(!settings.snapshot().get::<crate::config::EmailConfig>(&Scope::Machine).enabled);
        let lines = feed_lines(&pool, "config_migrated").await;
        assert!(
            lines.iter().any(|line| line.contains("~/.nucleos/email.yaml") && line.contains("not imported")),
            "{lines:?}"
        );

        // Fixed, it goes in on the next start.
        std::fs::write(&path, EMAIL).unwrap();
        let again = run(&pool, &settings, Some(&home.root), true).await;
        assert_eq!(again.imported, vec![(Scope::Machine, "email")]);
    }

    #[tokio::test]
    async fn a_file_is_never_imported_twice_nor_over_an_existing_document() {
        let pool = crate::testdb::fresh_pool().await;
        let settings = Settings::new();
        let home = home();
        write(&home.root, "email.yaml", EMAIL);
        run(&pool, &settings, Some(&home.root), true).await;

        // The owner edits the document through the settings door ...
        settings
            .update::<crate::config::EmailConfig, _>(&pool, &Scope::Machine, "owner", |email| {
                email.host = "imap.edited.example".to_string();
            })
            .await
            .unwrap();
        // ... and an old copy of the file turns up again (a restored backup).
        let path = write(&home.root, "email.yaml", EMAIL);

        let outcome = run(&pool, &settings, Some(&home.root), true).await;

        assert_eq!(outcome, Outcome::default(), "nothing to do, nothing reported");
        assert_eq!(
            settings.snapshot().get::<crate::config::EmailConfig>(&Scope::Machine).host,
            "imap.edited.example"
        );
        assert!(path.exists(), "the restored file is left alone");
        assert_eq!(settings.snapshot().version(&Scope::Machine, "email"), Some(2));
    }

    #[tokio::test]
    async fn a_second_importer_losing_the_race_is_silent() {
        let pool = crate::testdb::fresh_pool().await;
        let settings = Settings::new();
        let home = home();
        write(&home.root, "email.yaml", EMAIL);
        // Another path wrote the document after this importer decided to import it: `put` with `Some(0)` answers Stale.
        let body = yaml_to_body("email", EMAIL).unwrap();
        settings.put(&pool, &Scope::Machine, "email", &body, "other", Some(0)).await.unwrap();
        let stale = Settings::new(); // a handle that has not seen it
        let outcome = run(&pool, &stale, Some(&home.root), true).await;
        assert!(outcome.refused.is_empty(), "{outcome:?}");
        assert!(feed_lines(&pool, "config_migrated").await.is_empty(), "a lost race says nothing");
    }

    /// The pillar's opt-in is "a document exists", and an absent file must not create one.
    #[tokio::test]
    async fn the_github_opt_in_is_the_document_not_its_defaults() {
        let pool = crate::testdb::fresh_pool().await;
        let settings = Settings::new();
        let home = home();

        run(&pool, &settings, Some(&home.root), true).await;
        assert!(settings.snapshot().get_opt::<crate::config::GithubConfig>(&Scope::Machine).is_none());

        write(&home.root, "github.yaml", "enabled: false\n");
        run(&pool, &settings, Some(&home.root), true).await;
        let github = settings
            .snapshot()
            .get_opt::<crate::config::GithubConfig>(&Scope::Machine)
            .expect("a present file is an opt-in");
        assert!(!github.enabled, "and its own value wins over the default of `true`");
    }

    /// Pins decision 7, a deliberate deviation from spec §3 step 3 ("usa os defaults"): the defaults of `github` are
    /// `enabled: true`, so a `github.yaml` that does not parse must NOT turn the pillar on. No row, the pillar stays
    /// off, the file stays where it is, and the feed warning repeats on every start until the file is fixed.
    /// (`refuse` also logs a `tracing::warn!`; log capture is not asserted here, the feed line is the owner-visible one.)
    #[tokio::test]
    async fn an_invalid_github_file_leaves_the_pillar_off_the_file_kept_and_the_warning_on_every_start() {
        let pool = crate::testdb::fresh_pool().await;
        let settings = Settings::new();
        let home = home();
        let path = write(&home.root, "github.yaml", "enabled: [oops\n");

        let first = run(&pool, &settings, Some(&home.root), true).await;

        assert_eq!(first.refused, vec![(Scope::Machine, "github")]);
        assert!(first.imported.is_empty(), "{first:?}");
        assert!(settings.snapshot().entry(&Scope::Machine, "github").is_none(), "no row");
        assert!(
            settings.snapshot().get_opt::<crate::config::GithubConfig>(&Scope::Machine).is_none(),
            "the pillar is off: the opt-in is a document, and there is none"
        );
        assert!(path.exists(), "the file is kept");
        assert!(!imported(&path).exists());
        let lines = feed_lines(&pool, "config_migrated").await;
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert!(lines[0].contains("~/.nucleos/github.yaml") && lines[0].contains("not imported"), "{lines:?}");

        // The next start says it again: the warning is not a one-off.
        let second = run(&pool, &settings, Some(&home.root), true).await;
        assert_eq!(second.refused, vec![(Scope::Machine, "github")]);
        assert!(settings.snapshot().get_opt::<crate::config::GithubConfig>(&Scope::Machine).is_none());
        assert_eq!(feed_lines(&pool, "config_migrated").await.len(), 2, "one warning per start");
    }

    #[tokio::test]
    async fn council_router_and_models_come_across_in_their_own_shapes() {
        let pool = crate::testdb::fresh_pool().await;
        let settings = Settings::new();
        let home = home();
        write(&home.root, "council.yaml", MIXED_COUNCIL);
        write(&home.root, "router.yaml", "mode: shadow\nurl: http://127.0.0.1:9\n");
        write(&home.root, "nucleos-models.yaml", "claude_model: opus\ncodex_model: gpt\n");

        let outcome = run(&pool, &settings, Some(&home.root), true).await;

        assert!(outcome.refused.is_empty(), "{outcome:?}");
        let snapshot = settings.snapshot();
        // Imported although a local seat is named: runnable-ness is decided at startup, not here.
        let council = snapshot.get_opt::<crate::config::CouncilConfig>(&Scope::Machine).expect("council");
        assert_eq!(council.members.len(), 2);
        let router = snapshot.get_opt::<crate::models::route_doc::RouterDoc>(&Scope::Machine).expect("router");
        assert_eq!(router.mode, "shadow");
        assert!(router.to_config().is_ok());
        assert_eq!(snapshot.get::<crate::config::ModelsConfig>(&Scope::Machine).claude_model, "opus");
        assert!(imported(&home.root.join("nucleos-models.yaml")).exists());
    }

    #[tokio::test]
    async fn an_absent_council_file_leaves_no_council() {
        let pool = crate::testdb::fresh_pool().await;
        let settings = Settings::new();
        let home = home();
        run(&pool, &settings, Some(&home.root), true).await;
        assert!(settings.snapshot().get_opt::<crate::config::CouncilConfig>(&Scope::Machine).is_none());
        assert_eq!(settings.snapshot().iter().count(), 0, "an empty home imports nothing");
    }

    #[tokio::test]
    async fn a_router_off_this_machine_is_refused_not_stored() {
        let pool = crate::testdb::fresh_pool().await;
        let settings = Settings::new();
        let home = home();
        let path = write(&home.root, "router.yaml", "mode: shadow\nurl: http://example.com:1\n");

        let outcome = run(&pool, &settings, Some(&home.root), true).await;

        assert_eq!(outcome.refused, vec![(Scope::Machine, "router")]);
        assert!(path.exists());
        assert!(settings.snapshot().entry(&Scope::Machine, "router").is_none());
    }

    const RULES: &str = "gate_command: \"true\"\n";
    const PINS: &str = "workflows: []\n";

    #[tokio::test]
    async fn a_projects_files_are_imported_into_that_projects_scope() {
        let pool = crate::testdb::fresh_pool().await;
        let settings = Settings::new();
        let home = home();
        // `alpha` is on the roster and has a rules file; `beta` is only a directory with a pins file.
        sqlx::query("INSERT INTO autopilot_state (project_id) VALUES ('alpha')")
            .execute(&pool)
            .await
            .unwrap();
        let rules = crate::project::state::write_for_test(&home.root, "alpha", "autopilot.yaml", RULES);
        crate::project::state::write_for_test(&home.root, "beta", "workflows.yaml", PINS);

        let outcome = run(&pool, &settings, Some(&home.root), true).await;

        let alpha = Scope::project("alpha");
        let beta = Scope::project("beta");
        assert!(outcome.imported.contains(&(alpha.clone(), "autopilot")), "{outcome:?}");
        assert!(outcome.imported.contains(&(beta.clone(), "workflow_pins")), "{outcome:?}");
        assert_eq!(
            settings.snapshot().get::<crate::config::AutopilotRules>(&alpha).gate_command.as_deref(),
            Some("true")
        );
        assert!(settings.snapshot().entry(&beta, "autopilot").is_none(), "no file, no document");
        assert!(!rules.exists() && imported(&rules).exists());
        // The line goes to THAT project's feed.
        let alpha_lines: Vec<String> =
            sqlx::query_scalar("SELECT summary FROM feed WHERE project_id = 'alpha' AND kind = 'config_migrated'")
                .fetch_all(&pool)
                .await
                .unwrap();
        assert_eq!(alpha_lines.len(), 1, "{alpha_lines:?}");
        assert!(alpha_lines[0].contains("~/.nucleos/projects/alpha/autopilot.yaml"), "{alpha_lines:?}");
    }

    #[tokio::test]
    async fn an_id_that_cannot_name_a_directory_is_never_imported() {
        let pool = crate::testdb::fresh_pool().await;
        let settings = Settings::new();
        let home = home();
        sqlx::query("INSERT INTO autopilot_state (project_id) VALUES ('../escape')")
            .execute(&pool)
            .await
            .unwrap();
        write(&home.root, "escape/autopilot.yaml", RULES);

        let outcome = run(&pool, &settings, Some(&home.root), true).await;

        assert_eq!(outcome, Outcome::default());
    }

    #[tokio::test]
    async fn a_secondary_instance_neither_imports_nor_renames() {
        let pool = crate::testdb::fresh_pool().await;
        let settings = Settings::new();
        let home = home();
        let path = write(&home.root, "email.yaml", EMAIL);

        let outcome = run(&pool, &settings, Some(&home.root), false).await;

        assert_eq!(outcome, Outcome::default());
        assert!(path.exists());
        assert!(settings.snapshot().entry(&Scope::Machine, "email").is_none());
    }

    #[tokio::test]
    async fn no_home_directory_means_no_import() {
        let pool = crate::testdb::fresh_pool().await;
        let settings = Settings::new();
        assert_eq!(run(&pool, &settings, None, true).await, Outcome::default());
    }

    /// The startup order: init, then import, then readers. A sync reader of the process-global handle sees the imported
    /// document as soon as `run` returns, and a fresh process (a new `Settings` loaded from the same database) sees
    /// the same version.
    #[tokio::test]
    async fn the_imported_document_is_visible_to_sync_readers_right_after_startup() {
        let pool = crate::testdb::fresh_pool().await;
        let home = home();
        write(&home.root, "email.yaml", EMAIL);
        let _installed = crate::settings::testing::install(Settings::new());

        crate::settings::init(&pool).await.unwrap();
        assert_eq!(
            crate::settings::get::<crate::config::EmailConfig>(&Scope::Machine).host,
            crate::config::EmailConfig::default().host,
            "before the import the reader sees the defaults"
        );
        run(&pool, &crate::settings::global(), Some(&home.root), true).await;
        assert_eq!(
            crate::settings::get::<crate::config::EmailConfig>(&Scope::Machine).host,
            "imap.example.com"
        );

        let next_start = Settings::new();
        next_start.load(&pool).await.unwrap();
        assert_eq!(next_start.snapshot().version(&Scope::Machine, "email"), Some(1));
        assert_eq!(
            next_start.snapshot().get::<crate::config::EmailConfig>(&Scope::Machine).host,
            "imap.example.com"
        );
    }

    /// A write that fails leaves the snapshot as it was: nothing half-visible.
    #[tokio::test]
    async fn a_refused_write_leaves_the_snapshot_unchanged() {
        let pool = crate::testdb::fresh_pool().await;
        let settings = Settings::new();
        let before = settings.snapshot();
        let refused = settings
            .put(&pool, &Scope::Machine, "email", &serde_json::json!({ "enabled": "not a bool" }), "test", None)
            .await;
        assert!(matches!(refused, Err(SettingsError::Invalid(_))), "{refused:?}");
        assert!(std::sync::Arc::ptr_eq(&before, &settings.snapshot()) || settings.snapshot().iter().count() == 0);
        assert!(settings.snapshot().entry(&Scope::Machine, "email").is_none());
    }
}
