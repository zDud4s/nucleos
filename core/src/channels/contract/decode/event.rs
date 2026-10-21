//! The `event` and `event_response` shapes (D2) and the valid event -> `InboundEvent` conversion.

use chrono::Utc;

use super::*;
use crate::channels::contract::{
    Conversation, ConversationKind, EventBody, InAction, InMessage, InPart, InboundEvent,
    IngestReply, Sender,
};

/// The raw event: the union of `message` and `action` is flattened into optional fields and
/// `validate` demands the right ones. `at` stays the string the adapter sent (T5), so the
/// encode is exact; [`EventWire::into_inbound`] parses it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EventWire {
    pub event_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<String>,
    pub conversation: ConversationWire,
    pub sender: SenderWire,
    #[serde(rename = "type")]
    pub event_type: EventType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<MessageWire>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<ActionWire>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventType {
    Message,
    Action,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConversationWire {
    #[serde(rename = "ref")]
    pub reference: String,
    pub kind: ConversationKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SenderWire {
    #[serde(rename = "ref")]
    pub reference: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MessageWire {
    #[serde(rename = "ref")]
    pub reference: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply_to: Option<String>,
    pub parts: Vec<PartWire>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PartKind {
    Text,
    Audio,
    Image,
    File,
    Unsupported,
}

/// A part with every field any kind may carry; `part_fields` says which kind takes which.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PartWire {
    #[serde(rename = "type")]
    pub kind: PartKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub media: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caption: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActionWire {
    #[serde(rename = "ref")]
    pub reference: String,
    pub message_ref: String,
    pub token: String,
}

/// Which fields each part kind may carry, and which of them it must.
fn part_fields(kind: PartKind) -> (&'static [&'static str], &'static [&'static str]) {
    match kind {
        PartKind::Text => (&["text"], &["text"]),
        PartKind::Audio => (&["media", "mime", "duration_ms"], &["media"]),
        PartKind::Image => (&["media", "mime", "caption"], &["media"]),
        PartKind::File => (&["media", "mime", "name", "caption"], &["media", "name"]),
        PartKind::Unsupported => (&["label"], &["label"]),
    }
}

fn validate_part(part: &PartWire, at: &str) -> Result<(), ContractError> {
    let present = [
        ("text", part.text.is_some()),
        ("media", part.media.is_some()),
        ("mime", part.mime.is_some()),
        ("duration_ms", part.duration_ms.is_some()),
        ("caption", part.caption.is_some()),
        ("name", part.name.is_some()),
        ("label", part.label.is_some()),
    ];
    let (allowed, required) = part_fields(part.kind);
    for &(name, here) in present.iter() {
        check(
            !here || allowed.contains(&name),
            ErrorCode::Forbidden,
            &format!("{at}/{name}"),
        )?;
    }
    for &name in required {
        let here = present.iter().any(|&(n, h)| n == name && h);
        check(here, ErrorCode::RequiredIf, &format!("{at}/{name}"))?;
    }
    if let Some(text) = &part.text {
        check(!text.is_empty(), ErrorCode::Empty, &format!("{at}/text"))?;
    }
    if let Some(media) = &part.media {
        check(
            valid_media_id(media),
            ErrorCode::Grammar,
            &format!("{at}/media"),
        )?;
    }
    if let Some(mime) = &part.mime {
        check(valid_mime(mime), ErrorCode::Grammar, &format!("{at}/mime"))?;
    }
    if let Some(name) = &part.name {
        check_ref(name, &format!("{at}/name"))?;
    }
    if let Some(label) = &part.label {
        check(
            snake_ident(label, 32),
            ErrorCode::Grammar,
            &format!("{at}/label"),
        )?;
    }
    Ok(())
}

fn validate_message(message: &MessageWire) -> Result<(), ContractError> {
    check_ref(&message.reference, "/message/ref")?;
    if let Some(reply_to) = &message.reply_to {
        check_ref(reply_to, "/message/reply_to")?;
    }
    check(
        !message.parts.is_empty(),
        ErrorCode::Empty,
        "/message/parts",
    )?;
    for (i, part) in message.parts.iter().enumerate() {
        validate_part(part, &format!("/message/parts/{i}"))?;
    }
    Ok(())
}

/// Shape `event`.
pub struct EventShape;

impl Shape for EventShape {
    type Wire = EventWire;
    type Valid = EventWire;

    fn validate(event: EventWire) -> Result<EventWire, ContractError> {
        check_ref(&event.event_id, "/event_id")?;
        if let Some(at) = &event.at {
            check(valid_rfc3339(at), ErrorCode::Grammar, "/at")?;
        }
        let conversation = &event.conversation;
        check_ref(&conversation.reference, "/conversation/ref")?;
        match (conversation.kind, &conversation.parent) {
            (ConversationKind::Thread, None) => {
                return Err(ContractError::new(
                    ErrorCode::RequiredIf,
                    "/conversation/parent",
                ));
            }
            (ConversationKind::Thread, Some(parent)) => {
                check_ref(parent, "/conversation/parent")?;
            }
            (_, Some(_)) => {
                return Err(ContractError::new(
                    ErrorCode::Forbidden,
                    "/conversation/parent",
                ));
            }
            (_, None) => {}
        }
        check_ref(&event.sender.reference, "/sender/ref")?;
        if let Some(name) = &event.sender.name {
            check_ref(name, "/sender/name")?;
        }
        match event.event_type {
            EventType::Message => {
                check(event.action.is_none(), ErrorCode::Forbidden, "/action")?;
                let message = event
                    .message
                    .as_ref()
                    .ok_or_else(|| ContractError::new(ErrorCode::RequiredIf, "/message"))?;
                validate_message(message)?;
            }
            EventType::Action => {
                check(event.message.is_none(), ErrorCode::Forbidden, "/message")?;
                let action = event
                    .action
                    .as_ref()
                    .ok_or_else(|| ContractError::new(ErrorCode::RequiredIf, "/action"))?;
                check_ref(&action.reference, "/action/ref")?;
                check_ref(&action.message_ref, "/action/message_ref")?;
                check(
                    valid_token(&action.token),
                    ErrorCode::Grammar,
                    "/action/token",
                )?;
            }
        }
        Ok(event)
    }
}

impl EventWire {
    /// The typed event `ingest` consumes (D.3). Meant for a value that came out of
    /// [`decode`]`::<EventShape>`: the fields `validate` guarantees are read without checking
    /// again, so a hand-built wire value yields empty strings, not an error. `at` becomes UTC
    /// here; the wire value keeps the adapter's spelling.
    pub fn into_inbound(self) -> InboundEvent {
        let at = self
            .at
            .as_deref()
            .and_then(|at| DateTime::parse_from_rfc3339(at).ok())
            .map(|at| at.with_timezone(&Utc));
        let body = match self.event_type {
            EventType::Message => {
                let message = self.message.unwrap_or(MessageWire {
                    reference: String::new(),
                    reply_to: None,
                    parts: Vec::new(),
                });
                EventBody::Message {
                    message: InMessage {
                        reference: message.reference,
                        reply_to: message.reply_to,
                        parts: message.parts.into_iter().map(PartWire::into_part).collect(),
                    },
                }
            }
            EventType::Action => {
                let action = self.action.unwrap_or(ActionWire {
                    reference: String::new(),
                    message_ref: String::new(),
                    token: String::new(),
                });
                EventBody::Action {
                    action: InAction {
                        reference: action.reference,
                        message_ref: action.message_ref,
                        token: action.token,
                    },
                }
            }
        };
        InboundEvent {
            event_id: self.event_id,
            at,
            conversation: Conversation {
                reference: self.conversation.reference,
                kind: self.conversation.kind,
                parent: self.conversation.parent,
            },
            sender: Sender {
                reference: self.sender.reference,
                name: self.sender.name,
            },
            body,
        }
    }
}

impl PartWire {
    fn into_part(self) -> InPart {
        let media = self.media.unwrap_or_default();
        match self.kind {
            PartKind::Text => InPart::Text {
                text: self.text.unwrap_or_default(),
            },
            // D.3 keeps `mime` required on an audio part although the wire makes it optional;
            // the stored media row is the authority on the type, so the fallback is neutral.
            PartKind::Audio => InPart::Audio {
                media,
                mime: self
                    .mime
                    .unwrap_or_else(|| "application/octet-stream".to_string()),
                duration_ms: self.duration_ms,
            },
            PartKind::Image => InPart::Image {
                media,
                caption: self.caption,
            },
            PartKind::File => InPart::File {
                media,
                name: self.name,
                caption: self.caption,
            },
            PartKind::Unsupported => InPart::Unsupported {
                label: self.label.unwrap_or_default(),
            },
        }
    }
}

/// Decodes and validates a body, then converts it: what `ingest` calls.
pub fn decode_event(bytes: &[u8]) -> Result<InboundEvent, ContractError> {
    decode::<EventShape>(bytes).map(EventWire::into_inbound)
}

// ---- event_response -----------------------------------------------------------------------

/// The three answers of §5.3 as the adapter reads them: exactly one flag, and it is `true`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EventAnswer {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accepted: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duplicate: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dropped: Option<bool>,
}

impl EventAnswer {
    /// The reply the answer stands for. Meaningful on a decoded (validated) value.
    pub fn reply(&self) -> IngestReply {
        if self.duplicate.is_some() {
            IngestReply::Duplicate
        } else if self.dropped.is_some() {
            IngestReply::Dropped
        } else {
            IngestReply::Accepted
        }
    }
}

pub struct EventResponseShape;

impl Shape for EventResponseShape {
    type Wire = EventAnswer;
    type Valid = EventAnswer;

    fn validate(answer: EventAnswer) -> Result<EventAnswer, ContractError> {
        let flags = [
            ("/accepted", answer.accepted),
            ("/duplicate", answer.duplicate),
            ("/dropped", answer.dropped),
        ];
        let mut set = flags.iter().filter(|(_, flag)| flag.is_some());
        let (Some(&(at, flag)), None) = (set.next(), set.next()) else {
            return Err(ContractError::new(ErrorCode::OneOf, ""));
        };
        check(flag == Some(true), ErrorCode::Value, at)?;
        Ok(answer)
    }
}

/// D2: `event` and `event_response`. Every invalid fixture of Anexo C.7 is one test with its
/// JSON inline (the fixture harness re-runs them over the files).
#[cfg(test)]
mod tests {
    use super::*;
    use crate::channels::contract::decode::tests::{assert_fails, fails, round_trips};
    use crate::channels::contract::{ConversationKind, EventBody, InPart};
    use serde_json::{Value, json};

    const MESSAGE_FULL: &str = r#"{ "event_id": "upd-88123",
      "at": "2026-10-19T10:30:00Z",
      "conversation": {"ref": "-100123:5", "kind": "thread", "parent": "-100123"},
      "sender": {"ref": "4242", "name": "Duarte"},
      "type": "message",
      "message": { "ref": "991", "reply_to": "987",
        "parts": [ {"type": "text",  "text": "Vê isto e diz-me o que achas"},
                   {"type": "audio", "media": "m_17", "mime": "audio/ogg", "duration_ms": 4100},
                   {"type": "image", "media": "m_18", "caption": "o ecrã de erro"},
                   {"type": "file",  "media": "m_19", "name": "x.pdf", "caption": "o relatório"},
                   {"type": "unsupported", "label": "sticker"} ] } }"#;

    const GROUP_IMAGE: &str = r#"{ "event_id": "upd-88125",
      "at": "2026-10-19T10:31:00+01:00",
      "conversation": {"ref": "-100777", "kind": "group"},
      "sender": {"ref": "4242", "name": "Duarte"},
      "type": "message",
      "message": { "ref": "12", "parts": [ {"type": "image", "media": "m_20", "mime": "image/jpeg"} ] } }"#;

    const PRIVATE_TEXT: &str = r#"{ "event_id": "upd-88124",
      "conversation": {"ref": "4242", "kind": "private"},
      "sender": {"ref": "4242"},
      "type": "message",
      "message": { "ref": "992", "parts": [ {"type": "text", "text": "/kill on"} ] } }"#;

    const PAIR: &str = r#"{ "event_id": "upd-88126",
      "conversation": {"ref": "4242", "kind": "private"},
      "sender": {"ref": "4242", "name": "Duarte"},
      "type": "message",
      "message": { "ref": "993", "parts": [ {"type": "text", "text": "/pair 483920"} ] } }"#;

    const ACTION: &str = r#"{ "event_id": "cbq-551",
      "at": "2026-10-19T10:32:00Z",
      "conversation": {"ref": "-100123:5", "kind": "thread", "parent": "-100123"},
      "sender": {"ref": "4242", "name": "Duarte"},
      "type": "action",
      "action": { "ref": "551", "message_ref": "990", "token": "tk_9f2c" } }"#;

    fn event_fails(json: &str, code: ErrorCode, at: Option<&str>) {
        assert_fails::<EventShape>(json, code, at);
    }

    #[test]
    fn valid_events_re_encode_to_themselves() {
        for json in [MESSAGE_FULL, GROUP_IMAGE, PRIVATE_TEXT, PAIR, ACTION] {
            let original: Value = serde_json::from_str(json).unwrap();
            round_trips::<EventShape>(json, original);
        }
    }

    #[test]
    fn at_is_kept_verbatim() {
        let valid = decode::<EventShape>(GROUP_IMAGE.as_bytes()).unwrap();
        assert_eq!(valid.at.as_deref(), Some("2026-10-19T10:31:00+01:00"));
        let encoded = serde_json::to_value(&valid).unwrap();
        assert_eq!(encoded["at"], json!("2026-10-19T10:31:00+01:00"));
        // The typed event carries the instant, in UTC.
        let event = valid.into_inbound();
        assert_eq!(event.at.unwrap().to_rfc3339(), "2026-10-19T09:31:00+00:00");
        // Absent stays absent (T5): no invented time on the wire value.
        let private = decode::<EventShape>(PRIVATE_TEXT.as_bytes()).unwrap();
        assert_eq!(private.at, None);
        assert!(private.into_inbound().at.is_none());
    }

    #[test]
    fn thread_event_round_trips_to_inbound_event() {
        let event = decode_event(MESSAGE_FULL.as_bytes()).unwrap();
        assert_eq!(event.event_id, "upd-88123");
        assert_eq!(event.at.unwrap().to_rfc3339(), "2026-10-19T10:30:00+00:00");
        assert_eq!(event.conversation.reference, "-100123:5");
        assert_eq!(event.conversation.kind, ConversationKind::Thread);
        assert_eq!(event.conversation.parent.as_deref(), Some("-100123"));
        assert_eq!(event.sender.reference, "4242");
        assert_eq!(event.sender.name.as_deref(), Some("Duarte"));
        let EventBody::Message { message } = event.body else {
            panic!("a message event must convert to a message body");
        };
        assert_eq!(message.reference, "991");
        assert_eq!(message.reply_to.as_deref(), Some("987"));
        assert_eq!(message.parts.len(), 5);
        assert!(matches!(&message.parts[0], InPart::Text { text } if text.starts_with("Vê")));
        assert!(matches!(
            &message.parts[1],
            InPart::Audio { media, mime, duration_ms: Some(4100) }
                if media == "m_17" && mime == "audio/ogg"
        ));
        assert!(matches!(
            &message.parts[2],
            InPart::Image { media, caption: Some(c) } if media == "m_18" && c == "o ecrã de erro"
        ));
        assert!(matches!(
            &message.parts[3],
            InPart::File { media, name: Some(n), caption: Some(_) }
                if media == "m_19" && n == "x.pdf"
        ));
        assert!(matches!(
            &message.parts[4],
            InPart::Unsupported { label } if label == "sticker"
        ));
    }

    #[test]
    fn an_action_event_converts_to_an_action_body() {
        let event = decode_event(ACTION.as_bytes()).unwrap();
        let EventBody::Action { action } = event.body else {
            panic!("an action event must convert to an action body");
        };
        assert_eq!(action.reference, "551");
        assert_eq!(action.message_ref, "990");
        assert_eq!(action.token, "tk_9f2c");
    }

    #[test]
    fn a_minimal_private_text_has_no_optional_fields() {
        let event = decode_event(PRIVATE_TEXT.as_bytes()).unwrap();
        assert!(event.at.is_none());
        assert!(event.sender.name.is_none());
        assert!(event.conversation.parent.is_none());
        let EventBody::Message { message } = event.body else {
            panic!("a message body");
        };
        assert!(message.reply_to.is_none());
    }

    #[test]
    fn an_audio_without_a_mime_is_accepted_and_given_a_neutral_one() {
        let event = decode_event(
            br#"{"event_id":"u","conversation":{"ref":"1","kind":"private"},"sender":{"ref":"1"},"type":"message","message":{"ref":"1","parts":[{"type":"audio","media":"m_1"}]}}"#,
        )
        .unwrap();
        let EventBody::Message { message } = event.body else {
            panic!("a message body");
        };
        assert!(matches!(
            &message.parts[0],
            InPart::Audio { mime, .. } if mime == "application/octet-stream"
        ));
    }

    #[test]
    fn the_mime_and_token_grammars_are_the_t7_ones() {
        for good in [
            "audio/ogg",
            "image/jpeg",
            "application/vnd.api+json",
            "a/b",
            "0/9",
        ] {
            assert!(valid_mime(good), "{good}");
        }
        for bad in [
            "",
            "audio",
            "audio/",
            "/ogg",
            "Audio/ogg",
            "audio/ogg; codecs=opus",
            "audio/ogg;codecs=opus",
            "audio /ogg",
            "a/b/c",
            "-a/b",
            "a/.b",
        ] {
            assert!(!valid_mime(bad), "{bad}");
        }
        assert!(valid_token("tk_9f2c"));
        assert!(valid_token(&format!("tk_{}", "a".repeat(61))));
        for bad in ["", "tk_", "tk_A", "9f2c", "tk-9f2c", "TK_9f2c"] {
            assert!(!valid_token(bad), "{bad}");
        }
        assert!(!valid_token(&format!("tk_{}", "a".repeat(62))));
    }

    #[test]
    fn a_free_ref_is_one_to_256_bytes() {
        let long = "a".repeat(257);
        let wrap = |event_id: &str| {
            format!(
                r#"{{"event_id":"{event_id}","conversation":{{"ref":"1","kind":"private"}},"sender":{{"ref":"1"}},"type":"message","message":{{"ref":"1","parts":[{{"type":"text","text":"x"}}]}}}}"#
            )
        };
        event_fails(&wrap(&long), ErrorCode::Range, Some("/event_id"));
        decode::<EventShape>(wrap(&"a".repeat(256)).as_bytes()).unwrap();
    }

    // ---- the invalid fixtures of C.7, one test each ------------------------------------

    #[test]
    fn thread_without_parent() {
        event_fails(
            r#"{"event_id":"upd-1","conversation":{"ref":"-100123:5","kind":"thread"},"sender":{"ref":"4242"},"type":"message","message":{"ref":"1","parts":[{"type":"text","text":"olá"}]}}"#,
            ErrorCode::RequiredIf,
            Some("/conversation/parent"),
        );
    }

    #[test]
    fn parent_on_private() {
        event_fails(
            r#"{"event_id":"upd-1","conversation":{"ref":"4242","kind":"private","parent":"4242"},"sender":{"ref":"4242"},"type":"message","message":{"ref":"1","parts":[{"type":"text","text":"olá"}]}}"#,
            ErrorCode::Forbidden,
            Some("/conversation/parent"),
        );
    }

    #[test]
    fn bad_conversation_kind() {
        event_fails(
            r#"{"event_id":"upd-1","conversation":{"ref":"4242","kind":"channel"},"sender":{"ref":"4242"},"type":"message","message":{"ref":"1","parts":[{"type":"text","text":"olá"}]}}"#,
            ErrorCode::Decode,
            None,
        );
    }

    #[test]
    fn empty_parts() {
        event_fails(
            r#"{"event_id":"upd-1","conversation":{"ref":"4242","kind":"private"},"sender":{"ref":"4242"},"type":"message","message":{"ref":"1","parts":[]}}"#,
            ErrorCode::Empty,
            Some("/message/parts"),
        );
    }

    #[test]
    fn empty_text() {
        event_fails(
            r#"{"event_id":"upd-1","conversation":{"ref":"4242","kind":"private"},"sender":{"ref":"4242"},"type":"message","message":{"ref":"1","parts":[{"type":"text","text":""}]}}"#,
            ErrorCode::Empty,
            Some("/message/parts/0/text"),
        );
    }

    #[test]
    fn text_with_media() {
        event_fails(
            r#"{"event_id":"upd-1","conversation":{"ref":"4242","kind":"private"},"sender":{"ref":"4242"},"type":"message","message":{"ref":"1","parts":[{"type":"text","text":"olá","media":"m_1"}]}}"#,
            ErrorCode::Forbidden,
            Some("/message/parts/0/media"),
        );
    }

    #[test]
    fn negative_duration() {
        event_fails(
            r#"{"event_id":"upd-1","conversation":{"ref":"4242","kind":"private"},"sender":{"ref":"4242"},"type":"message","message":{"ref":"1","parts":[{"type":"audio","media":"m_1","duration_ms":-1}]}}"#,
            ErrorCode::Decode,
            None,
        );
    }

    #[test]
    fn bad_media_id() {
        event_fails(
            r#"{"event_id":"upd-1","conversation":{"ref":"4242","kind":"private"},"sender":{"ref":"4242"},"type":"message","message":{"ref":"1","parts":[{"type":"audio","media":"17"}]}}"#,
            ErrorCode::Grammar,
            Some("/message/parts/0/media"),
        );
    }

    #[test]
    fn bad_mime() {
        event_fails(
            r#"{"event_id":"upd-1","conversation":{"ref":"4242","kind":"private"},"sender":{"ref":"4242"},"type":"message","message":{"ref":"1","parts":[{"type":"audio","media":"m_1","mime":"audio/ogg; codecs=opus"}]}}"#,
            ErrorCode::Grammar,
            Some("/message/parts/0/mime"),
        );
    }

    #[test]
    fn image_without_media() {
        event_fails(
            r#"{"event_id":"upd-1","conversation":{"ref":"4242","kind":"private"},"sender":{"ref":"4242"},"type":"message","message":{"ref":"1","parts":[{"type":"image","caption":"x"}]}}"#,
            ErrorCode::RequiredIf,
            Some("/message/parts/0/media"),
        );
    }

    #[test]
    fn file_without_name() {
        event_fails(
            r#"{"event_id":"upd-1","conversation":{"ref":"4242","kind":"private"},"sender":{"ref":"4242"},"type":"message","message":{"ref":"1","parts":[{"type":"file","media":"m_1"}]}}"#,
            ErrorCode::RequiredIf,
            Some("/message/parts/0/name"),
        );
    }

    #[test]
    fn bad_unsupported_label() {
        event_fails(
            r#"{"event_id":"upd-1","conversation":{"ref":"4242","kind":"private"},"sender":{"ref":"4242"},"type":"message","message":{"ref":"1","parts":[{"type":"unsupported","label":"Sticker!"}]}}"#,
            ErrorCode::Grammar,
            Some("/message/parts/0/label"),
        );
    }

    #[test]
    fn unknown_part_type() {
        event_fails(
            r#"{"event_id":"upd-1","conversation":{"ref":"4242","kind":"private"},"sender":{"ref":"4242"},"type":"message","message":{"ref":"1","parts":[{"type":"sticker","media":"m_1"}]}}"#,
            ErrorCode::Decode,
            None,
        );
    }

    #[test]
    fn message_type_without_message() {
        event_fails(
            r#"{"event_id":"upd-1","conversation":{"ref":"4242","kind":"private"},"sender":{"ref":"4242"},"type":"message"}"#,
            ErrorCode::RequiredIf,
            Some("/message"),
        );
    }

    #[test]
    fn action_with_message() {
        event_fails(
            r#"{"event_id":"cbq-1","conversation":{"ref":"4242","kind":"private"},"sender":{"ref":"4242"},"type":"action","action":{"ref":"551","message_ref":"990","token":"tk_9f2c"},"message":{"ref":"1","parts":[{"type":"text","text":"olá"}]}}"#,
            ErrorCode::Forbidden,
            Some("/message"),
        );
    }

    #[test]
    fn action_bad_token() {
        event_fails(
            r#"{"event_id":"cbq-1","conversation":{"ref":"4242","kind":"private"},"sender":{"ref":"4242"},"type":"action","action":{"ref":"551","message_ref":"990","token":"tk_0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcd"}}"#,
            ErrorCode::Grammar,
            Some("/action/token"),
        );
    }

    #[test]
    fn action_missing_message_ref() {
        event_fails(
            r#"{"event_id":"cbq-1","conversation":{"ref":"4242","kind":"private"},"sender":{"ref":"4242"},"type":"action","action":{"ref":"551","token":"tk_9f2c"}}"#,
            ErrorCode::Decode,
            None,
        );
    }

    #[test]
    fn action_type_without_action() {
        event_fails(
            r#"{"event_id":"cbq-1","conversation":{"ref":"4242","kind":"private"},"sender":{"ref":"4242"},"type":"action"}"#,
            ErrorCode::RequiredIf,
            Some("/action"),
        );
    }

    #[test]
    fn message_with_action() {
        event_fails(
            r#"{"event_id":"upd-1","conversation":{"ref":"4242","kind":"private"},"sender":{"ref":"4242"},"type":"message","message":{"ref":"1","parts":[{"type":"text","text":"olá"}]},"action":{"ref":"551","message_ref":"990","token":"tk_9f2c"}}"#,
            ErrorCode::Forbidden,
            Some("/action"),
        );
    }

    #[test]
    fn bad_at() {
        event_fails(
            r#"{"event_id":"upd-1","at":"ontem","conversation":{"ref":"4242","kind":"private"},"sender":{"ref":"4242"},"type":"message","message":{"ref":"1","parts":[{"type":"text","text":"olá"}]}}"#,
            ErrorCode::Grammar,
            Some("/at"),
        );
    }

    #[test]
    fn empty_event_id() {
        event_fails(
            r#"{"event_id":"","conversation":{"ref":"4242","kind":"private"},"sender":{"ref":"4242"},"type":"message","message":{"ref":"1","parts":[{"type":"text","text":"olá"}]}}"#,
            ErrorCode::Empty,
            Some("/event_id"),
        );
    }

    #[test]
    fn origin_in_body() {
        event_fails(
            r#"{"event_id":"upd-1","origin":"shell","conversation":{"ref":"4242","kind":"private"},"sender":{"ref":"4242"},"type":"message","message":{"ref":"1","parts":[{"type":"text","text":"olá"}]}}"#,
            ErrorCode::Decode,
            None,
        );
    }

    #[test]
    fn key_case() {
        event_fails(
            r#"{"event_id":"upd-1","conversation":{"ref":"4242","kind":"private"},"sender":{"ref":"4242"},"Type":"message","message":{"ref":"1","parts":[{"type":"text","text":"olá"}]}}"#,
            ErrorCode::Decode,
            None,
        );
    }

    #[test]
    fn duplicate_key() {
        event_fails(
            r#"{"event_id":"upd-1","event_id":"upd-2","conversation":{"ref":"4242","kind":"private"},"sender":{"ref":"4242"},"type":"message","message":{"ref":"1","parts":[{"type":"text","text":"olá"}]}}"#,
            ErrorCode::Decode,
            None,
        );
    }

    #[test]
    fn malformed() {
        event_fails(
            r#"{"event_id":"upd-1","conversation":{"ref":"4242""#,
            ErrorCode::Decode,
            None,
        );
    }

    #[test]
    fn the_400_body_of_a_refused_event_is_the_code_and_the_pointer() {
        let err = fails::<EventShape>(
            r#"{"event_id":"upd-1","conversation":{"ref":"-100123:5","kind":"thread"},"sender":{"ref":"4242"},"type":"message","message":{"ref":"1","parts":[{"type":"text","text":"olá"}]}}"#,
        );
        assert_eq!(
            serde_json::to_value(ErrorResponse::from(err)).unwrap(),
            json!({"error": "required_if", "at": "/conversation/parent"})
        );
    }

    // ---- event_response ------------------------------------------------------------------

    #[test]
    fn valid_event_responses_re_encode_and_map_to_the_reply() {
        for (json, reply) in [
            (r#"{"accepted":true}"#, IngestReply::Accepted),
            (r#"{"duplicate":true}"#, IngestReply::Duplicate),
            (r#"{"dropped":true}"#, IngestReply::Dropped),
        ] {
            round_trips::<EventResponseShape>(json, serde_json::from_str(json).unwrap());
            assert_eq!(
                decode::<EventResponseShape>(json.as_bytes())
                    .unwrap()
                    .reply(),
                reply
            );
        }
    }

    #[test]
    fn response_two_flags() {
        assert_fails::<EventResponseShape>(
            r#"{"accepted":true,"duplicate":true}"#,
            ErrorCode::OneOf,
            Some(""),
        );
    }

    #[test]
    fn response_false() {
        assert_fails::<EventResponseShape>(
            r#"{"accepted":false}"#,
            ErrorCode::Value,
            Some("/accepted"),
        );
    }

    #[test]
    fn response_empty() {
        assert_fails::<EventResponseShape>("{}", ErrorCode::OneOf, Some(""));
    }
}
