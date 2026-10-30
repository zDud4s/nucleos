//! The in-memory view of the `settings` table. Readers take an `Arc<Snapshot>` and never block; a write builds
//! a changed copy and swaps it in AFTER its transaction committed.

use std::collections::BTreeMap;
use std::sync::{Arc, PoisonError, RwLock};

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
        serde_json::from_str(&self.canonical).unwrap_or(Value::Null)
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
    entries: BTreeMap<Scope, BTreeMap<String, Entry>>,
    unreadable: Vec<Unreadable>,
}

impl Snapshot {
    /// The document `T` at `scope`, if one is stored and readable.
    pub fn get_opt<T: Document>(&self, scope: &Scope) -> Option<Arc<T>> {
        let entry = self.entry(scope, T::NAME)?;
        Arc::clone(&entry.value).downcast::<T>().ok()
    }

    /// The document `T` at `scope`, or its default.
    pub fn get<T: Document + Default>(&self, scope: &Scope) -> Arc<T> {
        self.get_opt(scope)
            .unwrap_or_else(|| Arc::new(T::default()))
    }

    pub fn entry(&self, scope: &Scope, name: &str) -> Option<&Entry> {
        self.entries.get(scope)?.get(name)
    }

    /// The stored value of `name` at `scope` as `T`, when it is readable and of that type. For documents
    /// that have no static `Document::NAME` (the `channels/<id>` family).
    pub fn entry_as<T: 'static>(&self, scope: &Scope, name: &str) -> Option<&T> {
        self.entry(scope, name)?.value.downcast_ref::<T>()
    }

    pub fn version(&self, scope: &Scope, name: &str) -> Option<i64> {
        self.entry(scope, name).map(|entry| entry.version)
    }

    /// Every readable document, scope by scope, name by name.
    pub fn iter(&self) -> impl Iterator<Item = (&Scope, &str, &Entry)> {
        self.entries.iter().flat_map(|(scope, docs)| {
            docs.iter()
                .map(move |(name, entry)| (scope, name.as_str(), entry))
        })
    }

    pub fn unreadable(&self) -> &[Unreadable] {
        &self.unreadable
    }

    pub fn unreadable_for(&self, scope: &Scope, name: &str) -> Option<&Unreadable> {
        self.unreadable
            .iter()
            .find(|record| record.scope == *scope && record.name == name)
    }

    pub fn is_unreadable(&self, scope: &Scope, name: &str) -> bool {
        self.unreadable_for(scope, name).is_some()
    }

    /// Store `entry`, and forget any earlier "unreadable" record for the same document.
    pub(crate) fn insert(&mut self, scope: &Scope, name: &str, entry: Entry) {
        self.clear_unreadable(scope, name);
        self.entries
            .entry(scope.clone())
            .or_default()
            .insert(name.to_owned(), entry);
    }

    pub(crate) fn remove(&mut self, scope: &Scope, name: &str) {
        // A deleted document is no longer unreadable either: forget both.
        self.clear_unreadable(scope, name);
        if let Some(docs) = self.entries.get_mut(scope) {
            docs.remove(name);
            if docs.is_empty() {
                self.entries.remove(scope);
            }
        }
    }

    /// Record that a stored document cannot be read; readers fall back to the default.
    pub(crate) fn mark_unreadable(&mut self, record: Unreadable) {
        // `remove` also forgets an earlier unreadable record, so the list never holds two for one document.
        self.remove(&record.scope, &record.name);
        self.unreadable.push(record);
    }

    fn clear_unreadable(&mut self, scope: &Scope, name: &str) {
        self.unreadable
            .retain(|record| !(record.scope == *scope && record.name == name));
    }

    /// A snapshot holding `value` at `scope` as version 1, for a test that needs a reader to see a document.
    #[cfg(any(test, feature = "testkit"))]
    pub fn with_document<T: Document>(mut self, scope: &Scope, value: T) -> Self {
        let canonical = serde_json::to_string(&value).unwrap_or_default();
        self.insert(
            scope,
            T::NAME,
            Entry {
                version: 1,
                updated_at: "1970-01-01T00:00:00.000Z".to_owned(),
                updated_by: "test".to_owned(),
                canonical: Arc::from(canonical),
                value: Arc::new(value),
            },
        );
        self
    }
}

/// A cloneable handle on the current snapshot and the write lock. The daemon has one (see `settings::global`);
/// a store test builds its own so it never touches the process-wide one.
#[derive(Clone, Default)]
pub struct Settings {
    inner: Arc<Inner>,
}

#[derive(Default)]
struct Inner {
    current: RwLock<Arc<Snapshot>>,
    write: tokio::sync::Mutex<()>,
}

impl Settings {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn from_snapshot(snapshot: Snapshot) -> Self {
        let settings = Self::new();
        settings.swap(snapshot);
        settings
    }

    /// The current snapshot. Cheap: clones an `Arc`.
    pub fn snapshot(&self) -> Arc<Snapshot> {
        Arc::clone(
            &self
                .inner
                .current
                .read()
                .unwrap_or_else(PoisonError::into_inner),
        )
    }

    pub(super) fn swap(&self, snapshot: Snapshot) {
        *self
            .inner
            .current
            .write()
            .unwrap_or_else(PoisonError::into_inner) = Arc::new(snapshot);
    }

    /// Held for the whole of a write, from the first read inside the transaction to the swap after the commit.
    pub(super) async fn lock_writes(&self) -> tokio::sync::MutexGuard<'_, ()> {
        self.inner.write.lock().await
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
        VerifyConfig {
            capacity,
            ..VerifyConfig::default()
        }
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
        assert!(
            snapshot
                .get_opt::<VerifyConfig>(&Scope::project("p1"))
                .is_none(),
            "another scope is empty"
        );
        let (scope, name, found) = snapshot.iter().next().expect("one entry");
        assert_eq!((scope, name, found.version), (&Scope::Machine, "verify", 3));
        assert_eq!(found.body()["capacity"], 9);
    }

    #[test]
    fn a_name_with_the_wrong_type_is_not_found() {
        let mut snapshot = Snapshot::default();
        snapshot.insert(&Scope::Machine, "verify", entry(1, verify(9)));
        assert!(
            snapshot
                .get_opt::<crate::config::WebConfig>(&Scope::Machine)
                .is_none()
        );
    }

    #[test]
    fn a_snapshot_already_handed_out_does_not_change() {
        let settings = Settings::new();
        let before = settings.snapshot();
        let mut next = (*before).clone();
        next.insert(&Scope::Machine, "verify", entry(1, verify(9)));
        settings.swap(next);
        assert!(
            before.get_opt::<VerifyConfig>(&Scope::Machine).is_none(),
            "the old Arc is a frozen view"
        );
        assert_eq!(
            settings
                .snapshot()
                .get::<VerifyConfig>(&Scope::Machine)
                .capacity,
            9
        );
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
        assert_eq!(
            snapshot.get::<VerifyConfig>(&Scope::Machine).capacity,
            4,
            "falls back to the default"
        );

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
