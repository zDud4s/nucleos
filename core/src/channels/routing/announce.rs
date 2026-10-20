//! Feed announcements to the primary channel (Anexo D D.12). A port of the feed half of
//! `sidecars/telegram/pipe` (`feedDestination`, `formatFeed`), moved into the daemon: the cursor
//! is a column, the policy is the channel's own, and the send is an outbox row.

use chrono::{DateTime, Utc};
use nucleos_base::notify_policy::Policy;
use sqlx::SqlitePool;
use std::collections::BTreeMap;

use super::policy::{passes, resolved_policy};
use crate::channels::outbox::{self, Class, NewAction, Subject};
use crate::channels::strings::{self, Text};
use crate::channels::{access, contract, registry};
use crate::state::AppState;

/// Rows read per query; a longer backlog is walked in further batches of the same pass.
const BATCH: i64 = 200;

type FeedRow = (i64, Option<String>, String, String, Option<String>);

/// The text of a feed line. The capture request is a question: the raw kind prefix would only
/// stand in front of it, so it goes out as its summary alone.
fn feed_text(kind: &str, summary: &str) -> String {
    if kind == crate::capture::FEED_KIND {
        summary.to_owned()
    } else {
        strings::render(&Text::Feed {
            kind: kind.to_owned(),
            summary: summary.to_owned(),
        })
    }
}

/// `job:7` on a capture request names the job whose answer the owner is being asked for.
fn capture_subject(kind: &str, subject: Option<&str>) -> Option<Subject> {
    if kind != crate::capture::FEED_KIND {
        return None;
    }
    let id = subject?.strip_prefix("job:")?.parse().ok()?;
    Some(Subject::Capture(id))
}

/// Where one feed line goes: a mapped project thread when the channel has threads, otherwise the
/// home conversation, with the project named in front. Returns the conversation and the text.
fn destination(row: &FeedRow, home: &str, projects: &BTreeMap<String, String>) -> (String, String) {
    let text = feed_text(&row.2, &row.3);
    let Some(project) = row.1.as_deref().filter(|p| !p.is_empty()) else {
        return (home.to_owned(), text);
    };
    if let Some(thread) = projects.get(project) {
        return (thread.clone(), text);
    }
    let prefix = strings::render(&Text::ProjectPrefix {
        project: project.to_owned(),
    });
    (home.to_owned(), format!("{prefix}{text}"))
}

/// Advances the feed cursor of `channel` to at least `to`.
async fn advance_feed_cursor(
    conn: &mut sqlx::SqliteConnection,
    channel: &str,
    to: i64,
) -> sqlx::Result<()> {
    sqlx::query(
        "UPDATE channel_routing_state SET feed_cursor = MAX(feed_cursor, ?) WHERE channel = ?",
    )
    .bind(to)
    .bind(channel)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// With no destination the cursor still moves to the newest line, so nothing bursts out once the
/// destination resolves.
async fn skip_feed(pool: &SqlitePool, channel: &str) -> sqlx::Result<()> {
    sqlx::query(
        "UPDATE channel_routing_state \
         SET feed_cursor = MAX(feed_cursor, (SELECT COALESCE(MAX(id), 0) FROM feed)) \
         WHERE channel = ?",
    )
    .bind(channel)
    .execute(pool)
    .await?;
    Ok(())
}

/// Announces the feed lines past the primary's cursor and moves the cursor, in one transaction per
/// batch. Returns how many lines were enqueued (a line the policy mutes still moves the cursor).
pub(super) async fn announce_feed(
    state: &AppState,
    now: DateTime<Utc>,
    primary: &'static str,
    destination_ok: bool,
) -> sqlx::Result<u32> {
    let pool = &state.pool;
    if !destination_ok {
        skip_feed(pool, primary).await?;
        return Ok(0);
    }
    // The state was `Ok` a moment ago; a home removed since then reads as no destination.
    let Some(home) = access::home_conversation(pool, primary).await? else {
        skip_feed(pool, primary).await?;
        return Ok(0);
    };
    let threads = registry::capabilities(state, primary).await.threads;
    let projects = if threads {
        state
            .channels
            .config(primary)
            .map(|config| config.projects)
            .unwrap_or_default()
    } else {
        BTreeMap::new()
    };

    let mut policy: Option<Policy> = None;
    let mut announced = 0u32;
    loop {
        let cursor: i64 =
            sqlx::query_scalar("SELECT feed_cursor FROM channel_routing_state WHERE channel = ?")
                .bind(primary)
                .fetch_one(pool)
                .await?;
        let rows: Vec<FeedRow> = sqlx::query_as(
            "SELECT id, project_id, kind, summary, subject FROM feed \
             WHERE id > ? ORDER BY id LIMIT ?",
        )
        .bind(cursor)
        .bind(BATCH)
        .fetch_all(pool)
        .await?;
        let Some(last) = rows.last().map(|row| row.0) else {
            break;
        };
        // Read only when there are new lines, once per pass.
        if policy.is_none() {
            policy = Some(resolved_policy(pool, Some(primary)).await?);
        }
        let policy = policy.as_ref().expect("policy was just loaded");

        let mut tx = pool.begin().await?;
        let mut batch = 0u32;
        for row in rows.iter().filter(|row| passes(policy, &row.2)) {
            let (conversation, text) = destination(row, &home, &projects);
            outbox::enqueue(
                &mut tx,
                &contract::ConversationKey::new(primary, conversation),
                NewAction::Send {
                    reply_to: None,
                    parts: vec![contract::OutPart::Text { text }],
                    buttons: None,
                    silent: false,
                },
                Class::Notification,
                capture_subject(&row.2, row.4.as_deref()),
                now,
            )
            .await?;
            batch += 1;
        }
        advance_feed_cursor(&mut tx, primary, last).await?;
        tx.commit().await?;
        if batch > 0 {
            outbox::wake(state, primary);
        }
        announced += batch;
        if (rows.len() as i64) < BATCH {
            break;
        }
    }
    Ok(announced)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Arc;

    use chrono::{SecondsFormat, TimeZone};
    use nucleos_base::notify_policy::Rule;

    use super::*;
    use crate::channels::registry::ChannelConfig;
    use crate::channels::routing::{self, Wanted};
    use crate::channels::{ChannelsRuntime, testkit};

    const HOME: &str = "-100";

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 10, 20, 12, 0, 0).unwrap()
    }

    /// A state whose primary is `telegram`, configured, with a home and a seeded routing row.
    /// `projects` are the channel's thread map; `threads` the capability its hello declared.
    async fn ready(threads: bool, projects: &[(&str, &str)], with_home: bool) -> AppState {
        let mut state = testkit::state().await;
        let map: BTreeMap<String, String> = projects
            .iter()
            .map(|(p, t)| ((*p).to_owned(), (*t).to_owned()))
            .collect();
        let mut configs = HashMap::new();
        configs.insert(
            "telegram",
            ChannelConfig {
                doctrine: None,
                projects: map,
            },
        );
        state.channels = Arc::new(ChannelsRuntime::with_configs(configs));
        state
            .channels
            .presence
            .set_secrets_present("telegram", Vec::new());
        if threads {
            let capabilities = contract::Capabilities {
                threads: true,
                ..Default::default()
            };
            registry::record_hello(
                &state,
                "telegram",
                &contract::Hello {
                    kind: "telegram".into(),
                    version: contract::CONTRACT_VERSION.into(),
                    capabilities,
                },
                now(),
            )
            .await
            .unwrap();
        }
        sqlx::query("UPDATE channels_global SET primary_channel = 'telegram'")
            .execute(&state.pool)
            .await
            .unwrap();
        if with_home {
            sqlx::query(
                "INSERT INTO channel_identities (channel, sender_ref, conversation_ref, \
                 conversation_kind, home, created_at) VALUES ('telegram', '42', ?, 'group', 1, 't')",
            )
            .bind(HOME)
            .execute(&state.pool)
            .await
            .unwrap();
        }
        let mut conn = state.pool.acquire().await.unwrap();
        routing::seed(&mut conn, "telegram").await.unwrap();
        drop(conn);
        state
    }

    async fn line(
        state: &AppState,
        project: Option<&str>,
        kind: &str,
        summary: &str,
        subject: Option<&nucleos_base::feed::Subject>,
    ) -> i64 {
        nucleos_base::feed::append(&state.pool, project, kind, summary, None, subject)
            .await
            .unwrap()
    }

    fn feed_only() -> Wanted {
        Wanted {
            feed: true,
            ..Wanted::default()
        }
    }

    /// `(conversation, text, class, subject, expires_at)` of every outbox row, oldest first.
    async fn sent(
        state: &AppState,
    ) -> Vec<(String, String, String, Option<String>, Option<String>)> {
        let rows: Vec<(String, String, String, Option<String>, Option<String>)> = sqlx::query_as(
            "SELECT conversation, payload, class, subject, expires_at \
             FROM channel_outbox ORDER BY id",
        )
        .fetch_all(&state.pool)
        .await
        .unwrap();
        rows.into_iter()
            .map(|(conversation, payload, class, subject, expires)| {
                let payload: serde_json::Value = serde_json::from_str(&payload).unwrap();
                let text = payload["parts"][0]["text"].as_str().unwrap().to_owned();
                (conversation, text, class, subject, expires)
            })
            .collect()
    }

    async fn feed_cursor(state: &AppState) -> i64 {
        sqlx::query_scalar(
            "SELECT feed_cursor FROM channel_routing_state WHERE channel = 'telegram'",
        )
        .fetch_one(&state.pool)
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn route_feed_announces_summary_kind_to_primary_home() {
        let state = ready(false, &[], true).await;
        let id = line(&state, None, "job_done", "built", None).await;
        let report = routing::route_once(&state, now(), feed_only())
            .await
            .unwrap();
        assert_eq!(report.feed, 1);
        let rows = sent(&state).await;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].0, HOME);
        assert_eq!(rows[0].1, "job_done: built");
        assert_eq!(rows[0].2, "notification");
        assert_eq!(feed_cursor(&state).await, id);

        // Nothing new: nothing more is sent.
        let again = routing::route_once(&state, now(), feed_only())
            .await
            .unwrap();
        assert_eq!(again.feed, 0);
        assert_eq!(sent(&state).await.len(), 1);
    }

    #[tokio::test]
    async fn route_feed_project_row_goes_to_mapped_thread_or_prefixed_home() {
        // With threads: the mapped project goes to its thread, plain; an unmapped one is prefixed.
        let state = ready(true, &[("alpha", "-100:5")], true).await;
        line(&state, Some("alpha"), "job_done", "a", None).await;
        line(&state, Some("beta"), "job_done", "b", None).await;
        routing::route_once(&state, now(), feed_only())
            .await
            .unwrap();
        let rows = sent(&state).await;
        assert_eq!(
            (rows[0].0.as_str(), rows[0].1.as_str()),
            ("-100:5", "job_done: a")
        );
        assert_eq!(
            (rows[1].0.as_str(), rows[1].1.as_str()),
            (HOME, "[beta] job_done: b")
        );

        // Without threads the mapping is ignored: home, prefixed.
        let state = ready(false, &[("alpha", "-100:5")], true).await;
        line(&state, Some("alpha"), "job_done", "a", None).await;
        routing::route_once(&state, now(), feed_only())
            .await
            .unwrap();
        let rows = sent(&state).await;
        assert_eq!(
            (rows[0].0.as_str(), rows[0].1.as_str()),
            (HOME, "[alpha] job_done: a")
        );
    }

    #[tokio::test]
    async fn route_capture_requested_has_capture_subject() {
        let state = ready(false, &[], true).await;
        line(
            &state,
            None,
            crate::capture::FEED_KIND,
            "Job 7 failed. What now?",
            Some(&nucleos_base::feed::Subject::Job(7)),
        )
        .await;
        routing::route_once(&state, now(), feed_only())
            .await
            .unwrap();
        let rows = sent(&state).await;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].1, "Job 7 failed. What now?");
        assert_eq!(rows[0].3.as_deref(), Some("capture:7"));
    }

    #[tokio::test]
    async fn route_respects_the_channel_policy() {
        let state = ready(false, &[], true).await;
        let muted = Policy {
            families: Vec::new(),
            kinds: vec![Rule {
                selector: "job_done".into(),
                enabled: false,
            }],
        };
        routing::put_channel_policy(&state.pool, "telegram", &muted, now())
            .await
            .unwrap();
        line(&state, None, "job_done", "quiet", None).await;
        let last = line(&state, None, "job_failed", "loud", None).await;
        let report = routing::route_once(&state, now(), feed_only())
            .await
            .unwrap();
        assert_eq!(report.feed, 1);
        let rows = sent(&state).await;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].1, "job_failed: loud");
        // The muted line still moved the cursor.
        assert_eq!(feed_cursor(&state).await, last);
    }

    #[tokio::test]
    async fn route_without_destination_advances_cursors_and_enqueues_nothing() {
        let state = ready(false, &[], false).await;
        let last = line(&state, None, "job_done", "x", None).await;
        let report = routing::route_once(&state, now(), feed_only())
            .await
            .unwrap();
        assert_eq!(report, routing::RouteReport::default());
        assert!(sent(&state).await.is_empty());
        assert_eq!(feed_cursor(&state).await, last);

        // With no primary at all nothing is enqueued either.
        sqlx::query("UPDATE channels_global SET primary_channel = NULL")
            .execute(&state.pool)
            .await
            .unwrap();
        line(&state, None, "job_done", "y", None).await;
        routing::route_once(&state, now(), feed_only())
            .await
            .unwrap();
        assert!(sent(&state).await.is_empty());
    }

    #[tokio::test]
    async fn route_notification_ttl_is_24h_and_approvals_have_none() {
        let state = ready(false, &[], true).await;
        line(&state, None, "job_done", "x", None).await;
        routing::route_once(&state, now(), feed_only())
            .await
            .unwrap();
        let rows = sent(&state).await;
        let expected =
            (now() + routing::NOTIFICATION_TTL).to_rfc3339_opts(SecondsFormat::Secs, true);
        assert_eq!(rows[0].4.as_deref(), Some(expected.as_str()));

        // An approval request is enqueued with the same call and never expires by TTL.
        let mut conn = state.pool.acquire().await.unwrap();
        outbox::enqueue(
            &mut conn,
            &contract::ConversationKey::new("telegram", HOME),
            NewAction::Send {
                reply_to: None,
                parts: vec![contract::OutPart::Text {
                    text: "approve?".into(),
                }],
                buttons: None,
                silent: false,
            },
            Class::Approval,
            Some(Subject::Proposal(1)),
            now(),
        )
        .await
        .unwrap();
        drop(conn);
        let rows = sent(&state).await;
        assert_eq!(rows[1].2, "approval");
        assert_eq!(rows[1].4, None);
    }
}
