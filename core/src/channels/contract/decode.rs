//! Strict decoding of the adapter contract (Anexo C, T12): two stages. The structural stage is a
//! "raw" serde struct with `deny_unknown_fields` (no unions; every optional field optional) after
//! a duplicate-key pass; the semantic stage is a hand-written `validate`, so the error codes and
//! pointers (C.2.1) never depend on serde's messages. No regex: the T7 grammars are plain loops.
//!
//! This file holds the engine and the shapes `hello`, `hello_response`, `error_response`,
//! `media_response`, `ack`, `ack_response` and `renew_response` (D1). The wire types here are
//! their own (R9): they are both the raw input and the validated output, and encode back to the
//! canonical form (T6: values at their default are omitted).

use std::collections::HashSet;
use std::fmt;

use chrono::DateTime;
use serde::de::{self, DeserializeOwned, DeserializeSeed, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Serialize};

use super::{CONTRACT_VERSION, MAX_RETRY_AFTER_MS, MediaKind, parse_version, valid_channel_id};

// ---- errors -------------------------------------------------------------------------------

/// The validation codes of Anexo C, C.2.1 (wire: snake_case).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    Decode,
    Grammar,
    Range,
    Empty,
    RequiredIf,
    Forbidden,
    OneOf,
    Unique,
    Value,
}

impl ErrorCode {
    pub fn wire(self) -> &'static str {
        match self {
            Self::Decode => "decode",
            Self::Grammar => "grammar",
            Self::Range => "range",
            Self::Empty => "empty",
            Self::RequiredIf => "required_if",
            Self::Forbidden => "forbidden",
            Self::OneOf => "one_of",
            Self::Unique => "unique",
            Self::Value => "value",
        }
    }
}

/// `at` is a JSON pointer; `Some("")` (the document) differs from `None` (no pointer, E11).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContractError {
    pub code: ErrorCode,
    pub at: Option<String>,
}

impl ContractError {
    pub fn new(code: ErrorCode, at: impl Into<String>) -> Self {
        Self {
            code,
            at: Some(at.into()),
        }
    }

    /// A structural failure: serde gives no pointer.
    pub fn decode() -> Self {
        Self {
            code: ErrorCode::Decode,
            at: None,
        }
    }
}

impl fmt::Display for ContractError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.at {
            Some(at) => write!(f, "{} at \"{at}\"", self.code.wire()),
            None => f.write_str(self.code.wire()),
        }
    }
}

impl std::error::Error for ContractError {}

/// The body of every 4xx of the channel routes (T9).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ErrorResponse {
    pub error: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contract_version: Option<String>,
}

impl From<ContractError> for ErrorResponse {
    fn from(err: ContractError) -> Self {
        Self {
            error: err.code.wire().to_string(),
            at: err.at,
            detail: None,
            contract_version: None,
        }
    }
}

impl ErrorResponse {
    /// The 409 of a `hello` whose contract version is incompatible (§5.1).
    pub fn contract_version(hello: &AdapterHello) -> Self {
        Self {
            error: "contract_version".to_string(),
            at: None,
            detail: Some(format!("adapter speaks {}", hello.version)),
            contract_version: Some(CONTRACT_VERSION.to_string()),
        }
    }
}

// ---- the engine ---------------------------------------------------------------------------

/// Ties a raw wire type to its hand-written `validate`.
pub trait Shape {
    /// The raw struct: `deny_unknown_fields`, deserialized as is.
    type Wire: DeserializeOwned;
    /// What a successful decode returns.
    type Valid;
    fn validate(wire: Self::Wire) -> Result<Self::Valid, ContractError>;
}

/// Stage 0: refuses duplicate object keys at any depth (serde keeps the last one silently).
#[derive(Clone, Copy)]
struct NoDuplicateKeys;

impl<'de> DeserializeSeed<'de> for NoDuplicateKeys {
    type Value = ();

    fn deserialize<D: de::Deserializer<'de>>(self, deserializer: D) -> Result<(), D::Error> {
        deserializer.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for NoDuplicateKeys {
    type Value = ();

    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("any JSON value")
    }

    fn visit_bool<E: de::Error>(self, _: bool) -> Result<(), E> {
        Ok(())
    }

    fn visit_i64<E: de::Error>(self, _: i64) -> Result<(), E> {
        Ok(())
    }

    fn visit_u64<E: de::Error>(self, _: u64) -> Result<(), E> {
        Ok(())
    }

    fn visit_f64<E: de::Error>(self, _: f64) -> Result<(), E> {
        Ok(())
    }

    fn visit_str<E: de::Error>(self, _: &str) -> Result<(), E> {
        Ok(())
    }

    fn visit_unit<E: de::Error>(self) -> Result<(), E> {
        Ok(())
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<(), A::Error> {
        while seq.next_element_seed(NoDuplicateKeys)?.is_some() {}
        Ok(())
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<(), A::Error> {
        let mut seen: HashSet<String> = HashSet::new();
        while let Some(key) = map.next_key::<String>()? {
            if !seen.insert(key) {
                return Err(<A::Error as de::Error>::custom("duplicate key"));
            }
            map.next_value_seed(NoDuplicateKeys)?;
        }
        Ok(())
    }
}

fn no_duplicate_keys(bytes: &[u8]) -> Result<(), serde_json::Error> {
    let mut de = serde_json::Deserializer::from_slice(bytes);
    NoDuplicateKeys.deserialize(&mut de)?;
    de.end()
}

/// Strict two-stage decode (T12). Every structural failure is `decode` with no pointer.
pub fn decode<T: Shape>(bytes: &[u8]) -> Result<T::Valid, ContractError> {
    no_duplicate_keys(bytes).map_err(|_| ContractError::decode())?;
    let wire: T::Wire = serde_json::from_slice(bytes).map_err(|_| ContractError::decode())?;
    T::validate(wire)
}

// ---- T7 grammars, by hand -----------------------------------------------------------------

fn check(ok: bool, code: ErrorCode, at: &str) -> Result<(), ContractError> {
    if ok {
        Ok(())
    } else {
        Err(ContractError::new(code, at))
    }
}

/// `^[a-z][a-z0-9_]{0,max-1}$` (`reason`, `error`).
fn snake_ident(s: &str, max: usize) -> bool {
    let b = s.as_bytes();
    !b.is_empty()
        && b.len() <= max
        && b[0].is_ascii_lowercase()
        && b[1..]
            .iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'_')
}

/// `^m_[0-9a-z]{1,30}$`.
fn valid_media_id(s: &str) -> bool {
    match s.strip_prefix("m_") {
        Some(tail) => {
            (1..=30).contains(&tail.len())
                && tail
                    .bytes()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        }
        None => false,
    }
}

/// `^(/[^/]*)*$`: the empty string, or anything that starts with `/`.
fn valid_pointer(s: &str) -> bool {
    s.is_empty() || s.starts_with('/')
}

fn valid_rfc3339(s: &str) -> bool {
    DateTime::parse_from_rfc3339(s).is_ok()
}

/// A free ref (1-256 bytes): empty is `empty`, too long is `range`.
fn check_ref(s: &str, at: &str) -> Result<(), ContractError> {
    check(!s.is_empty(), ErrorCode::Empty, at)?;
    check(s.len() <= 256, ErrorCode::Range, at)
}

fn is_false(b: &bool) -> bool {
    !*b
}

// ---- hello --------------------------------------------------------------------------------

/// `max_text` bounds (T8).
const MAX_TEXT_CEILING: u64 = 1_000_000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdapterHello {
    pub kind: String,
    pub version: String,
    pub capabilities: AdapterCapabilities,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdapterCapabilities {
    #[serde(default, skip_serializing_if = "is_false")]
    pub buttons: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub edit: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub typing: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub threads: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub reply_to: bool,
    pub max_text: u64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub media_in: Vec<MediaKind>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub media_out: Vec<MediaKind>,
}

fn check_unique_kinds(list: &[MediaKind], at: &str) -> Result<(), ContractError> {
    for (i, kind) in list.iter().enumerate() {
        if list[..i].contains(kind) {
            return Err(ContractError::new(ErrorCode::Unique, format!("{at}/{i}")));
        }
    }
    Ok(())
}

/// Shape `hello`.
pub struct HelloShape;

impl Shape for HelloShape {
    type Wire = AdapterHello;
    type Valid = AdapterHello;

    fn validate(hello: AdapterHello) -> Result<AdapterHello, ContractError> {
        check(valid_channel_id(&hello.kind), ErrorCode::Grammar, "/kind")?;
        check(
            parse_version(&hello.version).is_some(),
            ErrorCode::Grammar,
            "/version",
        )?;
        let caps = &hello.capabilities;
        check(
            (1..=MAX_TEXT_CEILING).contains(&caps.max_text),
            ErrorCode::Range,
            "/capabilities/max_text",
        )?;
        check_unique_kinds(&caps.media_in, "/capabilities/media_in")?;
        check_unique_kinds(&caps.media_out, "/capabilities/media_out")?;
        Ok(hello)
    }
}

/// Shape `hello_response`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HelloAnswer {
    pub contract_version: String,
}

pub struct HelloResponseShape;

impl Shape for HelloResponseShape {
    type Wire = HelloAnswer;
    type Valid = HelloAnswer;

    fn validate(answer: HelloAnswer) -> Result<HelloAnswer, ContractError> {
        check(
            parse_version(&answer.contract_version).is_some(),
            ErrorCode::Grammar,
            "/contract_version",
        )?;
        Ok(answer)
    }
}

// ---- error_response -----------------------------------------------------------------------

pub struct ErrorResponseShape;

impl Shape for ErrorResponseShape {
    type Wire = ErrorResponse;
    type Valid = ErrorResponse;

    fn validate(body: ErrorResponse) -> Result<ErrorResponse, ContractError> {
        check(snake_ident(&body.error, 64), ErrorCode::Grammar, "/error")?;
        if let Some(at) = &body.at {
            check(valid_pointer(at), ErrorCode::Grammar, "/at")?;
        }
        if let Some(version) = &body.contract_version {
            check(
                parse_version(version).is_some(),
                ErrorCode::Grammar,
                "/contract_version",
            )?;
        }
        Ok(body)
    }
}

// ---- media_response -----------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MediaAnswer {
    pub media_id: String,
}

pub struct MediaResponseShape;

impl Shape for MediaResponseShape {
    type Wire = MediaAnswer;
    type Valid = MediaAnswer;

    fn validate(answer: MediaAnswer) -> Result<MediaAnswer, ContractError> {
        check(
            valid_media_id(&answer.media_id),
            ErrorCode::Grammar,
            "/media_id",
        )?;
        Ok(answer)
    }
}

// ---- ack ----------------------------------------------------------------------------------

/// The raw ack has no union: `ok` and `failed` are both optional and `validate` demands exactly
/// one. An empty `platform_refs` is the default and is omitted on encode.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AckBody {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ok: Option<bool>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub platform_refs: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failed: Option<AckFailure>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AckFailure {
    pub retryable: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_after_ms: Option<u64>,
    pub reason: String,
}

pub struct AckShape;

impl Shape for AckShape {
    type Wire = AckBody;
    type Valid = AckBody;

    fn validate(ack: AckBody) -> Result<AckBody, ContractError> {
        if ack.ok.is_some() == ack.failed.is_some() {
            return Err(ContractError::new(ErrorCode::OneOf, ""));
        }
        if let Some(ok) = ack.ok {
            check(ok, ErrorCode::Value, "/ok")?;
            for (i, reference) in ack.platform_refs.iter().enumerate() {
                check_ref(reference, &format!("/platform_refs/{i}"))?;
            }
        }
        if let Some(failed) = &ack.failed {
            check(
                ack.platform_refs.is_empty(),
                ErrorCode::Forbidden,
                "/platform_refs",
            )?;
            if let Some(delay) = failed.retry_after_ms {
                check(
                    failed.retryable,
                    ErrorCode::Forbidden,
                    "/failed/retry_after_ms",
                )?;
                check(
                    delay <= MAX_RETRY_AFTER_MS,
                    ErrorCode::Range,
                    "/failed/retry_after_ms",
                )?;
            }
            check(
                snake_ident(&failed.reason, 64),
                ErrorCode::Grammar,
                "/failed/reason",
            )?;
        }
        Ok(ack)
    }
}

// ---- ack_response, renew_response ---------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AckState {
    Pending,
    Done,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AckAnswer {
    pub state: AckState,
    #[serde(default, skip_serializing_if = "is_false")]
    pub ignored: bool,
}

pub struct AckResponseShape;

impl Shape for AckResponseShape {
    type Wire = AckAnswer;
    type Valid = AckAnswer;

    /// `ignored: true` only marks a late ack on an action already `done` or `failed`.
    fn validate(answer: AckAnswer) -> Result<AckAnswer, ContractError> {
        check(
            !(answer.ignored && answer.state == AckState::Pending),
            ErrorCode::Forbidden,
            "/ignored",
        )?;
        Ok(answer)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RenewAnswer {
    pub lease_until: String,
}

pub struct RenewResponseShape;

impl Shape for RenewResponseShape {
    type Wire = RenewAnswer;
    type Valid = RenewAnswer;

    fn validate(answer: RenewAnswer) -> Result<RenewAnswer, ContractError> {
        check(
            valid_rfc3339(&answer.lease_until),
            ErrorCode::Grammar,
            "/lease_until",
        )?;
        Ok(answer)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    fn fails<T: Shape>(json: &str) -> ContractError
    where
        T::Valid: fmt::Debug,
    {
        decode::<T>(json.as_bytes()).expect_err("the input must be refused")
    }

    fn assert_fails<T: Shape>(json: &str, code: ErrorCode, at: Option<&str>)
    where
        T::Valid: fmt::Debug,
    {
        let err = fails::<T>(json);
        assert_eq!(err.code, code, "{json}");
        assert_eq!(err.at.as_deref(), at, "{json}");
    }

    /// Decodes `json`, re-encodes it and compares with `canonical` (T6).
    fn round_trips<T: Shape>(json: &str, canonical: Value)
    where
        T::Valid: Serialize + fmt::Debug,
    {
        let valid = decode::<T>(json.as_bytes()).expect("the input must decode");
        assert_eq!(serde_json::to_value(&valid).unwrap(), canonical, "{json}");
    }

    #[test]
    fn decode_refuses_malformed_json_unknown_fields_and_wrong_types() {
        for bad in [
            "",
            "{",
            r#"{"media_id":"m_1"} trailing"#,
            r#"{"media_id":"m_1","extra":1}"#,
            r#"{"media_id":17}"#,
            r#"{"Media_id":"m_1"}"#,
            r#"{}"#,
        ] {
            assert_fails::<MediaResponseShape>(bad, ErrorCode::Decode, None);
        }
        assert_fails::<HelloShape>(
            r#"{"kind":"telegram","version":"1.0.0","capabilities":{"max_text":4096,"stickers":true}}"#,
            ErrorCode::Decode,
            None,
        );
        assert_fails::<HelloShape>(
            r#"{"kind":"telegram","version":"1.0.0","capabilities":{"buttons":true}}"#,
            ErrorCode::Decode,
            None,
        );
        assert_fails::<HelloShape>(
            r#"{"kind":"telegram","version":"1.0.0","capabilities":{"max_text":4096,"media_in":["video"]}}"#,
            ErrorCode::Decode,
            None,
        );
        assert_fails::<AckShape>(
            r#"{"failed":{"reason":"network"}}"#,
            ErrorCode::Decode,
            None,
        );
        assert_fails::<AckResponseShape>(r#"{"state":"lost"}"#, ErrorCode::Decode, None);
        assert_fails::<ErrorResponseShape>(
            r#"{"detail":"something failed"}"#,
            ErrorCode::Decode,
            None,
        );
    }

    #[test]
    fn decode_refuses_duplicate_keys_at_any_depth() {
        assert_fails::<MediaResponseShape>(
            r#"{"media_id":"m_1","media_id":"m_2"}"#,
            ErrorCode::Decode,
            None,
        );
        assert_fails::<HelloShape>(
            r#"{"kind":"telegram","version":"1.0.0","capabilities":{"max_text":1,"max_text":2}}"#,
            ErrorCode::Decode,
            None,
        );
        assert_fails::<AckShape>(
            r#"{"failed":{"retryable":true,"reason":"a","retryable":false}}"#,
            ErrorCode::Decode,
            None,
        );
    }

    #[test]
    fn a_grammar_violation_is_grammar_with_its_pointer() {
        assert_fails::<HelloShape>(
            r#"{"kind":"Telegram","version":"1.0.0","capabilities":{"max_text":4096}}"#,
            ErrorCode::Grammar,
            Some("/kind"),
        );
        assert_fails::<HelloShape>(
            r#"{"kind":"telegram","version":"1.0","capabilities":{"max_text":4096}}"#,
            ErrorCode::Grammar,
            Some("/version"),
        );
        assert_fails::<HelloResponseShape>(
            r#"{"contract_version":"v1"}"#,
            ErrorCode::Grammar,
            Some("/contract_version"),
        );
        assert_fails::<ErrorResponseShape>(
            r#"{"error":"Bad Request"}"#,
            ErrorCode::Grammar,
            Some("/error"),
        );
        assert_fails::<ErrorResponseShape>(
            r#"{"error":"grammar","at":"conversation.ref"}"#,
            ErrorCode::Grammar,
            Some("/at"),
        );
        assert_fails::<MediaResponseShape>(
            r#"{"media_id":"17"}"#,
            ErrorCode::Grammar,
            Some("/media_id"),
        );
        assert_fails::<AckShape>(
            r#"{"failed":{"retryable":true,"reason":"Rate limited"}}"#,
            ErrorCode::Grammar,
            Some("/failed/reason"),
        );
        assert_fails::<RenewResponseShape>(
            r#"{"lease_until":"in 60s"}"#,
            ErrorCode::Grammar,
            Some("/lease_until"),
        );
    }

    #[test]
    fn the_media_id_grammar_is_m_underscore_and_one_to_thirty_lowercase_alphanumerics() {
        assert!(valid_media_id("m_17"));
        assert!(valid_media_id(&format!("m_{}", "a".repeat(30))));
        for bad in ["m_", "m_A", "x_1", "m-1", "17"] {
            assert!(!valid_media_id(bad), "{bad}");
        }
        assert!(!valid_media_id(&format!("m_{}", "a".repeat(31))));
    }

    #[test]
    fn a_number_outside_its_bounds_is_range() {
        assert_fails::<HelloShape>(
            r#"{"kind":"telegram","version":"1.0.0","capabilities":{"max_text":0}}"#,
            ErrorCode::Range,
            Some("/capabilities/max_text"),
        );
        assert_fails::<HelloShape>(
            r#"{"kind":"telegram","version":"1.0.0","capabilities":{"max_text":1000001}}"#,
            ErrorCode::Range,
            Some("/capabilities/max_text"),
        );
        assert_fails::<AckShape>(
            r#"{"failed":{"retryable":true,"retry_after_ms":86400001,"reason":"rate_limited"}}"#,
            ErrorCode::Range,
            Some("/failed/retry_after_ms"),
        );
        assert_fails::<AckShape>(
            &format!(r#"{{"ok":true,"platform_refs":["{}"]}}"#, "a".repeat(257)),
            ErrorCode::Range,
            Some("/platform_refs/0"),
        );
        // The ceiling itself is accepted.
        decode::<AckShape>(
            br#"{"failed":{"retryable":true,"retry_after_ms":86400000,"reason":"rate_limited"}}"#,
        )
        .unwrap();
        // A negative delay is structural, not a range.
        assert_fails::<AckShape>(
            r#"{"failed":{"retryable":true,"retry_after_ms":-1,"reason":"network"}}"#,
            ErrorCode::Decode,
            None,
        );
    }

    #[test]
    fn an_empty_ref_is_empty() {
        assert_fails::<AckShape>(
            r#"{"ok":true,"platform_refs":[""]}"#,
            ErrorCode::Empty,
            Some("/platform_refs/0"),
        );
    }

    #[test]
    fn a_field_required_by_another_is_required_if() {
        // No D1 shape carries a `required_if` rule (it belongs to events, D2): the code and its
        // wire form are what this chunk owns.
        let err = ContractError::new(ErrorCode::RequiredIf, "/conversation/parent");
        assert_eq!(err.code.wire(), "required_if");
        assert_eq!(
            serde_json::to_value(ErrorResponse::from(err)).unwrap(),
            json!({"error": "required_if", "at": "/conversation/parent"})
        );
    }

    #[test]
    fn a_field_another_one_forbids_is_forbidden() {
        assert_fails::<AckShape>(
            r#"{"failed":{"retryable":false,"retry_after_ms":1000,"reason":"thread_not_found"}}"#,
            ErrorCode::Forbidden,
            Some("/failed/retry_after_ms"),
        );
        assert_fails::<AckResponseShape>(
            r#"{"state":"pending","ignored":true}"#,
            ErrorCode::Forbidden,
            Some("/ignored"),
        );
    }

    #[test]
    fn exactly_one_of_ok_and_failed_is_one_of_at_the_document() {
        for bad in [
            r#"{"ok":true,"failed":{"retryable":true,"reason":"network"}}"#,
            r#"{}"#,
        ] {
            assert_fails::<AckShape>(bad, ErrorCode::OneOf, Some(""));
        }
    }

    #[test]
    fn a_repeated_list_value_is_unique_at_the_second_index() {
        assert_fails::<HelloShape>(
            r#"{"kind":"telegram","version":"1.0.0","capabilities":{"max_text":4096,"media_out":["image","image"]}}"#,
            ErrorCode::Unique,
            Some("/capabilities/media_out/1"),
        );
        assert_fails::<HelloShape>(
            r#"{"kind":"telegram","version":"1.0.0","capabilities":{"max_text":4096,"media_in":["audio","file","audio"]}}"#,
            ErrorCode::Unique,
            Some("/capabilities/media_in/2"),
        );
    }

    #[test]
    fn a_forbidden_literal_is_value() {
        assert_fails::<AckShape>(r#"{"ok":false}"#, ErrorCode::Value, Some("/ok"));
    }

    #[test]
    fn valid_hellos_re_encode_in_canonical_form() {
        round_trips::<HelloShape>(
            r#"{"kind":"telegram","version":"1.0.0","capabilities":{"buttons":true,"edit":true,
            "typing":true,"threads":true,"reply_to":true,"max_text":4096,
            "media_in":["audio","image","file"],"media_out":["image","file","audio"]}}"#,
            json!({"kind": "telegram", "version": "1.0.0", "capabilities": {
                "buttons": true, "edit": true, "typing": true, "threads": true,
                "reply_to": true, "max_text": 4096,
                "media_in": ["audio", "image", "file"],
                "media_out": ["image", "file", "audio"]}}),
        );
        let minimal =
            json!({"kind": "signal", "version": "1.0.0", "capabilities": {"max_text": 2000}});
        round_trips::<HelloShape>(
            r#"{"kind":"signal","version":"1.0.0","capabilities":{"max_text":2000}}"#,
            minimal.clone(),
        );
        // Spelled out in full, it encodes back to the minimal form.
        round_trips::<HelloShape>(
            r#"{"kind":"signal","version":"1.0.0","capabilities":{"buttons":false,"edit":false,
            "typing":false,"threads":false,"reply_to":false,"max_text":2000,
            "media_in":[],"media_out":[]}}"#,
            minimal,
        );
        // A future major is a valid shape; the incompatibility is the 409, not a decode error.
        decode::<HelloShape>(
            br#"{"kind":"telegram","version":"2.0.0","capabilities":{"max_text":4096}}"#,
        )
        .unwrap();
    }

    #[test]
    fn valid_acks_and_answers_re_encode_in_canonical_form() {
        round_trips::<AckShape>(
            r#"{"ok":true,"platform_refs":["993","994"]}"#,
            json!({"ok": true, "platform_refs": ["993", "994"]}),
        );
        round_trips::<AckShape>(r#"{"ok":true}"#, json!({"ok": true}));
        round_trips::<AckShape>(r#"{"ok":true,"platform_refs":[]}"#, json!({"ok": true}));
        round_trips::<AckShape>(
            r#"{"failed":{"retryable":true,"retry_after_ms":45000,"reason":"rate_limited"}}"#,
            json!({"failed": {"retryable": true, "retry_after_ms": 45000, "reason": "rate_limited"}}),
        );
        round_trips::<AckShape>(
            r#"{"failed":{"retryable":true,"reason":"network"}}"#,
            json!({"failed": {"retryable": true, "reason": "network"}}),
        );
        round_trips::<AckShape>(
            r#"{"failed":{"retryable":false,"reason":"unsupported_capability"}}"#,
            json!({"failed": {"retryable": false, "reason": "unsupported_capability"}}),
        );
        round_trips::<AckResponseShape>(r#"{"state":"done"}"#, json!({"state": "done"}));
        round_trips::<AckResponseShape>(r#"{"state":"pending"}"#, json!({"state": "pending"}));
        round_trips::<AckResponseShape>(
            r#"{"state":"failed","ignored":true}"#,
            json!({"state": "failed", "ignored": true}),
        );
        round_trips::<AckResponseShape>(
            r#"{"state":"done","ignored":true}"#,
            json!({"state": "done", "ignored": true}),
        );
        round_trips::<AckResponseShape>(
            r#"{"state":"failed","ignored":false}"#,
            json!({"state": "failed"}),
        );
        round_trips::<RenewResponseShape>(
            r#"{"lease_until":"2026-10-19T10:31:30Z"}"#,
            json!({"lease_until": "2026-10-19T10:31:30Z"}),
        );
        round_trips::<MediaResponseShape>(r#"{"media_id":"m_17"}"#, json!({"media_id": "m_17"}));
        round_trips::<HelloResponseShape>(
            r#"{"contract_version":"1.0.0"}"#,
            json!({"contract_version": "1.0.0"}),
        );
    }

    #[test]
    fn a_failed_ack_without_a_delay_has_no_retry_after() {
        let ack =
            decode::<AckShape>(br#"{"failed":{"retryable":true,"reason":"network"}}"#).unwrap();
        assert_eq!(ack.failed.unwrap().retry_after_ms, None);
    }

    #[test]
    fn error_responses_round_trip_and_keep_an_empty_pointer_apart_from_none() {
        round_trips::<ErrorResponseShape>(
            r#"{"error":"required_if","at":"/conversation/parent","detail":"a thread conversation needs its parent"}"#,
            json!({"error": "required_if", "at": "/conversation/parent",
                "detail": "a thread conversation needs its parent"}),
        );
        round_trips::<ErrorResponseShape>(
            r#"{"error":"one_of","at":""}"#,
            json!({"error": "one_of", "at": ""}),
        );
        round_trips::<ErrorResponseShape>(
            r#"{"error":"not_leased"}"#,
            json!({"error": "not_leased"}),
        );
    }

    #[test]
    fn error_response_from_a_contract_error_carries_the_code_and_the_pointer() {
        let err = fails::<AckShape>(r#"{"ok":false}"#);
        assert_eq!(
            serde_json::to_value(ErrorResponse::from(err)).unwrap(),
            json!({"error": "value", "at": "/ok"})
        );
        let err = fails::<AckShape>("{}");
        assert_eq!(
            serde_json::to_value(ErrorResponse::from(err)).unwrap(),
            json!({"error": "one_of", "at": ""})
        );
        let err = fails::<AckShape>("{");
        assert_eq!(
            serde_json::to_value(ErrorResponse::from(err)).unwrap(),
            json!({"error": "decode"})
        );
    }

    #[test]
    fn the_contract_version_body_names_the_adapter_version() {
        let hello = decode::<HelloShape>(
            br#"{"kind":"telegram","version":"2.0.0","capabilities":{"max_text":4096}}"#,
        )
        .unwrap();
        assert_eq!(
            serde_json::to_value(ErrorResponse::contract_version(&hello)).unwrap(),
            json!({"error": "contract_version", "contract_version": "1.0.0",
                "detail": "adapter speaks 2.0.0"})
        );
    }

    #[test]
    fn every_error_code_has_its_snake_case_wire_name() {
        use ErrorCode::*;
        for (code, wire) in [
            (Decode, "decode"),
            (Grammar, "grammar"),
            (Range, "range"),
            (Empty, "empty"),
            (RequiredIf, "required_if"),
            (Forbidden, "forbidden"),
            (OneOf, "one_of"),
            (Unique, "unique"),
            (Value, "value"),
        ] {
            assert_eq!(code.wire(), wire);
            assert_eq!(serde_json::to_value(code).unwrap(), json!(wire));
        }
    }
}
