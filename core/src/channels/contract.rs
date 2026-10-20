//! Channel contract types (Anexo D, D.3): serde shapes and pure helpers, no I/O.
//!
//! `ref` is a reserved word in Rust, so the field is `reference` with `#[serde(rename = "ref")]`.
//! The strict decode layer (`decode`, `Shape`, `ErrorCode`, ...) lives in `contract/decode.rs`.

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

mod decode;
pub use decode::*;

#[cfg(test)]
mod conformance;

/// Where the shared fixtures live, relative to the `nucleos-core` package root.
#[cfg(test)]
pub(crate) const FIXTURES_REL: &str = "../sidecars/lib/channelkit/contract/fixtures";

/// The fixture directory of the checkout this test binary was compiled in.
#[cfg(test)]
pub(crate) fn fixtures_dir() -> std::path::PathBuf {
    let built_in = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let running_in = std::env::current_dir().expect("the working directory must be readable");
    // Same guard as governance/redact.rs: a shared target directory can hand this checkout a
    // binary compiled in another worktree, which would then read that worktree's fixtures and
    // pass while checking nothing. Cargo runs tests from the package root, so the two agree.
    assert_eq!(
        built_in,
        running_in.as_path(),
        "compiled in {} but running in {}; touch this file to force a rebuild",
        built_in.display(),
        running_in.display(),
    );
    built_in.join(FIXTURES_REL)
}

pub const CONTRACT_VERSION: &str = "1.0.0";

/// The largest `retry_after_ms` an adapter may report (24 h). Shared with the adapter kit.
pub const MAX_RETRY_AFTER_MS: u64 = 86_400_000;

/// `[a-z][a-z0-9-]{0,31}` (§4). Starts with a letter so it can never be read as a run id.
pub fn valid_channel_id(id: &str) -> bool {
    let b = id.as_bytes();
    if b.is_empty() || b.len() > 32 || !b[0].is_ascii_lowercase() {
        return false;
    }
    b[1..]
        .iter()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'-')
}

/// Parses a strict `d.d.d` version (digits only, three parts).
fn parse_version(v: &str) -> Option<(u64, u64, u64)> {
    let mut parts = v.split('.');
    let mut next = || {
        let p = parts.next()?;
        if p.is_empty() || !p.bytes().all(|c| c.is_ascii_digit()) {
            return None;
        }
        p.parse::<u64>().ok()
    };
    let major = next()?;
    let minor = next()?;
    let patch = next()?;
    if parts.next().is_some() {
        return None;
    }
    Some((major, minor, patch))
}

/// Compatible = same MAJOR and the adapter's MINOR <= the núcleo's MINOR (Anexo C, T1; fixture
/// `compat-newer-minor` expects `false`). Anything else is the 409 `contract_version` of §5.1.
/// PATCH is ignored; a version that is not `d.d.d` is incompatible.
pub fn compatible(adapter_contract: &str) -> bool {
    let (Some((a_major, a_minor, _)), Some((c_major, c_minor, _))) = (
        parse_version(adapter_contract),
        parse_version(CONTRACT_VERSION),
    ) else {
        return false;
    };
    a_major == c_major && a_minor <= c_minor
}

/// A `sender_ref` / `conversation_ref` / `message_ref` the núcleo accepts: not empty and at most
/// 128 bytes. Anything else is `malformed_ref` (Anexo E, E.1). This is the limit of the
/// MANAGEMENT routes; the refs inside an inbound EVENT (`event_id`, `ref`, `parent`, `message_ref`,
/// `action_ref`) are 1-256 bytes (Anexo C, T7) and are checked by `decode`/`Shape`, not here.
pub fn valid_ref(reference: &str) -> bool {
    !reference.is_empty() && reference.len() <= 128
}

/// The named refusals of the management routes (Anexo E, E.1); the wire name is the
/// `{"refusal": "<name>", "detail": "<sentence>"}` the shell switches on. `detail` is English prose.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ManagementRefusal {
    MalformedChannel,     // 400
    UnknownChannel,       // 404
    MalformedRef,         // 400
    ChannelNotConfigured, // 409
    NoHomeConversation,   // 409
    UnknownIdentity,      // 404
    UnknownConversation,  // 404
}

impl ManagementRefusal {
    pub fn status(self) -> StatusCode {
        match self {
            Self::MalformedChannel | Self::MalformedRef => StatusCode::BAD_REQUEST,
            Self::UnknownChannel | Self::UnknownIdentity | Self::UnknownConversation => {
                StatusCode::NOT_FOUND
            }
            Self::ChannelNotConfigured | Self::NoHomeConversation => StatusCode::CONFLICT,
        }
    }

    pub fn wire(self) -> &'static str {
        match self {
            Self::MalformedChannel => "malformed_channel",
            Self::UnknownChannel => "unknown_channel",
            Self::MalformedRef => "malformed_ref",
            Self::ChannelNotConfigured => "channel_not_configured",
            Self::NoHomeConversation => "no_home_conversation",
            Self::UnknownIdentity => "unknown_identity",
            Self::UnknownConversation => "unknown_conversation",
        }
    }
}

/// A conversation as the núcleo stores it: `"<channel>:<ref>"` (§4). The channel is the
/// manifest's `&'static str`, so the key is cheap to copy around and can only name a compiled kind.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ConversationKey {
    pub channel: &'static str,
    pub reference: String,
}

impl ConversationKey {
    pub fn new(channel: &'static str, reference: impl Into<String>) -> Self {
        Self {
            channel,
            reference: reference.into(),
        }
    }

    /// Splits on the FIRST `:` (a channel id never holds one; a Telegram thread ref does) and
    /// resolves the channel through `registry::by_id`. `None` for a shell UUID or an old
    /// unprefixed Telegram id.
    pub fn parse(chat_id: &str) -> Option<Self> {
        let (channel, reference) = chat_id.split_once(':')?;
        if reference.is_empty() {
            return None;
        }
        let manifest = crate::channels::registry::by_id(channel)?;
        Some(Self::new(manifest.id, reference))
    }

    /// The `runs.chat_id` / `chat_queue.chat_id` spelling.
    pub fn chat_id(&self) -> String {
        format!("{}:{}", self.channel, self.reference)
    }
}

// ---- §5.1 hello ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
pub struct Hello {
    pub kind: String,
    /// The adapter's contract version (not its own build version).
    pub version: String,
    pub capabilities: Capabilities,
}

// No derived `Default`: a derive would give `max_text` 0. The fallback is built by hand with 4096
// (D.4.3) and `registry::capabilities` uses it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Capabilities {
    #[serde(default)]
    pub buttons: bool,
    #[serde(default)]
    pub edit: bool,
    #[serde(default)]
    pub typing: bool,
    #[serde(default)]
    pub threads: bool,
    #[serde(default)]
    pub reply_to: bool,
    pub max_text: u32,
    #[serde(default)]
    pub media_in: Vec<MediaKind>,
    #[serde(default)]
    pub media_out: Vec<MediaKind>,
}

impl Default for Capabilities {
    fn default() -> Self {
        Self {
            buttons: false,
            edit: false,
            typing: false,
            threads: false,
            reply_to: false,
            max_text: 4096,
            media_in: Vec::new(),
            media_out: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MediaKind {
    Audio,
    Image,
    File,
}

#[derive(Debug, Serialize)]
pub struct HelloReply {
    pub contract_version: &'static str,
}

// ---- §5.3 events --------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
pub struct InboundEvent {
    pub event_id: String,
    #[serde(default)]
    pub at: Option<DateTime<Utc>>,
    pub conversation: Conversation,
    pub sender: Sender,
    #[serde(flatten)]
    pub body: EventBody,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum EventBody {
    Message { message: InMessage },
    Action { action: InAction },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Conversation {
    #[serde(rename = "ref")]
    pub reference: String,
    pub kind: ConversationKind,
    #[serde(default)]
    pub parent: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConversationKind {
    Private,
    Group,
    Thread,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Sender {
    #[serde(rename = "ref")]
    pub reference: String,
    #[serde(default)]
    pub name: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct InMessage {
    #[serde(rename = "ref")]
    pub reference: String,
    #[serde(default)]
    pub reply_to: Option<String>,
    pub parts: Vec<InPart>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum InPart {
    Text {
        text: String,
    },
    Audio {
        media: String,
        mime: String,
        #[serde(default)]
        duration_ms: Option<u64>,
    },
    Image {
        media: String,
        #[serde(default)]
        caption: Option<String>,
    },
    File {
        media: String,
        #[serde(default)]
        name: Option<String>,
        #[serde(default)]
        caption: Option<String>,
    },
    Unsupported {
        label: String,
    },
}

#[derive(Debug, Clone, Deserialize)]
pub struct InAction {
    #[serde(rename = "ref")]
    pub reference: String,
    pub message_ref: String,
    pub token: String,
}

/// The three answers of §5.3. `IntoResponse` gives 202/200/202 with the JSON bodies of the spec.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IngestReply {
    Accepted,
    Duplicate,
    Dropped,
}

impl IntoResponse for IngestReply {
    fn into_response(self) -> Response {
        let (status, body) = match self {
            Self::Accepted => (StatusCode::ACCEPTED, serde_json::json!({"accepted": true})),
            Self::Duplicate => (StatusCode::OK, serde_json::json!({"duplicate": true})),
            Self::Dropped => (StatusCode::ACCEPTED, serde_json::json!({"dropped": true})),
        };
        (status, Json(body)).into_response()
    }
}

// ---- §5.2 media ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
pub struct MediaCreated {
    pub media_id: String,
}

// ---- §6.1 outbox --------------------------------------------------------------------------

#[derive(Debug, Default, Serialize)]
pub struct OutboxPoll {
    pub actions: Vec<OutAction>,
    pub ephemeral: Vec<Ephemeral>,
}

/// `id` is the `channel_outbox.id`; `reply_to` / `target` are núcleo ids, with the platform refs
/// joined in by the outbox so the adapter never keeps a map of its own (§6.1).
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum OutAction {
    Send {
        id: i64,
        conversation: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        reply_to: Option<i64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        reply_to_platform_ref: Option<String>,
        parts: Vec<OutPart>,
        #[serde(skip_serializing_if = "Option::is_none")]
        buttons: Option<Vec<Vec<Button>>>,
        #[serde(default)]
        silent: bool,
    },
    Edit {
        id: i64,
        conversation: String,
        target: i64,
        target_platform_refs: Vec<String>,
        parts: Vec<OutPart>,
        #[serde(skip_serializing_if = "Option::is_none")]
        buttons: Option<Vec<Vec<Button>>>,
    },
    AnswerAction {
        id: i64,
        conversation: String,
        action_ref: String,
        text: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum OutPart {
    /// Neutral markdown (§6.4).
    Text {
        text: String,
    },
    Image {
        media: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        caption: Option<String>,
    },
    File {
        media: String,
        name: String,
    },
    Audio {
        media: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Button {
    pub label: String,
    pub token: String,
}

// The decode layer (`decode`, `Shape`, `ErrorCode`, `ContractError`, `ErrorResponse`)
// uses its own raw serde types and arrives with D1-D3/FX; nothing above changes.

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Ephemeral {
    Typing { conversation: String },
}

// ---- §6.2 ack -----------------------------------------------------------------------------

/// An `ok` ack without `platform_refs` settles `done` with an empty list.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum Ack {
    Ok {
        ok: bool,
        #[serde(default)]
        platform_refs: Vec<String>,
    },
    Failed {
        failed: Failure,
    },
}

#[derive(Debug, Clone, Deserialize)]
pub struct Failure {
    pub retryable: bool,
    #[serde(default)]
    pub retry_after_ms: Option<u64>,
    pub reason: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn valid_channel_id_follows_the_grammar() {
        assert!(valid_channel_id("telegram"));
        assert!(valid_channel_id("a"));
        assert!(valid_channel_id(&"a".repeat(32)));
        assert!(valid_channel_id("a-b1"));
        for bad in ["1abc", "Telegram", "a.b", "", "-a"] {
            assert!(!valid_channel_id(bad), "{bad}");
        }
        assert!(!valid_channel_id(&"a".repeat(33)));
    }

    #[test]
    fn compatible_follows_major_and_minor() {
        assert!(compatible("1.0.0"));
        assert!(compatible("1.0.9"));
        for bad in [
            "1.1.0", "2.0.0", "0.9.0", "v1", "1.0", "1.0.0.0", "1.x.0", "", "1..0",
        ] {
            assert!(!compatible(bad), "{bad}");
        }
    }

    #[test]
    fn valid_ref_bounds_are_one_to_128_bytes() {
        assert!(!valid_ref(""));
        assert!(valid_ref("x"));
        assert!(valid_ref(&"a".repeat(128)));
        assert!(!valid_ref(&"a".repeat(129)));
    }

    #[test]
    fn management_refusals_carry_status_and_wire_name() {
        use ManagementRefusal::*;
        let table = [
            (MalformedChannel, 400, "malformed_channel"),
            (UnknownChannel, 404, "unknown_channel"),
            (MalformedRef, 400, "malformed_ref"),
            (ChannelNotConfigured, 409, "channel_not_configured"),
            (NoHomeConversation, 409, "no_home_conversation"),
            (UnknownIdentity, 404, "unknown_identity"),
            (UnknownConversation, 404, "unknown_conversation"),
        ];
        for (refusal, status, wire) in table {
            assert_eq!(refusal.status().as_u16(), status, "{wire}");
            assert_eq!(refusal.wire(), wire);
            assert_eq!(serde_json::to_value(refusal).unwrap(), json!(wire));
        }
    }

    #[test]
    fn conversation_key_parse_splits_on_the_first_colon() {
        let key = ConversationKey::parse("telegram:-100123:5").unwrap();
        assert_eq!(key.channel, "telegram");
        assert_eq!(key.reference, "-100123:5");
    }

    #[test]
    fn conversation_key_parse_rejects_non_channel_ids() {
        for bad in [
            "c2f1b7e0-5a3d-4a8e-9a53-0e6b3f6a1d11",
            "-100123",
            "nope:1",
            "telegram:",
            "",
        ] {
            assert!(ConversationKey::parse(bad).is_none(), "{bad}");
        }
    }

    #[test]
    fn conversation_key_chat_id_round_trips() {
        let key = ConversationKey::new("telegram", "-100123:5");
        assert_eq!(key.chat_id(), "telegram:-100123:5");
        assert_eq!(ConversationKey::parse(&key.chat_id()), Some(key));
    }

    #[test]
    fn a_message_event_with_five_parts_deserializes() {
        let ev: InboundEvent = serde_json::from_value(json!({
            "event_id": "upd-88123",
            "at": "2026-10-19T10:30:00Z",
            "conversation": {"ref": "-100123:5", "kind": "thread", "parent": "-100123"},
            "sender": {"ref": "4242", "name": "Duarte"},
            "type": "message",
            "message": {"ref": "991", "reply_to": "987", "parts": [
                {"type": "text", "text": "hi"},
                {"type": "audio", "media": "m_17", "mime": "audio/ogg", "duration_ms": 4100},
                {"type": "image", "media": "m_18", "caption": "c"},
                {"type": "file", "media": "m_19", "name": "x.pdf", "caption": "c"},
                {"type": "unsupported", "label": "sticker"}
            ]}
        }))
        .unwrap();
        assert_eq!(ev.event_id, "upd-88123");
        assert!(ev.at.is_some());
        assert_eq!(ev.conversation.kind, ConversationKind::Thread);
        assert_eq!(ev.conversation.parent.as_deref(), Some("-100123"));
        assert_eq!(ev.sender.name.as_deref(), Some("Duarte"));
        let EventBody::Message { message } = ev.body else {
            panic!("not a message")
        };
        assert_eq!(message.reply_to.as_deref(), Some("987"));
        assert_eq!(message.parts.len(), 5);
        assert!(matches!(
            message.parts[1],
            InPart::Audio {
                duration_ms: Some(4100),
                ..
            }
        ));
        assert!(matches!(&message.parts[4], InPart::Unsupported { label } if label == "sticker"));
    }

    #[test]
    fn an_action_event_without_at_or_name_deserializes() {
        let ev: InboundEvent = serde_json::from_value(json!({
            "event_id": "cbq-551",
            "conversation": {"ref": "1", "kind": "private"},
            "sender": {"ref": "4242"},
            "type": "action",
            "action": {"ref": "551", "message_ref": "990", "token": "tk_9f2c"}
        }))
        .unwrap();
        assert!(ev.at.is_none());
        assert!(ev.sender.name.is_none());
        assert!(ev.conversation.parent.is_none());
        let EventBody::Action { action } = ev.body else {
            panic!("not an action")
        };
        assert_eq!(action.token, "tk_9f2c");
        assert_eq!(action.message_ref, "990");
    }

    #[test]
    fn capabilities_fill_defaults_and_the_manual_default_is_4096() {
        let caps: Capabilities = serde_json::from_value(json!({"max_text": 2000})).unwrap();
        assert_eq!(caps.max_text, 2000);
        assert!(!caps.buttons && !caps.edit && !caps.typing && !caps.threads && !caps.reply_to);
        assert!(caps.media_in.is_empty() && caps.media_out.is_empty());
        assert_eq!(Capabilities::default().max_text, 4096);
    }

    #[test]
    fn the_retry_ceiling_is_24_hours() {
        assert_eq!(MAX_RETRY_AFTER_MS, 86_400_000);
    }

    #[test]
    fn a_send_action_omits_absent_optionals() {
        let action = OutAction::Send {
            id: 7,
            conversation: "-100123".into(),
            reply_to: None,
            reply_to_platform_ref: None,
            parts: vec![OutPart::Text { text: "hi".into() }],
            buttons: None,
            silent: false,
        };
        let v = serde_json::to_value(&action).unwrap();
        let obj = v.as_object().unwrap();
        assert_eq!(obj["type"], "send");
        assert_eq!(obj["id"], 7);
        assert!(!obj.contains_key("reply_to"));
        assert!(!obj.contains_key("reply_to_platform_ref"));
        assert!(!obj.contains_key("buttons"));
        assert_eq!(v["parts"][0], json!({"type": "text", "text": "hi"}));
    }

    #[test]
    fn an_ack_accepts_ok_with_and_without_platform_refs_and_failed() {
        let bare: Ack = serde_json::from_value(json!({"ok": true})).unwrap();
        assert!(
            matches!(bare, Ack::Ok { ok: true, ref platform_refs } if platform_refs.is_empty())
        );
        let refs: Ack =
            serde_json::from_value(json!({"ok": true, "platform_refs": ["9"]})).unwrap();
        assert!(matches!(refs, Ack::Ok { ref platform_refs, .. } if platform_refs == &["9"]));
        let failed: Ack = serde_json::from_value(json!(
            {"failed": {"retryable": true, "retry_after_ms": 1500, "reason": "rate_limited"}}
        ))
        .unwrap();
        let Ack::Failed { failed } = failed else {
            panic!("not failed")
        };
        assert!(failed.retryable);
        assert_eq!(failed.retry_after_ms, Some(1500));
        assert_eq!(failed.reason, "rate_limited");
    }

    #[tokio::test]
    async fn ingest_replies_have_the_three_statuses_and_bodies() {
        let cases = [
            (IngestReply::Accepted, 202, json!({"accepted": true})),
            (IngestReply::Duplicate, 200, json!({"duplicate": true})),
            (IngestReply::Dropped, 202, json!({"dropped": true})),
        ];
        for (reply, status, body) in cases {
            let response = reply.into_response();
            assert_eq!(response.status().as_u16(), status);
            let bytes = axum::body::to_bytes(response.into_body(), 1024)
                .await
                .unwrap();
            assert_eq!(
                serde_json::from_slice::<serde_json::Value>(&bytes).unwrap(),
                body
            );
        }
    }
}
