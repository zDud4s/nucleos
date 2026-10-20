//! Outbox degradation of unsupported content (section 6.5, Anexo D D.10.2). Applied to the copy
//! handed to the adapter at lease time; the outbox row itself stays as it was enqueued.

use sqlx::SqliteConnection;

use super::Subject;
use crate::channels::contract::{Button, Capabilities, MediaKind, OutAction, OutPart};
use crate::channels::strings::{self, Text};

type Buttons = Option<Vec<Vec<Button>>>;

/// The few columns of another outbox row that degradation needs.
struct Target {
    state: String,
    refs: Vec<String>,
    replaced_by: Option<i64>,
    conversation: String,
}

impl Target {
    /// A settled send whose platform message the adapter can still address.
    fn addressable(&self) -> bool {
        self.state == "done" && !self.refs.is_empty()
    }
}

async fn target(
    conn: &mut SqliteConnection,
    channel: &str,
    id: i64,
) -> sqlx::Result<Option<Target>> {
    let row: Option<(String, Option<String>, Option<i64>, String)> = sqlx::query_as(
        "SELECT state, platform_refs, replaced_by, conversation FROM channel_outbox \
         WHERE id = ? AND channel = ?",
    )
    .bind(id)
    .bind(channel)
    .fetch_optional(&mut *conn)
    .await?;
    Ok(row.map(|(state, refs, replaced_by, conversation)| Target {
        state,
        refs: refs
            .and_then(|text| serde_json::from_str::<Vec<String>>(&text).ok())
            .unwrap_or_default(),
        replaced_by,
        conversation,
    }))
}

/// The conversation a send that replaces an edit must go to: where the failed target ended up
/// (`replaced_by` chain), else the edit's own conversation.
async fn substitute_conversation(
    conn: &mut SqliteConnection,
    channel: &str,
    found: Option<&Target>,
    own: String,
) -> sqlx::Result<String> {
    let mut conversation = own;
    let mut next = found.and_then(|t| t.replaced_by);
    // The chain is short (one fallback per hop); the bound only guards against a corrupt loop.
    for _ in 0..8 {
        let Some(id) = next else { break };
        let Some(row) = target(conn, channel, id).await? else {
            break;
        };
        conversation = row.conversation;
        next = row.replaced_by;
    }
    Ok(conversation)
}

/// `media::display_name` over a connection (that one takes a pool): the stored name, else the mime.
async fn display_name(conn: &mut SqliteConnection, media_id: &str) -> sqlx::Result<Option<String>> {
    let row: Option<(Option<String>, String)> =
        sqlx::query_as("SELECT name, mime FROM channel_media WHERE id = ?")
            .bind(media_id)
            .fetch_optional(&mut *conn)
            .await?;
    Ok(row.map(|(name, mime)| name.filter(|n| !n.is_empty()).unwrap_or(mime)))
}

fn text_part(text: Text) -> OutPart {
    OutPart::Text {
        text: strings::render(&text),
    }
}

/// A media part of a kind the channel cannot send becomes a text part naming the attachment.
async fn degrade_media(
    conn: &mut SqliteConnection,
    caps: &Capabilities,
    parts: Vec<OutPart>,
) -> sqlx::Result<Vec<OutPart>> {
    let mut out = Vec::with_capacity(parts.len());
    for part in parts {
        let (kind, media, label, caption) = match part {
            OutPart::Image { media, caption } => (MediaKind::Image, media, None, caption),
            OutPart::File { media, name } => (MediaKind::File, media, Some(name), None),
            OutPart::Audio { media } => (MediaKind::Audio, media, None, None),
            text @ OutPart::Text { .. } => {
                out.push(text);
                continue;
            }
        };
        if caps.media_out.contains(&kind) {
            out.push(match kind {
                MediaKind::Image => OutPart::Image { media, caption },
                MediaKind::File => OutPart::File {
                    media,
                    name: label.unwrap_or_default(),
                },
                MediaKind::Audio => OutPart::Audio { media },
            });
            continue;
        }
        let name = match label {
            Some(name) => name,
            None => display_name(conn, &media).await?.unwrap_or(media),
        };
        out.push(text_part(Text::MediaUnavailable { name }));
        if let Some(caption) = caption.filter(|c| !c.is_empty()) {
            out.push(OutPart::Text { text: caption });
        }
    }
    Ok(out)
}

/// Without `buttons`: drops them and, for an approval request (`subject = proposal:<id>`), adds the
/// "reply approve N / reject N" text. Without a proposal subject the buttons are only dropped.
async fn degrade_buttons(
    conn: &mut SqliteConnection,
    caps: &Capabilities,
    row_id: i64,
    parts: &mut Vec<OutPart>,
    buttons: Buttons,
) -> sqlx::Result<Buttons> {
    if caps.buttons || buttons.is_none() {
        return Ok(buttons);
    }
    let subject: Option<Option<String>> =
        sqlx::query_scalar("SELECT subject FROM channel_outbox WHERE id = ?")
            .bind(row_id)
            .fetch_optional(&mut *conn)
            .await?;
    if let Some(Subject::Proposal(id)) = subject.flatten().as_deref().and_then(Subject::from_text) {
        parts.push(text_part(Text::ReplyApproveReject { id }));
    }
    Ok(None)
}

/// Rewrites `action` so it only uses what `caps` supports, and joins the platform refs the adapter
/// needs to address earlier messages.
pub(super) async fn degrade(
    conn: &mut SqliteConnection,
    channel: &str,
    caps: &Capabilities,
    action: OutAction,
) -> sqlx::Result<OutAction> {
    match action {
        OutAction::Send {
            id,
            conversation,
            reply_to,
            reply_to_platform_ref: _,
            parts,
            buttons,
            silent,
        } => {
            let mut parts = degrade_media(conn, caps, parts).await?;
            let buttons = degrade_buttons(conn, caps, id, &mut parts, buttons).await?;
            let mut reply_ref = None;
            let mut reply_to = reply_to;
            if let Some(target_id) = reply_to {
                match target(conn, channel, target_id).await? {
                    Some(t) if t.addressable() => reply_ref = t.refs.into_iter().next(),
                    _ => reply_to = None,
                }
            }
            Ok(OutAction::Send {
                id,
                conversation,
                reply_to,
                reply_to_platform_ref: reply_ref,
                parts,
                buttons,
                silent,
            })
        }
        OutAction::Edit {
            id,
            conversation,
            target: target_id,
            target_platform_refs: _,
            parts,
            buttons,
        } => {
            let mut parts = degrade_media(conn, caps, parts).await?;
            let buttons = degrade_buttons(conn, caps, id, &mut parts, buttons).await?;
            let found = target(conn, channel, target_id).await?;
            let editable = caps.edit
                && found
                    .as_ref()
                    .is_some_and(|t| t.addressable() && t.replaced_by.is_none());
            if editable && let Some(t) = found {
                return Ok(OutAction::Edit {
                    id,
                    conversation,
                    target: target_id,
                    target_platform_refs: t.refs,
                    parts,
                    buttons,
                });
            }
            let conversation =
                substitute_conversation(conn, channel, found.as_ref(), conversation).await?;
            Ok(OutAction::Send {
                id,
                conversation,
                reply_to: None,
                reply_to_platform_ref: None,
                parts,
                buttons,
                silent: false,
            })
        }
        answer @ OutAction::AnswerAction { .. } => Ok(answer),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};
    use sqlx::SqlitePool;

    fn full_caps() -> Capabilities {
        Capabilities {
            buttons: true,
            edit: true,
            reply_to: true,
            media_out: vec![MediaKind::Image, MediaKind::File, MediaKind::Audio],
            ..Capabilities::default()
        }
    }

    async fn insert(
        pool: &SqlitePool,
        conversation: &str,
        state: &str,
        refs: Option<&str>,
        replaced_by: Option<i64>,
        subject: Option<&str>,
    ) -> i64 {
        sqlx::query_scalar(
            "INSERT INTO channel_outbox (channel, conversation, lane, action, payload, class, \
             subject, state, due_at, platform_refs, replaced_by, attempts, created_at) \
             VALUES ('fake', ?, 'ordered', 'send', '{}', 'turn', ?, ?, \
             '2026-10-19T09:00:00Z', ?, ?, 0, '2026-10-19T09:00:00Z') RETURNING id",
        )
        .bind(conversation)
        .bind(subject)
        .bind(state)
        .bind(refs)
        .bind(replaced_by)
        .fetch_one(pool)
        .await
        .unwrap()
    }

    fn one_button() -> Buttons {
        Some(vec![vec![Button {
            label: "Approve".to_string(),
            token: "tk".to_string(),
        }]])
    }

    fn hi() -> Vec<OutPart> {
        vec![OutPart::Text {
            text: "hi".to_string(),
        }]
    }

    fn send(id: i64, reply_to: Option<i64>, parts: Vec<OutPart>, buttons: Buttons) -> OutAction {
        OutAction::Send {
            id,
            conversation: "c1".to_string(),
            reply_to,
            reply_to_platform_ref: None,
            parts,
            buttons,
            silent: false,
        }
    }

    fn edit(id: i64, target: i64) -> OutAction {
        OutAction::Edit {
            id,
            conversation: "c1".to_string(),
            target,
            target_platform_refs: Vec::new(),
            parts: hi(),
            buttons: None,
        }
    }

    async fn run(pool: &SqlitePool, caps: &Capabilities, action: OutAction) -> Value {
        let mut conn = pool.acquire().await.unwrap();
        let got = degrade(&mut conn, "fake", caps, action).await.unwrap();
        serde_json::to_value(got).unwrap()
    }

    fn texts(value: &Value) -> Vec<String> {
        value["parts"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|p| p["type"] == "text")
            .map(|p| p["text"].as_str().unwrap().to_string())
            .collect()
    }

    #[tokio::test]
    async fn no_buttons_degrades_to_reply_text() {
        let state = crate::channels::testkit::state().await;
        let pool = &state.pool;
        let id = insert(pool, "c1", "pending", None, None, Some("proposal:12")).await;
        let got = run(
            pool,
            &Capabilities::default(),
            send(id, None, hi(), one_button()),
        )
        .await;
        assert!(got.get("buttons").is_none(), "{got}");
        let all = texts(&got);
        assert_eq!(all[0], "hi");
        let last = all.last().unwrap();
        assert!(
            last.contains("approve 12") && last.contains("reject 12"),
            "{last}"
        );
    }

    #[tokio::test]
    async fn no_buttons_and_no_subject_just_drops_them() {
        let state = crate::channels::testkit::state().await;
        let pool = &state.pool;
        let id = insert(pool, "c1", "pending", None, None, None).await;
        let got = run(
            pool,
            &Capabilities::default(),
            send(id, None, hi(), one_button()),
        )
        .await;
        assert!(got.get("buttons").is_none(), "{got}");
        assert_eq!(texts(&got), vec!["hi".to_string()]);
    }

    #[tokio::test]
    async fn missing_media_out_kind_becomes_text() {
        let state = crate::channels::testkit::state().await;
        let pool = &state.pool;
        sqlx::query(
            "INSERT INTO channel_media (id, channel, path, mime, name, bytes, created_at) \
             VALUES ('m1', 'fake', 'p', 'image/png', 'cat.png', 1, 't')",
        )
        .execute(pool)
        .await
        .unwrap();
        let id = insert(pool, "c1", "pending", None, None, None).await;
        let parts = vec![
            OutPart::Image {
                media: "m1".to_string(),
                caption: None,
            },
            OutPart::File {
                media: "m2".to_string(),
                name: "a.txt".to_string(),
            },
        ];
        let caps = Capabilities {
            media_out: vec![MediaKind::File],
            ..Capabilities::default()
        };
        let got = run(pool, &caps, send(id, None, parts, None)).await;
        let image_text = &got["parts"][0];
        assert_eq!(image_text["type"], "text");
        assert!(
            image_text["text"].as_str().unwrap().contains("cat.png"),
            "{got}"
        );
        assert_eq!(
            got["parts"][1],
            json!({"type": "file", "media": "m2", "name": "a.txt"})
        );
    }

    #[tokio::test]
    async fn edit_without_the_capability_becomes_a_send() {
        let state = crate::channels::testkit::state().await;
        let pool = &state.pool;
        let target = insert(pool, "c1", "done", Some(r#"["p1"]"#), None, None).await;
        let id = insert(pool, "c1", "pending", None, None, None).await;
        let caps = Capabilities {
            edit: false,
            ..full_caps()
        };
        let got = run(pool, &caps, edit(id, target)).await;
        assert_eq!(got["type"], "send");
        assert_eq!(got["id"], id);
        assert_eq!(got["conversation"], "c1");
        assert_eq!(texts(&got), vec!["hi".to_string()]);
    }

    #[tokio::test]
    async fn edit_of_a_replaced_target_goes_to_the_substitute() {
        let state = crate::channels::testkit::state().await;
        let pool = &state.pool;
        let substitute = insert(pool, "home", "done", Some(r#"["p9"]"#), None, None).await;
        let target = insert(pool, "thread", "failed", None, Some(substitute), None).await;
        let id = insert(pool, "thread", "pending", None, None, None).await;
        let got = run(pool, &full_caps(), edit(id, target)).await;
        assert_eq!(got["type"], "send");
        assert_eq!(got["conversation"], "home");
    }

    #[tokio::test]
    async fn edit_carries_target_platform_refs() {
        let state = crate::channels::testkit::state().await;
        let pool = &state.pool;
        let target = insert(pool, "c1", "done", Some(r#"["p1","p2"]"#), None, None).await;
        let id = insert(pool, "c1", "pending", None, None, None).await;
        let got = run(pool, &full_caps(), edit(id, target)).await;
        assert_eq!(got["type"], "edit");
        assert_eq!(got["target"], target);
        assert_eq!(got["target_platform_refs"], json!(["p1", "p2"]));
    }

    #[tokio::test]
    async fn edit_of_a_done_target_without_refs_becomes_a_send() {
        let state = crate::channels::testkit::state().await;
        let pool = &state.pool;
        let target = insert(pool, "c1", "done", Some("[]"), None, None).await;
        let no_refs = insert(pool, "c1", "done", None, None, None).await;
        let id = insert(pool, "c1", "pending", None, None, None).await;
        for t in [target, no_refs] {
            let got = run(pool, &full_caps(), edit(id, t)).await;
            assert_eq!(got["type"], "send", "{got}");
            assert_eq!(got["conversation"], "c1");
        }
    }

    #[tokio::test]
    async fn reply_to_a_target_without_refs_drops_the_reply_to() {
        let state = crate::channels::testkit::state().await;
        let pool = &state.pool;
        let target = insert(pool, "c1", "done", None, None, None).await;
        let failed = insert(pool, "c1", "failed", None, None, None).await;
        let id = insert(pool, "c1", "pending", None, None, None).await;
        for t in [target, failed, 9999] {
            let got = run(pool, &full_caps(), send(id, Some(t), hi(), None)).await;
            assert!(got.get("reply_to").is_none(), "{got}");
            assert!(got.get("reply_to_platform_ref").is_none(), "{got}");
        }
    }

    #[tokio::test]
    async fn send_with_reply_to_carries_the_platform_ref() {
        let state = crate::channels::testkit::state().await;
        let pool = &state.pool;
        let target = insert(pool, "c1", "done", Some(r#"["p1","p2"]"#), None, None).await;
        let id = insert(pool, "c1", "pending", None, None, None).await;
        let got = run(pool, &full_caps(), send(id, Some(target), hi(), None)).await;
        assert_eq!(got["reply_to"], target);
        assert_eq!(got["reply_to_platform_ref"], "p1");
    }

    #[tokio::test]
    async fn a_fully_capable_adapter_gets_the_action_unchanged() {
        let state = crate::channels::testkit::state().await;
        let pool = &state.pool;
        let id = insert(pool, "c1", "pending", None, None, Some("proposal:12")).await;
        let parts = vec![
            OutPart::Text {
                text: "hi".to_string(),
            },
            OutPart::Audio {
                media: "m1".to_string(),
            },
        ];
        let action = send(id, None, parts, one_button());
        let before = serde_json::to_value(&action).unwrap();
        assert_eq!(run(pool, &full_caps(), action).await, before);

        let answer = OutAction::AnswerAction {
            id,
            conversation: "c1".to_string(),
            action_ref: "cb".to_string(),
            text: "ok".to_string(),
        };
        let before = serde_json::to_value(&answer).unwrap();
        assert_eq!(run(pool, &Capabilities::default(), answer).await, before);
    }
}
