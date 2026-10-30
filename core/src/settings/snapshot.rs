//! The in-memory view of the `settings` table. Readers take an `Arc<Snapshot>` and never block; a write builds
//! a changed copy and swaps it in AFTER its transaction committed. RED stub: types are real, behaviour is not.

use std::collections::BTreeMap;
use std::sync::{Arc, RwLock};

use serde_json::Value;

use super::decode::Erased;
use super::registry::Document;
use super::scope::Scope;

/// One stored document as the daemon sees it.
#[derive(Clone)]
pub struct Entry {
    pub version: i64,
    pub updated_at: String,
    pub updated_by: String,
    /// The stored JSON text (what a client is shown).
    pub canonical: Arc<str>,
    /// The decoded value, type-erased; [`Snapshot::get_opt`] downcasts it.
    pub value: Erased,
}

impl Entry {
    /// The stored body as JSON; `Null` if the text cannot be parsed.
    pub fn body(&self) -> Value {
        todo!("RED stub: Entry::body")
    }
}

/// A stored document this daemon could not decode. Readers see the default meanwhile.
#[derive(Debug, Clone, PartialEq)]
pub struct Unreadable {
    pub scope: Scope,
    pub name: String,
    pub version: i64,
    pub reason: String,
}

/// Every stored document, by scope then name, plus the ones that could not be read.
#[derive(Clone, Default)]
pub struct Snapshot {
    #[allow(dead_code)] // RED stub: read by the GREEN implementation
    entries: BTreeMap<Scope, BTreeMap<String, Entry>>,
    #[allow(dead_code)] // RED stub: read by the GREEN implementation
    unreadable: Vec<Unreadable>,
}

impl Snapshot {
    /// The document `T` at `scope`, if one is stored and readable.
    pub fn get_opt<T: Document>(&self, _scope: &Scope) -> Option<Arc<T>> {
        todo!("RED stub: Snapshot::get_opt")
    }

    /// The document `T` at `scope`, or its default.
    pub fn get<T: Document + Default>(&self, _scope: &Scope) -> Arc<T> {
        todo!("RED stub: Snapshot::get")
    }

    pub fn entry(&self, _scope: &Scope, _name: &str) -> Option<&Entry> {
        todo!("RED stub: Snapshot::entry")
    }

    pub fn version(&self, _scope: &Scope, _name: &str) -> Option<i64> {
        todo!("RED stub: Snapshot::version")
    }

    /// Every readable document, scope by scope, name by name.
    pub fn iter(&self) -> impl Iterator<Item = (&Scope, &str, &Entry)> {
        let never: std::iter::Empty<(&Scope, &str, &Entry)> = todo!("RED stub: Snapshot::iter");
        never
    }

    pub fn unreadable(&self) -> &[Unreadable] {
        todo!("RED stub: Snapshot::unreadable")
    }

    pub fn unreadable_for(&self, _scope: &Scope, _name: &str) -> Option<&Unreadable> {
        todo!("RED stub: Snapshot::unreadable_for")
    }

    pub fn is_unreadable(&self, _scope: &Scope, _name: &str) -> bool {
        todo!("RED stub: Snapshot::is_unreadable")
    }

    /// Store `entry`, and forget any earlier "unreadable" record for the same document.
    pub(crate) fn insert(&mut self, _scope: &Scope, _name: &str, _entry: Entry) {
        todo!("RED stub: Snapshot::insert")
    }

    pub(crate) fn remove(&mut self, _scope: &Scope, _name: &str) {
        todo!("RED stub: Snapshot::remove")
    }

    /// Record that a stored document cannot be read; readers fall back to the default.
    pub(crate) fn mark_unreadable(&mut self, _record: Unreadable) {
        todo!("RED stub: Snapshot::mark_unreadable")
    }

    /// A snapshot holding `value` at `scope` as version 1, for a test that needs a reader to see a document.
    #[cfg(any(test, feature = "testkit"))]
    pub fn with_document<T: Document>(self, _scope: &Scope, _value: T) -> Self {
        todo!("RED stub: Snapshot::with_document")
    }
}

/// A cloneable handle on the current snapshot and the write lock. The daemon has one (see `settings::global`);
/// a store test builds its own so it never touches the process-wide one.
#[derive(Clone, Default)]
pub struct Settings {
    #[allow(dead_code)] // RED stub: read by the GREEN implementation
    inner: Arc<Inner>,
}

#[derive(Default)]
struct Inner {
    #[allow(dead_code)] // RED stub
    current: RwLock<Arc<Snapshot>>,
    #[allow(dead_code)] // RED stub
    write: tokio::sync::Mutex<()>,
}

impl Settings {
    pub fn new() -> Self {
        todo!("RED stub: Settings::new")
    }

    pub fn from_snapshot(_snapshot: Snapshot) -> Self {
        todo!("RED stub: Settings::from_snapshot")
    }

    /// The current snapshot. Cheap: clones an `Arc`.
    pub fn snapshot(&self) -> Arc<Snapshot> {
        todo!("RED stub: Settings::snapshot")
    }

    pub(super) fn swap(&self, _snapshot: Snapshot) {
        todo!("RED stub: Settings::swap")
    }

    /// Held for the whole of a write, from the first read inside the transaction to the swap after the commit.
    #[allow(dead_code)] // RED stub: used by store.rs in GREEN
    pub(super) async fn lock_writes(&self) -> tokio::sync::MutexGuard<'_, ()> {
        todo!("RED stub: Settings::lock_writes")
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::config::VerifyConfig;

    fn entry(version: i64, config: VerifyConfig) -> Entry {
        Entry {
            version,
            updated_at: "2026-10-11T00:00:00.000Z".to_owned(),
            updated_by: "test".to_owned(),
            canonical: Arc::from(serde_json::to_string(&config).unwrap()),
            value: Arc::new(config),
        }
    }

    fn verify(capacity: u32) -> VerifyConfig {
        VerifyConfig { capacity, ..VerifyConfig::default() }
    }

    #[test]
    fn an_empty_snapshot_answers_with_the_default() {
        let snapshot = Snapshot::default();
        assert!(snapshot.get_opt::<VerifyConfig>(&Scope::Machine).is_none());
        assert_eq!(snapshot.get::<VerifyConfig>(&Scope::Machine).capacity, 4);
        assert_eq!(snapshot.version(&Scope::Machine, "verify"), None);
    }

    #[test]
    fn an_inserted_document_is_read_back_by_type_scope_and_name() {
        let mut snapshot = Snapshot::default();
        snapshot.insert(&Scope::Machine, "verify", entry(3, verify(9)));
        assert_eq!(snapshot.get::<VerifyConfig>(&Scope::Machine).capacity, 9);
        assert_eq!(snapshot.version(&Scope::Machine, "verify"), Some(3));
        assert!(snapshot.get_opt::<VerifyConfig>(&Scope::project("p1")).is_none(), "another scope is empty");
        let (scope, name, found) = snapshot.iter().next().expect("one entry");
        assert_eq!((scope, name, found.version), (&Scope::Machine, "verify", 3));
        assert_eq!(found.body()["capacity"], 9);
    }

    #[test]
    fn a_name_with_the_wrong_type_is_not_found() {
        let mut snapshot = Snapshot::default();
        snapshot.insert(&Scope::Machine, "verify", entry(1, verify(9)));
        assert!(snapshot.get_opt::<crate::config::WebConfig>(&Scope::Machine).is_none());
    }

    #[test]
    fn a_snapshot_already_handed_out_does_not_change() {
        let settings = Settings::new();
        let before = settings.snapshot();
        let mut next = (*before).clone();
        next.insert(&Scope::Machine, "verify", entry(1, verify(9)));
        settings.swap(next);
        assert!(before.get_opt::<VerifyConfig>(&Scope::Machine).is_none(), "the old Arc is a frozen view");
        assert_eq!(settings.snapshot().get::<VerifyConfig>(&Scope::Machine).capacity, 9);
    }

    #[test]
    fn an_unreadable_mark_removes_the_entry_and_a_fresh_insert_clears_it() {
        let mut snapshot = Snapshot::default();
        snapshot.insert(&Scope::Machine, "verify", entry(1, verify(9)));
        snapshot.mark_unreadable(Unreadable {
            scope: Scope::Machine,
            name: "verify".to_owned(),
            version: 2,
            reason: "bad".to_owned(),
        });
        assert!(snapshot.is_unreadable(&Scope::Machine, "verify"));
        assert_eq!(snapshot.unreadable().len(), 1);
        assert_eq!(snapshot.get::<VerifyConfig>(&Scope::Machine).capacity, 4, "falls back to the default");

        snapshot.insert(&Scope::Machine, "verify", entry(3, verify(5)));
        assert!(!snapshot.is_unreadable(&Scope::Machine, "verify"));
        assert!(snapshot.unreadable().is_empty());
    }

    #[test]
    fn removing_the_last_entry_of_a_scope_leaves_nothing_behind() {
        let mut snapshot = Snapshot::default();
        snapshot.insert(&Scope::Machine, "verify", entry(1, verify(9)));
        snapshot.remove(&Scope::Machine, "verify");
        assert!(snapshot.entry(&Scope::Machine, "verify").is_none());
        assert_eq!(snapshot.iter().count(), 0);
    }

    #[test]
    fn removing_an_unreadable_document_forgets_that_it_was_unreadable() {
        let mut snapshot = Snapshot::default();
        snapshot.mark_unreadable(Unreadable {
            scope: Scope::Machine,
            name: "verify".to_owned(),
            version: 2,
            reason: "bad".to_owned(),
        });
        snapshot.remove(&Scope::Machine, "verify");
        assert!(!snapshot.is_unreadable(&Scope::Machine, "verify"));
        assert!(snapshot.unreadable().is_empty());
    }

    #[test]
    fn with_document_builds_a_snapshot_for_a_test() {
        let snapshot = Snapshot::default().with_document(&Scope::Machine, verify(7));
        assert_eq!(snapshot.get::<VerifyConfig>(&Scope::Machine).capacity, 7);
        assert_eq!(snapshot.version(&Scope::Machine, "verify"), Some(1));
    }
}
