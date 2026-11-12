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
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::Value;
use sqlx::SqlitePool;

use super::{DocType, Kind, Scope, Settings, SettingsError, Unreadable};
use crate::models::route_advice;
use crate::models::route_doc::RouterDoc;
use crate::{config, feed, machine_config, project, workflows};

/// What a file is renamed to once its document exists.
pub const IMPORTED_SUFFIX: &str = ".imported";

/// `written_by` of every document this module creates.
const WRITTEN_BY: &str = "import";

/// The prefix of the error `yaml_to_body` answers for a document it has no arm for; a test holds the registry to it.
const NO_IMPORTER: &str = "no importer for";

/// What one run did, for the startup log and for tests. Scopes are cloned, names are the registry's.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Outcome {
    pub imported: Vec<(Scope, &'static str)>,
    pub refused: Vec<(Scope, &'static str)>,
}

fn to_json<T: Serialize>(value: T) -> Result<Value, String> {
    serde_json::to_value(value).map_err(|error| error.to_string())
}

/// The document body a legacy YAML file means, by the legacy parser's own rules.
///
/// One arm per registered document; `every_registered_document_has_a_conversion` fails when one is missing. The
/// council is parsed as if a local model were available (the write route's rule): whether a `kind: local` seat can
/// run is proved at startup, by `config::runnable_council`.
pub fn yaml_to_body(name: &str, text: &str) -> Result<Value, String> {
    match name {
        "machine" => to_json(
            serde_yaml::from_str::<config::MachineConfig>(text)
                .map_err(|error| error.to_string())?,
        ),
        "email" => to_json(config::parse_email_config(text)?),
        "voice" => to_json(config::parse_voice_config(text)?),
        "calendar" => to_json(config::parse_calendar_config(text)?),
        "web" => to_json(config::parse_web_config(text)?),
        "browser" => to_json(config::parse_browser_config(text)?),
        "github" => to_json(config::parse_github_config(text)?),
        "council" => to_json(config::parse_council_config(text, true)?),
        "router" => to_json(RouterDoc::from_config(&route_advice::parse_config(text)?)),
        "models" => to_json(config::parse_models_config(text)?),
        "verify" => to_json(config::parse_verify_config(text)?),
        "devtime" => to_json(config::parse_devtime_config(text)?),
        "autopilot" => {
            to_json(config::parse_schedule_rules(text).map_err(|error| error.to_string())?)
        }
        "workflow_pins" => to_json(workflows::parse_pins(text)?),
        // agy is new in the settings store: no legacy file ever held it.
        "agy" => Err("agy has no legacy file".into()),
        // The account pool is new in the settings store as well: no legacy file ever held it.
        "account_pool" => Err("account_pool has no legacy file".into()),
        "keep_awake" => to_json(config::parse_keep_awake_config(text)?),
        "background" => to_json(config::parse_background_config(text)?),
        channel if channel.starts_with("channels/") => {
            // A blank file means the defaults, as the old reader did.
            if text.trim().is_empty() {
                return to_json(crate::channels::registry::ChannelConfig::default());
            }
            // The struct denies unknown fields, but a legacy file may carry keys this daemon dropped: read through
            // the lenient path (as a stored row is) so those keys are removed and the rest kept. A syntax or type
            // error still refuses the file.
            let raw: Value = serde_yaml::from_str(text).map_err(|error| error.to_string())?;
            let config: crate::channels::registry::ChannelConfig = super::decode::tolerant(raw)?;
            to_json(config)
        }
        other => Err(format!("{NO_IMPORTER} `{other}`")),
    }
}

/// The file a document came from, or `None` when there is no such place (a project id that cannot name a directory).
fn source_path(root: &Path, scope: &Scope, ty: &DocType) -> Option<PathBuf> {
    match scope.project_id() {
        Some(id) => project::state::file(Some(root), id, ty.origin_file),
        None => Some(root.join(ty.origin_file)),
    }
}

/// The file as a person is shown it: `~/.nucleos/<file>` or `~/.nucleos/projects/<id>/<file>`.
fn shown(scope: &Scope, ty: &DocType) -> String {
    match scope.project_id() {
        Some(id) => project::state::display_path(id, ty.origin_file),
        None => machine_config::display_path(ty.origin_file),
    }
}

fn renamed(path: &Path) -> Option<PathBuf> {
    let mut name = path.file_name()?.to_os_string();
    name.push(IMPORTED_SUFFIX);
    Some(path.with_file_name(name))
}

/// One line in the scope's feed. A lost line is logged and let go: the import itself already happened (or was refused).
async fn feed_line(pool: &SqlitePool, scope: &Scope, summary: &str) {
    if let Err(error) = feed::append(
        pool,
        scope.project_id(),
        "config_migrated",
        summary,
        None,
        None,
    )
    .await
    {
        tracing::warn!(%error, "a settings import line could not be written to the feed");
    }
}

async fn refuse(
    pool: &SqlitePool,
    settings: &Settings,
    scope: &Scope,
    ty: &'static DocType,
    why: &str,
    outcome: &mut Outcome,
) {
    tracing::warn!(document = ty.name, %scope, %why, "settings file not imported; nothing is stored, so readers use the defaults and an opt-in pillar stays off");
    feed_line(
        pool,
        scope,
        &format!(
            "{} was not imported ({why}); the file stays where it is, nothing is stored, and an opt-in pillar (github, email) stays off until it is fixed",
            shown(scope, ty)
        ),
    )
    .await;
    // A project document is a safety setting (the pre-publish gate, pins): "nothing stored" would read as "gate
    // off", so mark it unreadable and let `rules::*` answer `Err`. Machine documents keep absent = defaults.
    if ty.kind == Kind::Project {
        settings
            .mark_unreadable(Unreadable {
                scope: scope.clone(),
                name: ty.name.to_owned(),
                version: 0,
                reason: why.to_owned(),
            })
            .await;
    }
    outcome.refused.push((scope.clone(), ty.name));
}

async fn import_document(
    pool: &SqlitePool,
    settings: &Settings,
    root: &Path,
    scope: &Scope,
    ty: &'static DocType,
    outcome: &mut Outcome,
) {
    // A document that exists is the truth; the file (if any) is not even read.
    if settings.snapshot().entry(scope, ty.name).is_some() {
        return;
    }
    let Some(path) = source_path(root, scope, ty) else {
        return;
    };
    let text = match tokio::fs::read_to_string(&path).await {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return,
        Err(error) => {
            refuse(
                pool,
                settings,
                scope,
                ty,
                &format!("it could not be read: {error}"),
                outcome,
            )
            .await;
            return;
        }
    };
    let body = match yaml_to_body(ty.name, &text) {
        Ok(body) => body,
        Err(why) => {
            refuse(pool, settings, scope, ty, &why, outcome).await;
            return;
        }
    };
    match settings
        .put(pool, scope, ty.name, &body, WRITTEN_BY, Some(0))
        .await
    {
        Ok(_) => {}
        // Somebody else created it between the check above and the write: theirs stands.
        Err(SettingsError::Stale { .. }) => return,
        Err(SettingsError::Invalid(why)) => {
            refuse(pool, settings, scope, ty, &why, outcome).await;
            return;
        }
        Err(other) => {
            refuse(pool, settings, scope, ty, &other.to_string(), outcome).await;
            return;
        }
    }
    outcome.imported.push((scope.clone(), ty.name));
    let kept = match renamed(&path) {
        Some(target) => match tokio::fs::rename(&path, &target).await {
            Ok(()) => format!(
                "the file was renamed to {}{IMPORTED_SUFFIX} and is no longer read",
                ty.origin_file
            ),
            Err(error) => {
                tracing::warn!(%error, path = %path.display(), "imported, but the file could not be renamed");
                "the file could not be renamed, but it is no longer read".to_string()
            }
        },
        None => "the file is no longer read".to_string(),
    };
    feed_line(
        pool,
        scope,
        &format!("{} imported into the database; {kept}", shown(scope, ty)),
    )
    .await;
}

/// Imports every machine file that has no document yet.
async fn import_machine(
    pool: &SqlitePool,
    settings: &Settings,
    root: &Path,
    outcome: &mut Outcome,
) {
    for ty in super::documents()
        .iter()
        .filter(|ty| ty.kind == Kind::Machine)
    {
        import_document(pool, settings, root, &Scope::Machine, ty, outcome).await;
    }
}

/// Every project id that may have state files: the roster (rooted or not — a project in `off` keeps its rules) united
/// with the directories already under `~/.nucleos/projects/`. An id that could not name a directory has no state
/// directory (`project::state::valid_id`) and is dropped.
async fn project_ids(pool: &SqlitePool, root: &Path) -> BTreeSet<String> {
    let mut ids = BTreeSet::new();
    match sqlx::query_scalar::<_, String>("SELECT project_id FROM autopilot_state")
        .fetch_all(pool)
        .await
    {
        Ok(rows) => ids.extend(rows),
        Err(error) => {
            tracing::warn!(%error, "could not read the project roster, so only the project folders on disk are imported")
        }
    }
    if let Ok(mut entries) = tokio::fs::read_dir(root.join(project::state::PROJECTS_DIR)).await {
        while let Ok(Some(entry)) = entries.next_entry().await {
            let is_dir = entry.file_type().await.is_ok_and(|kind| kind.is_dir());
            if let (true, Some(name)) = (is_dir, entry.file_name().to_str()) {
                ids.insert(name.to_string());
            }
        }
    }
    ids.retain(|id| project::state::valid_id(id));
    ids
}

async fn import_projects(
    pool: &SqlitePool,
    settings: &Settings,
    root: &Path,
    outcome: &mut Outcome,
) {
    let types: Vec<&'static DocType> = super::documents()
        .iter()
        .filter(|ty| ty.kind == Kind::Project)
        .collect();
    for id in project_ids(pool, root).await {
        let scope = Scope::project(id);
        for ty in &types {
            import_document(pool, settings, root, &scope, ty, outcome).await;
        }
    }
}

/// The startup import. See the module doc. `root` is `machine_config::root()`.
pub async fn run(
    pool: &SqlitePool,
    settings: &Settings,
    root: Option<&Path>,
    is_primary: bool,
) -> Outcome {
    let mut outcome = Outcome::default();
    if !is_primary {
        tracing::debug!("secondary instance: no settings import");
        return outcome;
    }
    let Some(root) = root else {
        return outcome;
    };
    import_machine(pool, settings, root, &mut outcome).await;
    import_projects(pool, settings, root, &mut outcome).await;
    outcome
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
        let email = settings
            .snapshot()
            .get::<crate::config::EmailConfig>(&Scope::Machine);
        assert_eq!(email.host, "imap.example.com");
        assert_eq!(
            settings.snapshot().version(&Scope::Machine, "email"),
            Some(1)
        );
        assert!(!path.exists(), "the original is renamed away");
        assert_eq!(
            std::fs::read_to_string(imported(&path)).unwrap(),
            EMAIL,
            "contents are kept byte for byte"
        );
        let lines = feed_lines(&pool, "config_migrated").await;
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert!(lines[0].contains("~/.nucleos/email.yaml"), "{lines:?}");
        assert!(
            !lines[0].contains("imap.example.com"),
            "a feed line never carries the body: {lines:?}"
        );
        assert_eq!(
            feed_lines(&pool, "config_written").await.len(),
            1,
            "the store's own line"
        );
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
        assert!(
            settings
                .snapshot()
                .entry(&Scope::Machine, "email")
                .is_none()
        );
        assert!(
            !settings
                .snapshot()
                .get::<crate::config::EmailConfig>(&Scope::Machine)
                .enabled
        );
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

        assert_eq!(
            outcome,
            Outcome::default(),
            "nothing to do, nothing reported"
        );
        assert_eq!(
            settings
                .snapshot()
                .get::<crate::config::EmailConfig>(&Scope::Machine)
                .host,
            "imap.edited.example"
        );
        assert!(path.exists(), "the restored file is left alone");
        assert_eq!(
            settings.snapshot().version(&Scope::Machine, "email"),
            Some(2)
        );
    }

    #[tokio::test]
    async fn a_second_importer_losing_the_race_is_silent() {
        let pool = crate::testdb::fresh_pool().await;
        let settings = Settings::new();
        let home = home();
        write(&home.root, "email.yaml", EMAIL);
        // Another path wrote the document after this importer decided to import it: `put` with `Some(0)` answers Stale.
        let body = yaml_to_body("email", EMAIL).unwrap();
        settings
            .put(&pool, &Scope::Machine, "email", &body, "other", Some(0))
            .await
            .unwrap();
        let stale = Settings::new(); // a handle that has not seen it
        let outcome = run(&pool, &stale, Some(&home.root), true).await;
        assert!(outcome.refused.is_empty(), "{outcome:?}");
        assert!(
            feed_lines(&pool, "config_migrated").await.is_empty(),
            "a lost race says nothing"
        );
    }

    /// The pillar's opt-in is "a document exists", and an absent file must not create one.
    #[tokio::test]
    async fn the_github_opt_in_is_the_document_not_its_defaults() {
        let pool = crate::testdb::fresh_pool().await;
        let settings = Settings::new();
        let home = home();

        run(&pool, &settings, Some(&home.root), true).await;
        assert!(
            settings
                .snapshot()
                .get_opt::<crate::config::GithubConfig>(&Scope::Machine)
                .is_none()
        );

        write(&home.root, "github.yaml", "enabled: false\n");
        run(&pool, &settings, Some(&home.root), true).await;
        let github = settings
            .snapshot()
            .get_opt::<crate::config::GithubConfig>(&Scope::Machine)
            .expect("a present file is an opt-in");
        assert!(
            !github.enabled,
            "and its own value wins over the default of `true`"
        );
    }

    /// Pins decision 7, a deliberate deviation from spec §3 step 3 ("usa os defaults"): the defaults of `github` are
    /// `enabled: true`, so a `github.yaml` that does not parse must NOT turn the pillar on. No row, the pillar stays
    /// off, the file stays where it is, and the feed warning repeats on every start until the file is fixed.
    /// (`refuse` also logs a `tracing::warn!`; log capture is not asserted here, the feed line is the owner-visible one.)
    #[tokio::test]
    async fn an_invalid_github_file_leaves_the_pillar_off_the_file_kept_and_the_warning_on_every_start()
     {
        let pool = crate::testdb::fresh_pool().await;
        let settings = Settings::new();
        let home = home();
        let path = write(&home.root, "github.yaml", "enabled: [oops\n");

        let first = run(&pool, &settings, Some(&home.root), true).await;

        assert_eq!(first.refused, vec![(Scope::Machine, "github")]);
        assert!(first.imported.is_empty(), "{first:?}");
        assert!(
            settings
                .snapshot()
                .entry(&Scope::Machine, "github")
                .is_none(),
            "no row"
        );
        assert!(
            settings
                .snapshot()
                .get_opt::<crate::config::GithubConfig>(&Scope::Machine)
                .is_none(),
            "the pillar is off: the opt-in is a document, and there is none"
        );
        assert!(path.exists(), "the file is kept");
        assert!(!imported(&path).exists());
        let lines = feed_lines(&pool, "config_migrated").await;
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert!(
            lines[0].contains("~/.nucleos/github.yaml") && lines[0].contains("not imported"),
            "{lines:?}"
        );

        // The next start says it again: the warning is not a one-off.
        let second = run(&pool, &settings, Some(&home.root), true).await;
        assert_eq!(second.refused, vec![(Scope::Machine, "github")]);
        assert!(
            settings
                .snapshot()
                .get_opt::<crate::config::GithubConfig>(&Scope::Machine)
                .is_none()
        );
        assert_eq!(
            feed_lines(&pool, "config_migrated").await.len(),
            2,
            "one warning per start"
        );
    }

    #[tokio::test]
    async fn council_router_and_models_come_across_in_their_own_shapes() {
        let pool = crate::testdb::fresh_pool().await;
        let settings = Settings::new();
        let home = home();
        write(&home.root, "council.yaml", MIXED_COUNCIL);
        write(
            &home.root,
            "router.yaml",
            "mode: shadow\nurl: http://127.0.0.1:9\n",
        );
        write(
            &home.root,
            "nucleos-models.yaml",
            "claude_model: opus\ncodex_model: gpt\n",
        );

        let outcome = run(&pool, &settings, Some(&home.root), true).await;

        assert!(outcome.refused.is_empty(), "{outcome:?}");
        let snapshot = settings.snapshot();
        // Imported although a local seat is named: runnable-ness is decided at startup, not here.
        let council = snapshot
            .get_opt::<crate::config::CouncilConfig>(&Scope::Machine)
            .expect("council");
        assert_eq!(council.members.len(), 2);
        let router = snapshot
            .get_opt::<crate::models::route_doc::RouterDoc>(&Scope::Machine)
            .expect("router");
        assert_eq!(router.mode, "shadow");
        assert!(router.to_config().is_ok());
        assert_eq!(
            snapshot
                .get::<crate::config::ModelsConfig>(&Scope::Machine)
                .claude_model,
            "opus"
        );
        assert!(imported(&home.root.join("nucleos-models.yaml")).exists());
    }

    /// Prices imported as `null` next to a kept `prices_as_of` would read as "not old" for up to a
    /// week while nothing could be priced, so they are checked through `price_of`, in the imported
    /// snapshot and in a fresh load of the table.
    #[tokio::test]
    async fn the_prices_of_the_models_file_come_across_and_can_be_read() {
        let pool = crate::testdb::fresh_pool().await;
        let settings = Settings::new();
        let home = home();
        write(
            &home.root,
            "nucleos-models.yaml",
            "claude_model: opus\ncodex_model: gpt\nprices_as_of: \"2026-10-01\"\n\
             model_prices:\n  claude-opus-4-7: { input: 5.0, output: 25.0 }\n",
        );

        let outcome = run(&pool, &settings, Some(&home.root), true).await;

        assert!(outcome.refused.is_empty(), "{outcome:?}");
        let models = settings
            .snapshot()
            .get::<crate::config::ModelsConfig>(&Scope::Machine);
        assert_eq!(models.prices_as_of.as_deref(), Some("2026-10-01"));
        assert!(models.price_of("claude-opus-4-7").is_some(), "{models:?}");
        let next_start = Settings::new();
        next_start.load(&pool).await.unwrap();
        let stored = next_start
            .snapshot()
            .get::<crate::config::ModelsConfig>(&Scope::Machine);
        assert!(stored.price_of("claude-opus-4-7").is_some(), "{stored:?}");
    }

    #[tokio::test]
    async fn an_absent_council_file_leaves_no_council() {
        let pool = crate::testdb::fresh_pool().await;
        let settings = Settings::new();
        let home = home();
        run(&pool, &settings, Some(&home.root), true).await;
        assert!(
            settings
                .snapshot()
                .get_opt::<crate::config::CouncilConfig>(&Scope::Machine)
                .is_none()
        );
        assert_eq!(
            settings.snapshot().iter().count(),
            0,
            "an empty home imports nothing"
        );
    }

    #[tokio::test]
    async fn a_router_off_this_machine_is_refused_not_stored() {
        let pool = crate::testdb::fresh_pool().await;
        let settings = Settings::new();
        let home = home();
        let path = write(
            &home.root,
            "router.yaml",
            "mode: shadow\nurl: http://example.com:1\n",
        );

        let outcome = run(&pool, &settings, Some(&home.root), true).await;

        assert_eq!(outcome.refused, vec![(Scope::Machine, "router")]);
        assert!(path.exists());
        assert!(
            settings
                .snapshot()
                .entry(&Scope::Machine, "router")
                .is_none()
        );
    }

    const RULES: &str = "gate_command: \"true\"\n";
    const PINS: &str = "workflows: []\n";

    #[tokio::test]
    async fn a_projects_files_are_imported_into_that_projects_scope() {
        let pool = crate::testdb::fresh_pool().await;
        let settings = Settings::new();
        let home = home();
        // `alpha` is on the roster and has a rules file; `beta` is only a directory with a pins file.
        sqlx::query("INSERT INTO autopilot_state (project_id, mode) VALUES ('alpha', 'off')")
            .execute(&pool)
            .await
            .unwrap();
        let rules =
            crate::project::state::write_for_test(&home.root, "alpha", "autopilot.yaml", RULES);
        crate::project::state::write_for_test(&home.root, "beta", "workflows.yaml", PINS);

        let outcome = run(&pool, &settings, Some(&home.root), true).await;

        let alpha = Scope::project("alpha");
        let beta = Scope::project("beta");
        assert!(
            outcome.imported.contains(&(alpha.clone(), "autopilot")),
            "{outcome:?}"
        );
        assert!(
            outcome.imported.contains(&(beta.clone(), "workflow_pins")),
            "{outcome:?}"
        );
        assert_eq!(
            settings
                .snapshot()
                .get::<crate::config::AutopilotRules>(&alpha)
                .gate_command
                .as_deref(),
            Some("true")
        );
        assert!(
            settings.snapshot().entry(&beta, "autopilot").is_none(),
            "no file, no document"
        );
        assert!(!rules.exists() && imported(&rules).exists());
        // The line goes to THAT project's feed.
        let alpha_lines: Vec<String> = sqlx::query_scalar(
            "SELECT summary FROM feed WHERE project_id = 'alpha' AND kind = 'config_migrated'",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(alpha_lines.len(), 1, "{alpha_lines:?}");
        assert!(
            alpha_lines[0].contains("~/.nucleos/projects/alpha/autopilot.yaml"),
            "{alpha_lines:?}"
        );
    }

    #[tokio::test]
    async fn an_id_that_cannot_name_a_directory_is_never_imported() {
        let pool = crate::testdb::fresh_pool().await;
        let settings = Settings::new();
        let home = home();
        sqlx::query("INSERT INTO autopilot_state (project_id, mode) VALUES ('../escape', 'off')")
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
        assert!(
            settings
                .snapshot()
                .entry(&Scope::Machine, "email")
                .is_none()
        );
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
        assert_eq!(
            next_start.snapshot().version(&Scope::Machine, "email"),
            Some(1)
        );
        assert_eq!(
            next_start
                .snapshot()
                .get::<crate::config::EmailConfig>(&Scope::Machine)
                .host,
            "imap.example.com"
        );
    }

    /// A project file that is syntactically broken: `gate_before_publish: true` is in it, and the old loader failed
    /// the whole file, so nothing was published. "Refused" must therefore not read as "absent".
    const BAD_RULES: &str = "gate_before_publish: true\ngate_command: [oops\n";
    const BAD_PINS: &str = "workflows: [oops\n";

    /// D2: a refused project document is marked unreadable in the snapshot (not left absent), so that
    /// `rules::*` answer `Err` for that project; the file stays on disk under its own name.
    #[tokio::test]
    async fn a_refused_project_file_is_marked_unreadable_and_not_left_absent() {
        use crate::config::AutopilotRules;
        use crate::project::rules::read_from;
        use crate::workflows::Pins;

        let pool = crate::testdb::fresh_pool().await;
        let settings = Settings::new();
        let home = home();
        let rules =
            crate::project::state::write_for_test(&home.root, "alpha", "autopilot.yaml", BAD_RULES);
        crate::project::state::write_for_test(&home.root, "gamma", "workflows.yaml", BAD_PINS);
        // A neighbour that is fine must come in and stay readable.
        crate::project::state::write_for_test(&home.root, "beta", "workflows.yaml", PINS);

        let outcome = run(&pool, &settings, Some(&home.root), true).await;

        assert!(
            outcome
                .refused
                .contains(&(Scope::project("alpha"), "autopilot")),
            "{outcome:?}"
        );
        let snapshot = settings.snapshot();
        let alpha = Scope::project("alpha");
        assert!(
            snapshot.entry(&alpha, "autopilot").is_none(),
            "nothing is stored"
        );
        assert!(
            snapshot.is_unreadable(&alpha, "autopilot"),
            "the refusal must be recorded, or the readers answer the default (gate off)"
        );
        let error = read_from::<AutopilotRules>(&snapshot, "alpha").unwrap_err();
        assert_eq!(error.name, "autopilot");
        assert!(!error.reason.is_empty());
        assert!(
            read_from::<Pins>(&snapshot, "gamma").is_err(),
            "the same holds for the other project document"
        );
        assert!(read_from::<Pins>(&snapshot, "beta").is_ok());
        assert!(read_from::<AutopilotRules>(&snapshot, "beta").is_ok());
        assert!(
            read_from::<AutopilotRules>(&snapshot, "gamma").is_ok(),
            "a project's other document is not poisoned"
        );
        assert!(rules.exists(), "the original is not renamed");
        assert!(!imported(&rules).exists());
        assert_eq!(std::fs::read_to_string(&rules).unwrap(), BAD_RULES);
    }

    /// D2, restart: nothing is stored, so every start refuses again and marks again.
    #[tokio::test]
    async fn every_start_marks_a_refused_project_file_again() {
        use crate::config::AutopilotRules;
        use crate::project::rules::read_from;

        let pool = crate::testdb::fresh_pool().await;
        let home = home();
        let rules =
            crate::project::state::write_for_test(&home.root, "alpha", "autopilot.yaml", BAD_RULES);
        let alpha = Scope::project("alpha");

        let first = Settings::new();
        run(&pool, &first, Some(&home.root), true).await;
        assert!(first.snapshot().is_unreadable(&alpha, "autopilot"));

        // A new process: a fresh handle loaded from the same database, which holds nothing for the document.
        let second = Settings::new();
        second.load(&pool).await.unwrap();
        let again = run(&pool, &second, Some(&home.root), true).await;
        assert!(
            again.refused.contains(&(alpha.clone(), "autopilot")),
            "{again:?}"
        );
        assert!(
            read_from::<AutopilotRules>(&second.snapshot(), "alpha").is_err(),
            "the second start must mark it again"
        );
        assert_eq!(std::fs::read_to_string(&rules).unwrap(), BAD_RULES);
        assert!(!imported(&rules).exists());
    }

    /// D2, recovery: a valid write through the settings door clears the mark, and the broken file is never read
    /// again (a document exists), so the project reads well on the next start too.
    #[tokio::test]
    async fn a_valid_put_after_a_refusal_makes_the_project_readable_again() {
        use crate::config::AutopilotRules;
        use crate::project::rules::read_from;

        let pool = crate::testdb::fresh_pool().await;
        let settings = Settings::new();
        let home = home();
        crate::project::state::write_for_test(&home.root, "alpha", "autopilot.yaml", BAD_RULES);
        let alpha = Scope::project("alpha");
        run(&pool, &settings, Some(&home.root), true).await;
        assert!(
            read_from::<AutopilotRules>(&settings.snapshot(), "alpha").is_err(),
            "premise: the refusal made the project unreadable"
        );

        let body = yaml_to_body("autopilot", RULES).unwrap();
        settings
            .put(&pool, &alpha, "autopilot", &body, "owner", None)
            .await
            .unwrap();

        let rules = read_from::<AutopilotRules>(&settings.snapshot(), "alpha").unwrap();
        assert_eq!(rules.gate_command.as_deref(), Some("true"));
        assert!(!settings.snapshot().is_unreadable(&alpha, "autopilot"));

        let next_start = Settings::new();
        next_start.load(&pool).await.unwrap();
        let outcome = run(&pool, &next_start, Some(&home.root), true).await;
        assert_eq!(
            outcome,
            Outcome::default(),
            "the stored document wins; the file is not read"
        );
        assert!(read_from::<AutopilotRules>(&next_start.snapshot(), "alpha").is_ok());
    }

    /// Machine documents keep their rule (refused = absent = defaults; github stays off): the new mark is for
    /// project scope only. Characterisation: this passes before the GREEN and guards it.
    #[tokio::test]
    async fn a_refused_machine_file_is_not_marked_unreadable() {
        let pool = crate::testdb::fresh_pool().await;
        let settings = Settings::new();
        let home = home();
        write(&home.root, "email.yaml", "enabled: [oops\n");
        write(&home.root, "github.yaml", "enabled: [oops\n");

        let outcome = run(&pool, &settings, Some(&home.root), true).await;

        assert_eq!(outcome.refused.len(), 2, "{outcome:?}");
        assert!(settings.snapshot().unreadable().is_empty());
        assert!(
            settings
                .snapshot()
                .get_opt::<crate::config::GithubConfig>(&Scope::Machine)
                .is_none()
        );
    }

    /// `ChannelConfig` refuses unknown keys on a PUT (typo guard), but an old `channels/<id>.yaml` was read
    /// leniently, so a stray key in it must not cost the owner the whole channel configuration: the known fields
    /// are stored, the unknown key is dropped, and the file is treated like any other successful import.
    #[tokio::test]
    async fn channel_yaml_with_an_unknown_key_is_imported_and_the_key_dropped() {
        use crate::channels::registry::ChannelConfig;

        let pool = crate::testdb::fresh_pool().await;
        let settings = Settings::new();
        let home = home();
        let text =
            "stream_edits: false\ndoctrine: be brief\nprojects:\n  alpha: \"42\"\nretired_key: 1\n";
        let path = write(&home.root, "channels/telegram.yaml", text);

        let outcome = run(&pool, &settings, Some(&home.root), true).await;

        assert!(outcome.refused.is_empty(), "{outcome:?}");
        assert_eq!(
            outcome.imported,
            vec![(Scope::Machine, "channels/telegram")]
        );
        let snapshot = settings.snapshot();
        let stored = snapshot
            .entry_as::<ChannelConfig>(&Scope::Machine, "channels/telegram")
            .expect("the channel document is stored");
        assert_eq!(stored.stream_edits, Some(false));
        assert_eq!(stored.doctrine.as_deref(), Some("be brief"));
        assert_eq!(stored.projects.get("alpha").map(String::as_str), Some("42"));
        let body = snapshot
            .entry(&Scope::Machine, "channels/telegram")
            .unwrap()
            .body();
        assert!(body.get("retired_key").is_none(), "{body}");
        assert_eq!(
            snapshot.version(&Scope::Machine, "channels/telegram"),
            Some(1)
        );
        // Treated as the other imported files: renamed, byte for byte, with the migration line.
        assert!(!path.exists());
        assert_eq!(std::fs::read_to_string(imported(&path)).unwrap(), text);
        let lines = feed_lines(&pool, "config_migrated").await;
        assert!(
            lines
                .iter()
                .any(|line| line.contains("channels/telegram.yaml") && line.contains("imported")),
            "{lines:?}"
        );
    }

    /// The tolerance above is for unknown keys only: a channel file that is not YAML is still refused and kept.
    #[tokio::test]
    async fn a_syntactically_invalid_channel_yaml_is_still_refused() {
        let pool = crate::testdb::fresh_pool().await;
        let settings = Settings::new();
        let home = home();
        let path = write(
            &home.root,
            "channels/telegram.yaml",
            "stream_edits: [oops\n",
        );

        let outcome = run(&pool, &settings, Some(&home.root), true).await;

        assert_eq!(outcome.refused, vec![(Scope::Machine, "channels/telegram")]);
        assert!(outcome.imported.is_empty(), "{outcome:?}");
        assert!(path.exists() && !imported(&path).exists());
        assert!(
            settings
                .snapshot()
                .entry(&Scope::Machine, "channels/telegram")
                .is_none()
        );
    }

    /// A write that fails leaves the snapshot as it was: nothing half-visible.
    #[tokio::test]
    async fn a_refused_write_leaves_the_snapshot_unchanged() {
        let pool = crate::testdb::fresh_pool().await;
        let settings = Settings::new();
        let before = settings.snapshot();
        let refused = settings
            .put(
                &pool,
                &Scope::Machine,
                "email",
                &serde_json::json!({ "enabled": "not a bool" }),
                "test",
                None,
            )
            .await;
        assert!(
            matches!(refused, Err(SettingsError::Invalid(_))),
            "{refused:?}"
        );
        assert!(
            std::sync::Arc::ptr_eq(&before, &settings.snapshot())
                || settings.snapshot().iter().count() == 0
        );
        assert!(
            settings
                .snapshot()
                .entry(&Scope::Machine, "email")
                .is_none()
        );
    }
}
