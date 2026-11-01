//! What a project's `autopilot` and `workflow_pins` documents say, read from the settings snapshot.
//!
//! This replaced the removed `config::load_schedule_rules` and `workflows::read_pins` as the READERS of the two project
//! documents. The old loaders read a file on every call and failed with an `io::Error` when the file did not
//! parse; this one reads the in-memory snapshot (no I/O) and fails with [`DocumentUnreadable`] when the stored
//! document no longer fits its type. The contract callers rely on is unchanged: nothing stored is the default,
//! unreadable is an `Err`, and each caller keeps the safe side it already had for that `Err`.

use crate::config::AutopilotRules;
use crate::settings::{Document, Scope, Snapshot};
use crate::workflows::Pins;

/// A stored document that exists but no longer fits its type. `reason` is the decoder's own text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocumentUnreadable {
    pub name: &'static str,
    pub reason: String,
}

impl std::fmt::Display for DocumentUnreadable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "the stored `{}` settings of this project cannot be read: {}",
            self.name, self.reason
        )
    }
}

impl std::error::Error for DocumentUnreadable {}

pub fn autopilot(project_id: &str) -> Result<AutopilotRules, DocumentUnreadable> {
    read_from(&crate::settings::global().snapshot(), project_id)
}

pub fn pins(project_id: &str) -> Result<Pins, DocumentUnreadable> {
    read_from(&crate::settings::global().snapshot(), project_id)
}

/// The project's pins plus the stored version they were read at, from ONE snapshot.
///
/// An absent document is `(Pins::default(), 0)`: `expected: Some(0)` on a `put` means "must not exist yet".
pub fn pins_versioned(project_id: &str) -> Result<(Pins, i64), DocumentUnreadable> {
    let snapshot = crate::settings::global().snapshot();
    let pins = read_from::<Pins>(&snapshot, project_id)?;
    let version = snapshot
        .version(&Scope::project(project_id), "workflow_pins")
        .unwrap_or(0);
    Ok((pins, version))
}

/// Whether the project has a readable stored document called `name`. After a failed load nothing is known, so
/// it is `false` (and the readers above are `Err`).
pub fn is_stored(project_id: &str, name: &str) -> bool {
    let snapshot = crate::settings::global().snapshot();
    let scope = Scope::project(project_id);
    snapshot.load_failed().is_none()
        && snapshot.entry(&scope, name).is_some()
        && !snapshot.is_unreadable(&scope, name)
}

pub fn read_from<T: Document + Default + Clone>(
    snapshot: &Snapshot,
    project_id: &str,
) -> Result<T, DocumentUnreadable> {
    // A snapshot whose load failed is incomplete: "nothing stored" there would read as "gate off".
    if let Some(reason) = snapshot.load_failed() {
        return Err(DocumentUnreadable {
            name: T::NAME,
            reason: format!("the settings could not be loaded: {reason}"),
        });
    }
    let scope = Scope::project(project_id);
    if let Some(record) = snapshot.unreadable_for(&scope, T::NAME) {
        return Err(DocumentUnreadable {
            name: T::NAME,
            reason: record.reason.clone(),
        });
    }
    Ok((*snapshot.get::<T>(&scope)).clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::VerifyGuardMode;
    use crate::settings::{Scope, Settings, Unreadable, testing as settings_testing};

    fn rules_with_gate(command: &str) -> AutopilotRules {
        AutopilotRules {
            gate_command: Some(command.to_string()),
            ..AutopilotRules::default()
        }
    }

    fn unreadable(project: &str, name: &str, reason: &str) -> Unreadable {
        Unreadable {
            scope: Scope::project(project),
            name: name.to_string(),
            version: 3,
            reason: reason.to_string(),
        }
    }

    /// A pin as `workflows.yaml` records it: `origin` and `hash` are required by `Pin`.
    const LINT_PIN: &str =
        "workflows:\n  - name: lint\n    version: v1\n    origin: library\n    hash: abc123\n";

    #[test]
    fn nothing_stored_reads_as_the_default_rules() {
        let _installed = settings_testing::install(Settings::new());
        assert_eq!(autopilot("alpha").unwrap(), AutopilotRules::default());
        assert!(!is_stored("alpha", "autopilot"));
    }

    #[test]
    fn a_stored_document_is_returned_by_value_and_only_for_its_own_project() {
        let snapshot = Snapshot::default()
            .with_document(&Scope::project("alpha"), rules_with_gate("cargo test"));
        let _installed = settings_testing::install(Settings::from_snapshot(snapshot));

        assert_eq!(
            autopilot("alpha").unwrap().gate_command.as_deref(),
            Some("cargo test")
        );
        assert_eq!(autopilot("beta").unwrap().gate_command, None);
        assert!(is_stored("alpha", "autopilot"));
        assert!(!is_stored("beta", "autopilot"));
    }

    #[test]
    fn an_unreadable_document_is_an_error_that_names_the_document_and_the_reason() {
        let mut snapshot = Snapshot::default();
        snapshot.mark_unreadable(unreadable(
            "alpha",
            "autopilot",
            "invalid type: sequence, expected a map",
        ));
        let _installed = settings_testing::install(Settings::from_snapshot(snapshot));

        let error = autopilot("alpha").unwrap_err();
        assert_eq!(error.name, "autopilot");
        let text = error.to_string();
        assert!(text.contains("autopilot"), "{text}");
        assert!(text.contains("invalid type: sequence"), "{text}");
        // Another project is not poisoned by it, and an unreadable document is not "stored".
        assert!(autopilot("beta").is_ok());
        assert!(!is_stored("alpha", "autopilot"));
    }

    #[test]
    fn the_two_documents_are_unreadable_independently() {
        let mut snapshot = Snapshot::default();
        snapshot.mark_unreadable(unreadable("alpha", "workflow_pins", "bad pin"));
        let _installed = settings_testing::install(Settings::from_snapshot(snapshot));

        assert!(autopilot("alpha").is_ok());
        assert_eq!(pins("alpha").unwrap_err().name, "workflow_pins");
    }

    #[test]
    fn a_change_published_after_the_first_read_is_seen_by_the_next_one() {
        let _home = testing::Home::new();
        assert_eq!(
            autopilot("alpha").unwrap().verify_guard,
            VerifyGuardMode::default()
        );

        testing::publish_rules("alpha", "gate_command: cargo test\n");
        assert_eq!(
            autopilot("alpha").unwrap().gate_command.as_deref(),
            Some("cargo test")
        );

        testing::publish_rules("alpha", "gate_command: cargo clippy\n");
        assert_eq!(
            autopilot("alpha").unwrap().gate_command.as_deref(),
            Some("cargo clippy")
        );

        testing::publish_unreadable("alpha", "autopilot", "broken");
        assert!(autopilot("alpha").is_err());

        testing::publish_rules("alpha", "gate_command: cargo test\n");
        assert!(
            autopilot("alpha").is_ok(),
            "a valid write clears the unreadable record"
        );

        testing::clear("alpha", "autopilot");
        assert_eq!(autopilot("alpha").unwrap(), AutopilotRules::default());
    }

    #[test]
    fn pins_follow_the_same_contract() {
        let _home = testing::Home::new();
        assert!(pins("alpha").unwrap().workflows.is_empty());
        testing::publish_pins("alpha", LINT_PIN);
        let pinned = pins("alpha").unwrap();
        assert_eq!(pinned.workflows.len(), 1);
        assert_eq!(pinned.workflows[0].name, "lint");
        assert!(is_stored("alpha", "workflow_pins"));
    }

    /// D2: when `settings::init` fails the snapshot is empty, which reads as "nothing stored" (gate off). It must
    /// read as unreadable instead, so every caller takes its safe side. (A plain never-initialised handle stays the
    /// default: `Home::new()` and most tests above depend on that, so the failure has to be recorded by `init`.)
    #[tokio::test]
    async fn rules_are_an_error_after_a_failed_settings_init() {
        let _home = testing::Home::new();
        let pool = crate::testdb::fresh_pool().await;
        sqlx::query("DROP TABLE settings")
            .execute(&pool)
            .await
            .unwrap();

        assert!(crate::settings::init(&pool).await.is_err(), "premise");

        assert!(autopilot("alpha").is_err(), "must not answer the default");
        assert!(pins("alpha").is_err());
        assert!(pins_versioned("alpha").is_err());
        assert!(!is_stored("alpha", "autopilot"));
    }

    /// Characterisation: `Home` is test support, already implemented, so this one passes before the GREEN.
    #[test]
    fn home_is_a_usable_stand_in_directory() {
        let home = testing::Home::new();
        assert!(home.path().is_dir());
    }

    #[test]
    fn a_reader_that_goes_on_to_publish_takes_the_exclusive_hold() {
        testing::hold_shared();
        // No premise on the value: the shared hold does not exclude tests installing settings under SERIAL alone.
        let _ = autopilot("alpha");

        testing::publish_rules("alpha", "gate_command: cargo test\n");

        assert_eq!(
            autopilot("alpha").unwrap().gate_command.as_deref(),
            Some("cargo test")
        );
    }

    #[test]
    fn a_new_home_starts_from_an_empty_snapshot() {
        let first = testing::Home::new();
        testing::publish_rules("alpha", "gate_command: cargo test\n");
        drop(first);

        let _second = testing::Home::new();

        assert_eq!(autopilot("alpha").unwrap(), AutopilotRules::default());
    }

    #[test]
    #[should_panic(expected = "lock-order violation")]
    fn taking_the_env_lock_while_holding_the_rules_gate_panics() {
        testing::hold_shared();

        let _lock = crate::execution::worktree::test_env_lock();
    }
}

#[cfg(any(test, feature = "testkit"))]
pub mod testing {
    use crate::settings::{self, Scope, Settings, Snapshot};
    use std::cell::RefCell;
    use std::ops::Deref;
    use std::sync::{PoisonError, RwLock, RwLockReadGuard, RwLockWriteGuard};

    /// Readers share it, a publisher owns it. Process-wide because the settings handle is.
    static GATE: RwLock<()> = RwLock::new(());

    /// What this test thread holds. Kept in a thread-local so a helper such as `test_state()` can take the hold on
    /// behalf of a test body that cannot carry a guard, and so it is released when the test thread ends (libtest
    /// runs each test on its own thread).
    enum Held {
        Shared(#[allow(dead_code)] RwLockReadGuard<'static, ()>),
        Exclusive {
            // Field order is drop order: restore the previous handle, then let the readers back in.
            _installed: settings::testing::Installed,
            _gate: RwLockWriteGuard<'static, ()>,
        },
    }

    thread_local! {
        static HELD: RefCell<Option<Held>> = const { RefCell::new(None) };
    }

    /// "This test reads rules and publishes none." Idempotent; a no-op when the thread already holds anything.
    pub fn hold_shared() {
        HELD.with(|held| {
            let mut held = held.borrow_mut();
            if held.is_none() {
                *held = Some(Held::Shared(
                    GATE.read().unwrap_or_else(PoisonError::into_inner),
                ));
            }
        });
    }

    /// Whether this thread holds the gate, shared or exclusive. The hold outlives `Home` and the fixtures that took
    /// it, so once true this stays true until the test thread ends.
    pub fn holds_the_gate() -> bool {
        HELD.with(|held| held.borrow().is_some())
    }

    /// "This test says what the rules are." Idempotent. A shared hold is dropped before the write lock is awaited.
    fn exclusive() {
        HELD.with(|held| {
            let mut held = held.borrow_mut();
            if matches!(*held, Some(Held::Exclusive { .. })) {
                return;
            }
            *held = None;
            // Lock order is SERIAL (inside `install`) first, then GATE, the same as a test that calls
            // `settings::testing::install` itself and then `hold_shared()` through `test_state()`. The
            // reverse order (GATE, then SERIAL) deadlocked against such a test.
            let installed = settings::testing::install(Settings::new());
            let gate = GATE.write().unwrap_or_else(PoisonError::into_inner);
            *held = Some(Held::Exclusive {
                _installed: installed,
                _gate: gate,
            });
        });
    }

    /// A temporary directory standing in for `~/.nucleos`, with an EMPTY `Settings` as the process handle and the
    /// test thread holding the settings exclusively. The existing fixtures kept a `TempDir` for exactly this;
    /// returning a `Home` instead keeps every `home.path()` compiling. `Home::new()` always starts from an empty
    /// snapshot, even when the thread already published something before.
    pub struct Home {
        dir: tempfile::TempDir,
    }

    impl Home {
        pub fn new() -> Home {
            exclusive();
            let _ = settings::set_global(Settings::new());
            Home {
                dir: tempfile::tempdir().expect("create a stand-in home"),
            }
        }
    }

    impl Default for Home {
        fn default() -> Self {
            Self::new()
        }
    }

    impl Deref for Home {
        type Target = tempfile::TempDir;
        fn deref(&self) -> &tempfile::TempDir {
            &self.dir
        }
    }

    /// Replaces the process snapshot by an edited copy of itself. Takes the exclusive hold first, so a fixture can
    /// publish before or after the module's `test_state()` (which only holds shared).
    fn edit(change: impl FnOnce(Snapshot) -> Snapshot) {
        exclusive();
        let current = settings::global().snapshot();
        settings::set_global(Settings::from_snapshot(change((*current).clone())));
    }

    /// Stores `yaml` as the project's `autopilot` document, parsed and range-checked exactly like the file was.
    pub fn publish_rules(project_id: &str, yaml: &str) {
        let rules = crate::config::parse_schedule_rules(yaml).expect("a fixture's rules parse");
        edit(|snapshot| snapshot.with_document(&Scope::project(project_id), rules));
    }

    /// Stores `yaml` as the project's `workflow_pins` document, parsed and checked exactly like the file was.
    pub fn publish_pins(project_id: &str, yaml: &str) {
        let pins = crate::workflows::parse_pins(yaml).expect("a fixture's pins parse");
        edit(|snapshot| snapshot.with_document(&Scope::project(project_id), pins));
    }

    /// Makes the document absent (the old "file does not exist").
    pub fn clear(project_id: &str, name: &str) {
        edit(|mut snapshot| {
            snapshot.remove(&Scope::project(project_id), name);
            snapshot
        });
    }

    /// Records the document as stored-but-unreadable (the old "file does not parse").
    pub fn publish_unreadable(project_id: &str, name: &str, reason: &str) {
        edit(|mut snapshot| {
            snapshot.mark_unreadable(settings::Unreadable {
                scope: Scope::project(project_id),
                name: name.to_string(),
                version: 1,
                reason: reason.to_string(),
            });
            snapshot
        });
    }
}
