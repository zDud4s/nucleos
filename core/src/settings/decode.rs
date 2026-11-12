//! Two ways to read a document. A WRITE is strict: an unknown field is an error, so a typo is refused instead of
//! silently dropped. A READ of a stored row is lenient: a key this daemon's struct does not know (a row written
//! by a newer daemon) is removed and the rest is kept, so an upgrade followed by a rollback does not brick a
//! document. Neither loosens the structs' own `deny_unknown_fields`.

use std::any::Any;
use std::sync::Arc;

use serde_json::Value;

use super::registry::Document;

/// A decoded document with its type forgotten, so one table can hold all of them.
pub type Erased = Arc<dyn Any + Send + Sync>;

/// A body that passed validation: the text to store and the value to publish in the snapshot.
#[derive(Clone)]
pub struct Prepared {
    pub canonical: String,
    pub value: Erased,
}

impl std::fmt::Debug for Prepared {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Prepared")
            .field("canonical", &self.canonical)
            .finish_non_exhaustive()
    }
}

/// How many unknown keys one stored row may lose before it is called unreadable.
const MAX_STRIPPED_KEYS: usize = 16;

/// Decode `body` as `T` (unknown fields refused) and run `T::finish`.
pub fn strict<T: Document>(body: &Value) -> Result<T, String> {
    let typed: T = serde_json::from_value(body.clone()).map_err(|error| error.to_string())?;
    typed.finish()
}

/// [`strict`], plus the canonical text and the type-erased value.
pub fn prepare<T: Document>(body: &Value) -> Result<Prepared, String> {
    let typed = strict::<T>(body)?;
    let canonical = serde_json::to_string(&typed).map_err(|error| error.to_string())?;
    Ok(Prepared {
        canonical,
        value: Arc::new(typed),
    })
}

/// Decode stored text as `T`, dropping keys `T` does not know. `finish` is NOT applied: a row written under
/// older rules must stay readable; the next write validates it again.
pub fn load<T: Document>(text: &str) -> Result<Erased, String> {
    let json: Value = serde_json::from_str(text).map_err(|error| error.to_string())?;
    let typed: T = tolerant(json)?;
    Ok(Arc::new(typed))
}

/// Decode `json` as `T`, dropping keys `T` does not know (the lenient read, for any deserializable type).
pub fn tolerant<T: serde::de::DeserializeOwned>(mut json: Value) -> Result<T, String> {
    for _ in 0..=MAX_STRIPPED_KEYS {
        match serde_json::from_value::<T>(json.clone()) {
            Ok(typed) => return Ok(typed),
            Err(error) => {
                let message = error.to_string();
                let Some(key) = unknown_field(&message) else {
                    return Err(message);
                };
                if !strip_key(&mut json, &key) {
                    return Err(message);
                }
            }
        }
    }
    Err("too many unknown fields".to_owned())
}

/// Remove the first object entry called `key`, outermost first. True when one was removed.
fn strip_key(value: &mut Value, key: &str) -> bool {
    match value {
        Value::Object(map) => {
            if map.remove(key).is_some() {
                return true;
            }
            map.values_mut().any(|inner| strip_key(inner, key))
        }
        Value::Array(items) => items.iter_mut().any(|inner| strip_key(inner, key)),
        _ => false,
    }
}

/// The key out of serde's ``unknown field `key`, expected ...`` message.
fn unknown_field(message: &str) -> Option<String> {
    let rest = message.strip_prefix("unknown field `")?;
    let end = rest.find('`')?;
    Some(rest[..end].to_owned())
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use serde::{Deserialize, Serialize};
    use serde_json::json;

    use super::*;
    use crate::settings::registry::Document;
    use crate::settings::scope::Kind;

    #[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
    #[serde(default, deny_unknown_fields)]
    struct Sample {
        limit: u32,
        tags: Vec<String>,
        inner: SampleInner,
    }

    #[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
    #[serde(default, deny_unknown_fields)]
    struct SampleInner {
        depth: u8,
    }

    impl Document for Sample {
        const NAME: &'static str = "sample";
        const KIND: Kind = Kind::Machine;
        fn finish(self) -> Result<Self, String> {
            if self.limit > 100 {
                return Err("limit must be at most 100".to_owned());
            }
            Ok(self)
        }
    }

    fn typed<T: Send + Sync + 'static>(erased: Erased) -> Arc<T> {
        erased
            .downcast::<T>()
            .expect("the document has the expected type")
    }

    #[test]
    fn strict_refuses_an_unknown_field() {
        let why = strict::<Sample>(&json!({"limit": 1, "gone": true})).unwrap_err();
        assert!(why.contains("unknown field `gone`"), "{why}");
        let nested = strict::<Sample>(&json!({"inner": {"depth": 1, "gone": true}})).unwrap_err();
        assert!(nested.contains("unknown field `gone`"), "{nested}");
    }

    #[test]
    fn strict_applies_finish() {
        assert_eq!(strict::<Sample>(&json!({"limit": 7})).unwrap().limit, 7);
        assert_eq!(
            strict::<Sample>(&json!({"limit": 101})).unwrap_err(),
            "limit must be at most 100"
        );
    }

    #[test]
    fn prepare_writes_the_canonical_text_with_defaults_filled_in() {
        let prepared = prepare::<Sample>(&json!({"limit": 3})).unwrap();
        assert_eq!(
            prepared.canonical,
            r#"{"limit":3,"tags":[],"inner":{"depth":0}}"#
        );
        assert_eq!(typed::<Sample>(prepared.value).limit, 3);
    }

    #[test]
    fn preparing_a_canonical_body_again_changes_nothing() {
        let first = prepare::<Sample>(&json!({"tags": ["b", "a"]})).unwrap();
        let again: serde_json::Value = serde_json::from_str(&first.canonical).unwrap();
        assert_eq!(
            prepare::<Sample>(&again).unwrap().canonical,
            first.canonical
        );
    }

    #[test]
    fn load_tolerates_unknown_keys_at_any_depth() {
        let text = r#"{"limit": 4, "gone": 1, "inner": {"depth": 2, "also_gone": [1]}}"#;
        let sample = typed::<Sample>(load::<Sample>(text).unwrap());
        assert_eq!(sample.limit, 4);
        assert_eq!(sample.inner.depth, 2);
    }

    #[test]
    fn load_does_not_apply_finish() {
        // A row written by an older daemon under looser rules must still be readable.
        let sample = typed::<Sample>(load::<Sample>(r#"{"limit": 500}"#).unwrap());
        assert_eq!(sample.limit, 500);
    }

    #[test]
    fn load_reports_a_type_error_and_bad_json() {
        let Err(why) = load::<Sample>(r#"{"limit": "many"}"#) else {
            panic!("a type error")
        };
        assert!(why.contains("invalid type"), "{why}");
        assert!(load::<Sample>("{not json").is_err());
    }

    #[test]
    fn load_gives_up_after_sixteen_unknown_keys() {
        let keys: Vec<String> = (0..20).map(|n| format!("\"k{n}\": 1")).collect();
        let text = format!("{{{}}}", keys.join(","));
        assert_eq!(
            load::<Sample>(&text).err().as_deref(),
            Some("too many unknown fields")
        );
    }

    #[test]
    fn unknown_field_reads_the_key_out_of_serdes_message() {
        assert_eq!(
            unknown_field("unknown field `gone`, expected one of `limit`, `tags`").as_deref(),
            Some("gone")
        );
        assert_eq!(
            unknown_field("unknown field `x`, there are no fields").as_deref(),
            Some("x")
        );
        assert_eq!(unknown_field("invalid type: string \"many\""), None);
    }
}
