//! The `outbox_response` and `action` shapes (D3): what the núcleo sends an adapter.

use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutActionType {
    Send,
    Edit,
    AnswerAction,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutPartKind {
    Text,
    Image,
    File,
    Audio,
}

/// An outbound part with every field any kind may carry (there is no `unsupported` on the way
/// out). `mime` is optional on `audio` and `file`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OutPartWire {
    #[serde(rename = "type")]
    pub kind: OutPartKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub media: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caption: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ButtonWire {
    pub label: String,
    pub token: String,
}

/// The raw outbound action: `send`, `edit` and `answer_action` flattened into optional fields;
/// `validate_out_action` demands the right ones per type.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OutActionWire {
    pub id: i64,
    #[serde(rename = "type")]
    pub kind: OutActionType,
    pub conversation: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply_to: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply_to_platform_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_platform_refs: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parts: Option<Vec<OutPartWire>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub buttons: Option<Vec<Vec<ButtonWire>>>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub silent: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EphemeralType {
    Typing,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EphemeralWire {
    #[serde(rename = "type")]
    pub kind: EphemeralType,
    pub conversation: String,
}

/// Both lists are required, even when empty (an expired long-poll answers `{"actions":[],"ephemeral":[]}`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OutboxWire {
    pub actions: Vec<OutActionWire>,
    pub ephemeral: Vec<EphemeralWire>,
}

fn validate_out_part(part: &OutPartWire, at: &str) -> Result<(), ContractError> {
    let present = [
        ("text", part.text.is_some()),
        ("media", part.media.is_some()),
        ("mime", part.mime.is_some()),
        ("caption", part.caption.is_some()),
        ("name", part.name.is_some()),
    ];
    let (allowed, required): (&[&str], &[&str]) = match part.kind {
        OutPartKind::Text => (&["text"], &["text"]),
        OutPartKind::Image => (&["media", "caption"], &["media"]),
        OutPartKind::File => (&["media", "mime", "name"], &["media", "name"]),
        OutPartKind::Audio => (&["media", "mime"], &["media"]),
    };
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
    Ok(())
}

fn validate_out_parts(parts: &[OutPartWire], base: &str) -> Result<(), ContractError> {
    let at = format!("{base}/parts");
    check(!parts.is_empty(), ErrorCode::Empty, &at)?;
    for (i, part) in parts.iter().enumerate() {
        validate_out_part(part, &format!("{at}/{i}"))?;
    }
    Ok(())
}

fn validate_buttons(rows: &[Vec<ButtonWire>], base: &str) -> Result<(), ContractError> {
    for (r, row) in rows.iter().enumerate() {
        let row_at = format!("{base}/buttons/{r}");
        check(!row.is_empty(), ErrorCode::Empty, &row_at)?;
        for (c, button) in row.iter().enumerate() {
            let at = format!("{row_at}/{c}");
            check_ref(&button.label, &format!("{at}/label"))?;
            check(
                valid_token(&button.token),
                ErrorCode::Grammar,
                &format!("{at}/token"),
            )?;
        }
    }
    Ok(())
}

/// Validates one action; `base` is its pointer (`""` alone, `/actions/<i>` inside a poll).
fn validate_out_action(action: &OutActionWire, base: &str) -> Result<(), ContractError> {
    let at = |field: &str| format!("{base}/{field}");
    check(action.id >= 1, ErrorCode::Range, &at("id"))?;
    check_ref(&action.conversation, &at("conversation"))?;
    let present = [
        ("reply_to", action.reply_to.is_some()),
        (
            "reply_to_platform_ref",
            action.reply_to_platform_ref.is_some(),
        ),
        ("target", action.target.is_some()),
        (
            "target_platform_refs",
            action.target_platform_refs.is_some(),
        ),
        ("parts", action.parts.is_some()),
        ("buttons", action.buttons.is_some()),
        ("silent", action.silent),
        ("action_ref", action.action_ref.is_some()),
        ("text", action.text.is_some()),
    ];
    let (allowed, required): (&[&str], &[&str]) = match action.kind {
        OutActionType::Send => (
            &[
                "reply_to",
                "reply_to_platform_ref",
                "parts",
                "buttons",
                "silent",
            ],
            &["parts"],
        ),
        OutActionType::Edit => (
            &["target", "target_platform_refs", "parts", "buttons"],
            &["target", "target_platform_refs", "parts"],
        ),
        OutActionType::AnswerAction => (&["action_ref", "text"], &["action_ref", "text"]),
    };
    for &(name, here) in present.iter() {
        check(
            !here || allowed.contains(&name),
            ErrorCode::Forbidden,
            &at(name),
        )?;
    }
    for &name in required {
        let here = present.iter().any(|&(n, h)| n == name && h);
        check(here, ErrorCode::RequiredIf, &at(name))?;
    }
    // The platform ref of the replied-to message only exists with the id it translates.
    check(
        action.reply_to_platform_ref.is_none() || action.reply_to.is_some(),
        ErrorCode::Forbidden,
        &at("reply_to_platform_ref"),
    )?;
    if let Some(reply_to) = action.reply_to {
        check(reply_to >= 1, ErrorCode::Range, &at("reply_to"))?;
    }
    if let Some(reference) = &action.reply_to_platform_ref {
        check_ref(reference, &at("reply_to_platform_ref"))?;
    }
    if let Some(target) = action.target {
        check(target >= 1, ErrorCode::Range, &at("target"))?;
    }
    if let Some(refs) = &action.target_platform_refs {
        let refs_at = at("target_platform_refs");
        check(!refs.is_empty(), ErrorCode::Empty, &refs_at)?;
        for (i, reference) in refs.iter().enumerate() {
            check_ref(reference, &format!("{refs_at}/{i}"))?;
        }
    }
    if let Some(parts) = &action.parts {
        validate_out_parts(parts, base)?;
    }
    if let Some(rows) = &action.buttons {
        validate_buttons(rows, base)?;
    }
    if let Some(reference) = &action.action_ref {
        check_ref(reference, &at("action_ref"))?;
    }
    Ok(())
}

/// Shape `action`: one outbound action (`send`, `edit` or `answer_action`).
pub struct OutActionShape;

impl Shape for OutActionShape {
    type Wire = OutActionWire;
    type Valid = OutActionWire;

    fn validate(action: OutActionWire) -> Result<OutActionWire, ContractError> {
        validate_out_action(&action, "")?;
        Ok(action)
    }
}

/// Shape `outbox_response`: the long-poll answer. Every action is validated at `/actions/<i>`.
pub struct OutboxResponseShape;

impl Shape for OutboxResponseShape {
    type Wire = OutboxWire;
    type Valid = OutboxWire;

    fn validate(poll: OutboxWire) -> Result<OutboxWire, ContractError> {
        for (i, action) in poll.actions.iter().enumerate() {
            validate_out_action(action, &format!("/actions/{i}"))?;
        }
        for (i, typing) in poll.ephemeral.iter().enumerate() {
            check_ref(
                &typing.conversation,
                &format!("/ephemeral/{i}/conversation"),
            )?;
        }
        Ok(poll)
    }
}

/// D3: `outbox_response` and `action`. Every fixture of Anexo C.8 is one test with its JSON
/// inline (the fixture harness re-runs them over the files).
#[cfg(test)]
mod tests {
    use super::*;
    use crate::channels::contract::decode::tests::{assert_fails, round_trips};
    use crate::channels::contract::{Button, Ephemeral, OutAction, OutPart, OutboxPoll};
    use serde_json::Value;

    const SEND_FULL: &str = r#"{ "id": 57, "type": "send", "conversation": "-100123:5",
      "reply_to": 52, "reply_to_platform_ref": "980",
      "parts": [
        { "type": "text",
          "text": "**Proposal 12**: install `ripgrep`\n\n| field | value |\n|---|---|\n| cost | 0 |\n\n- see [the docs](https://example.org)\n\n    rg --version" },
        { "type": "image", "media": "m_31", "caption": "diff" },
        { "type": "file",  "media": "m_32", "name": "relatorio.pdf", "mime": "application/pdf" },
        { "type": "audio", "media": "m_33", "mime": "audio/ogg" } ],
      "buttons": [ [ {"label": "Approve", "token": "tk_9f2c"},
                     {"label": "Reject",  "token": "tk_9f2d"} ] ],
      "silent": true }"#;

    const SEND_TEXT: &str = r#"{ "id": 41, "type": "send", "conversation": "4242",
      "parts": [ {"type": "text", "text": "Done."} ] }"#;

    const EDIT: &str = r#"{ "id": 58, "type": "edit", "conversation": "-100123:5",
      "target": 57, "target_platform_refs": ["993", "994"],
      "parts": [ {"type": "text", "text": "**Proposal 12**: approved"} ] }"#;

    const ANSWER: &str = r#"{ "id": 59, "type": "answer_action", "conversation": "-100123:5",
      "action_ref": "551", "text": "Already handled." }"#;

    const MIXED: &str = r#"{ "actions": [
        { "id": 41, "type": "send", "conversation": "4242",
          "parts": [ {"type": "text", "text": "Working on it (turn 812)."} ] },
        { "id": 44, "type": "answer_action", "conversation": "-100123:5",
          "action_ref": "551", "text": "Approved." } ],
      "ephemeral": [ { "type": "typing", "conversation": "4242" } ] }"#;

    fn action_fails(json: &str, code: ErrorCode, at: Option<&str>) {
        assert_fails::<OutActionShape>(json, code, at);
    }

    #[test]
    fn valid_actions_re_encode_to_themselves() {
        for json in [SEND_FULL, SEND_TEXT, EDIT, ANSWER] {
            let original: Value = serde_json::from_str(json).unwrap();
            round_trips::<OutActionShape>(json, original);
        }
    }

    #[test]
    fn valid_responses_re_encode_to_themselves() {
        for json in [r#"{ "actions": [], "ephemeral": [] }"#, MIXED] {
            let original: Value = serde_json::from_str(json).unwrap();
            round_trips::<OutboxResponseShape>(json, original);
        }
    }

    #[test]
    fn send_full_keeps_its_buttons_and_well_formed_tokens() {
        let action = decode::<OutActionShape>(SEND_FULL.as_bytes()).unwrap();
        assert_eq!(action.kind, OutActionType::Send);
        assert!(action.silent);
        let buttons = action.buttons.unwrap();
        assert_eq!(buttons[0].len(), 2);
        assert!(buttons[0].iter().all(|b| valid_token(&b.token)));
    }

    #[test]
    fn send_text_has_no_silent() {
        let action = decode::<OutActionShape>(SEND_TEXT.as_bytes()).unwrap();
        assert!(!action.silent);
    }

    #[test]
    fn send_unknown_field() {
        action_fails(
            r#"{"id":41,"type":"send","conversation":"4242","priority":"high","parts":[{"type":"text","text":"Done."}]}"#,
            ErrorCode::Decode,
            None,
        );
    }

    #[test]
    fn send_empty_parts() {
        action_fails(
            r#"{"id":41,"type":"send","conversation":"4242","parts":[]}"#,
            ErrorCode::Empty,
            Some("/parts"),
        );
    }

    #[test]
    fn send_unsupported_part() {
        action_fails(
            r#"{"id":41,"type":"send","conversation":"4242","parts":[{"type":"unsupported","label":"sticker"}]}"#,
            ErrorCode::Decode,
            None,
        );
    }

    #[test]
    fn send_empty_text() {
        action_fails(
            r#"{"id":41,"type":"send","conversation":"4242","parts":[{"type":"text","text":""}]}"#,
            ErrorCode::Empty,
            Some("/parts/0/text"),
        );
    }

    #[test]
    fn send_image_with_text() {
        action_fails(
            r#"{"id":41,"type":"send","conversation":"4242","parts":[{"type":"image","media":"m_31","text":"x"}]}"#,
            ErrorCode::Forbidden,
            Some("/parts/0/text"),
        );
    }

    #[test]
    fn send_file_without_name() {
        action_fails(
            r#"{"id":41,"type":"send","conversation":"4242","parts":[{"type":"file","media":"m_32"}]}"#,
            ErrorCode::RequiredIf,
            Some("/parts/0/name"),
        );
    }

    #[test]
    fn send_audio_bad_media() {
        action_fails(
            r#"{"id":41,"type":"send","conversation":"4242","parts":[{"type":"audio","media":""}]}"#,
            ErrorCode::Grammar,
            Some("/parts/0/media"),
        );
    }

    #[test]
    fn send_ref_without_reply_to() {
        action_fails(
            r#"{"id":41,"type":"send","conversation":"4242","reply_to_platform_ref":"993","parts":[{"type":"text","text":"Done."}]}"#,
            ErrorCode::Forbidden,
            Some("/reply_to_platform_ref"),
        );
    }

    #[test]
    fn send_empty_button_row() {
        action_fails(
            r#"{"id":41,"type":"send","conversation":"4242","parts":[{"type":"text","text":"Done."}],"buttons":[[]]}"#,
            ErrorCode::Empty,
            Some("/buttons/0"),
        );
    }

    #[test]
    fn send_bad_button_token() {
        action_fails(
            r#"{"id":41,"type":"send","conversation":"4242","parts":[{"type":"text","text":"Done."}],"buttons":[[{"label":"Approve","token":"approve:12"}]]}"#,
            ErrorCode::Grammar,
            Some("/buttons/0/0/token"),
        );
    }

    #[test]
    fn send_empty_button_label() {
        action_fails(
            r#"{"id":41,"type":"send","conversation":"4242","parts":[{"type":"text","text":"Done."}],"buttons":[[{"label":"","token":"tk_9f2c"}]]}"#,
            ErrorCode::Empty,
            Some("/buttons/0/0/label"),
        );
    }

    #[test]
    fn id_zero() {
        action_fails(
            r#"{"id":0,"type":"send","conversation":"4242","parts":[{"type":"text","text":"Done."}]}"#,
            ErrorCode::Range,
            Some("/id"),
        );
    }

    #[test]
    fn edit_without_target_refs() {
        action_fails(
            r#"{"id":58,"type":"edit","conversation":"4242","target":57,"parts":[{"type":"text","text":"x"}]}"#,
            ErrorCode::RequiredIf,
            Some("/target_platform_refs"),
        );
    }

    #[test]
    fn edit_with_empty_target_refs() {
        action_fails(
            r#"{"id":58,"type":"edit","conversation":"4242","target":57,"target_platform_refs":[],"parts":[{"type":"text","text":"x"}]}"#,
            ErrorCode::Empty,
            Some("/target_platform_refs"),
        );
    }

    #[test]
    fn edit_with_reply_to() {
        action_fails(
            r#"{"id":58,"type":"edit","conversation":"4242","target":57,"target_platform_refs":["993"],"reply_to":52,"parts":[{"type":"text","text":"x"}]}"#,
            ErrorCode::Forbidden,
            Some("/reply_to"),
        );
    }

    #[test]
    fn answer_empty_action_ref() {
        action_fails(
            r#"{"id":59,"type":"answer_action","conversation":"4242","action_ref":"","text":"ok"}"#,
            ErrorCode::Empty,
            Some("/action_ref"),
        );
    }

    #[test]
    fn unknown_action_type() {
        action_fails(
            r#"{"id":60,"type":"react","conversation":"4242"}"#,
            ErrorCode::Decode,
            None,
        );
    }

    #[test]
    fn response_missing_ephemeral() {
        assert_fails::<OutboxResponseShape>(r#"{"actions":[]}"#, ErrorCode::Decode, None);
    }

    #[test]
    fn ephemeral_not_typing() {
        assert_fails::<OutboxResponseShape>(
            r#"{"actions":[],"ephemeral":[{"type":"send","conversation":"4242"}]}"#,
            ErrorCode::Decode,
            None,
        );
    }

    #[test]
    fn typing_without_conversation() {
        assert_fails::<OutboxResponseShape>(
            r#"{"actions":[],"ephemeral":[{"type":"typing"}]}"#,
            ErrorCode::Decode,
            None,
        );
    }

    #[test]
    fn a_poll_action_error_points_inside_the_poll() {
        assert_fails::<OutboxResponseShape>(
            r#"{"actions":[{"id":41,"type":"send","conversation":"4242","parts":[{"type":"text","text":"ok"}]},{"id":0,"type":"send","conversation":"4242","parts":[{"type":"text","text":"ok"}]}],"ephemeral":[]}"#,
            ErrorCode::Range,
            Some("/actions/1/id"),
        );
    }

    #[test]
    fn a_generated_poll_validates() {
        let poll = OutboxPoll {
            actions: vec![
                OutAction::Send {
                    id: 57,
                    conversation: "-100123:5".to_string(),
                    reply_to: Some(52),
                    reply_to_platform_ref: Some("980".to_string()),
                    parts: vec![
                        OutPart::Text {
                            text: "**Proposal 12**".to_string(),
                        },
                        OutPart::Image {
                            media: "m_31".to_string(),
                            caption: Some("diff".to_string()),
                        },
                        OutPart::File {
                            media: "m_32".to_string(),
                            name: "relatorio.pdf".to_string(),
                        },
                        OutPart::Audio {
                            media: "m_33".to_string(),
                        },
                    ],
                    buttons: Some(vec![vec![Button {
                        label: "Approve".to_string(),
                        token: "tk_9f2c".to_string(),
                    }]]),
                    silent: false,
                },
                OutAction::Edit {
                    id: 58,
                    conversation: "-100123:5".to_string(),
                    target: 57,
                    target_platform_refs: vec!["993".to_string(), "994".to_string()],
                    parts: vec![OutPart::Text {
                        text: "approved".to_string(),
                    }],
                    buttons: None,
                },
                OutAction::AnswerAction {
                    id: 59,
                    conversation: "-100123:5".to_string(),
                    action_ref: "551".to_string(),
                    text: "Already handled.".to_string(),
                },
            ],
            ephemeral: vec![Ephemeral::Typing {
                conversation: "4242".to_string(),
            }],
        };
        let bytes = serde_json::to_vec(&poll).unwrap();
        let valid = decode::<OutboxResponseShape>(&bytes)
            .expect("what the núcleo emits must pass its own validation");
        assert_eq!(valid.actions.len(), 3);
        assert_eq!(valid.ephemeral.len(), 1);
    }
}
