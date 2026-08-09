//! Watching what a local model would redact, without letting it redact anything.
//!
//! `redact.rs` enforces, and can only enforce what arithmetic proves: an IBAN survives mod-97, a
//! GitHub token carries its issuer's prefix. A person's name has no check digit. Deciding whether
//! it is a name needs judgement, judgement here means a 4B model, and a 4B that is wrong one time
//! in ten cannot be given the power to cut words out of a stranger's message on the way to a model
//! that is trying to answer a question about it.
//!
//! So this half observes. It writes what it would have removed and removes nothing, and in a few
//! months the rows are what answer the only question that matters: would redacting this class have
//! cost anything a person needed?
//!
//! **What it reads is the whole safety argument.** Only columns the source keeps for ever — a
//! subject, a sender's name, and the summary a local model wrote. Never `emails.body_text`, which
//! triage sets to NULL when it is done: an excerpt of a body would outlive the body, and a
//! measurement that quietly undoes a retention decision is worse than no measurement.
//!
//! It is best-effort throughout. Nothing here blocks a verdict, fails a triage run, or is retried.
//! An observation that did not happen is a gap in a sample; a triage that did not happen is mail
//! nobody read.

use serde::Deserialize;

/// Columns this pass is allowed to read, matching the CHECK constraint in migration 0052.
///
/// Duplicated between Rust and SQL deliberately: the database refuses a bad write and this refuses
/// to attempt one, so a mistake is caught where it is made rather than as a constraint violation in
/// a log nobody reads. `column_list_matches_the_migration` is what keeps the two equal.
pub const OBSERVABLE_COLUMNS: &[&str] = &["subject", "from_name", "triage_summary"];

/// How much of one field is shown to the model.
///
/// A summary is a sentence or two and a subject is a line, so this is generous rather than tight.
/// It exists because the field is third-party text in the `subject` case, and any length that
/// arrives from outside needs a bound before it reaches a context window.
const FIELD_CAP: usize = 2_000;

#[derive(Debug, Clone, PartialEq)]
pub struct Observation {
    pub class: String,
    pub excerpt: String,
    pub confidence: Option<f64>,
}

#[derive(Debug, Deserialize)]
struct RawObservation {
    class: String,
    excerpt: String,
    #[serde(default)]
    confidence: Option<f64>,
}

/// The classes worth telling apart. Anything else the model invents lands in `other` rather than
/// being dropped, because an unexpected class is a finding about the prompt.
const KNOWN_CLASSES: &[&str] = &["name", "address", "health", "financial"];

/// The instruction given to the local model.
///
/// It asks for spans that are actually present, and the `excerpt` requirement is what makes that
/// checkable: a model inventing a name produces an excerpt the source does not contain, and
/// `parse_observations` drops it. That check is worth more than any wording here, because wording
/// cannot stop a small model from confabulating and a substring test can.
pub fn prompt_for(field: &str, text: &str) -> String {
    let capped: String = text.chars().take(FIELD_CAP).collect();
    format!(
        "Find personal data in the text below. Answer with a JSON array and nothing else. \
Each entry: {{\"class\": one of name|address|health|financial, \"excerpt\": the exact substring \
from the text, \"confidence\": 0.0 to 1.0}}. Copy each excerpt character for character from the \
text; do not paraphrase, translate or shorten it. If there is no personal data, answer [].\n\n\
Field: {field}\nText:\n{capped}"
    )
}

/// PURE: the observations in a model's answer that are actually supported by the text.
///
/// Three filters, and each drops a specific failure seen from small models rather than a
/// hypothetical one:
///
/// - An excerpt the source does not contain is a confabulation. This is the load-bearing check:
///   the model is asked to quote, so anything it cannot quote it did not find.
/// - An empty excerpt is a hit with nothing behind it, which would count towards a decision while
///   carrying no evidence for it.
/// - A confidence outside 0..=1 is dropped to `None` rather than clamped, because a model that
///   answered `95` meant percent and clamping would silently record `1.0` for it.
///
/// Returns `None` when the answer could not be READ at all, which is not the same as
/// `Some(vec![])`. An empty array is the model saying it found nothing, and that is a measurement —
/// it is the denominator. An answer that does not parse is the model failing, and recording that as
/// "found nothing" would write a permanent clean verdict for a summary nobody successfully looked
/// at: the sweep's `NOT EXISTS` clause never revisits a summary that has a row, so the denominator
/// this table exists to produce would quietly absorb every garbled reply.
pub fn parse_observations(answer: &str, source: &str) -> Option<Vec<Observation>> {
    let raw = serde_json::from_str::<Vec<RawObservation>>(answer.trim()).ok()?;

    let mut seen = std::collections::HashSet::new();
    let observations = raw
        .into_iter()
        .filter(|entry| !entry.excerpt.trim().is_empty())
        .filter(|entry| source.contains(entry.excerpt.trim()))
        .filter(|entry| seen.insert((entry.class.clone(), entry.excerpt.trim().to_string())))
        .map(|entry| Observation {
            class: if KNOWN_CLASSES.contains(&entry.class.as_str()) {
                entry.class
            } else {
                "other".to_string()
            },
            excerpt: entry.excerpt.trim().to_string(),
            confidence: entry
                .confidence
                .filter(|value| (0.0..=1.0).contains(value)),
        })
        .collect();
    Some(observations)
}

/// Records observations, ignoring ones already recorded for the same source.
///
/// `INSERT OR IGNORE` against the unique index rather than a read-then-write: re-triaging a message
/// re-runs this pass over the same text, and the second run must not double the count that a
/// decision will later be read off.
pub async fn record(
    pool: &sqlx::SqlitePool,
    source_table: &str,
    source_id: i64,
    source_column: &str,
    observations: &[Observation],
    observed_at: &str,
) -> sqlx::Result<u64> {
    debug_assert!(
        OBSERVABLE_COLUMNS.contains(&source_column),
        "{source_column} is not a column this pass may read"
    );

    let mut written = 0;
    for observation in observations {
        let result = sqlx::query(
            "INSERT OR IGNORE INTO pii_observations
                 (source_table, source_id, source_column, class, excerpt, confidence, observed_at)
             VALUES (?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(source_table)
        .bind(source_id)
        .bind(source_column)
        .bind(&observation.class)
        .bind(&observation.excerpt)
        .bind(observation.confidence)
        .bind(observed_at)
        .execute(pool)
        .await?;
        written += result.rows_affected();
    }
    Ok(written)
}

/// How many messages one sweep looks at.
///
/// Bounded because each one is a model call: an unbounded sweep on a mailbox that has just been
/// backfilled would occupy the local model for hours, and the local model is also what answers
/// triage and, when configured, the chat.
const SWEEP_BATCH: i64 = 20;

/// How often a sweep runs. Slow on purpose — this is measurement, and nothing waits for it.
const SWEEP_INTERVAL: std::time::Duration = std::time::Duration::from_secs(900);

/// Observes the summaries triage has written and not yet been looked at.
///
/// A sweep rather than a step inside triage, and the reason is what "best-effort" has to mean: a
/// model call on the triage path would make an observation's failure into a verdict's failure, and
/// a slow local model into slow mail. Nothing here can hold anything up, because nothing here is on
/// the way to anywhere.
///
/// Returns how many observations were written, for the log line.
pub async fn observe_pending(
    pool: &sqlx::SqlitePool,
    client: &reqwest::Client,
    base_url: &str,
    model: &str,
    now: &str,
) -> sqlx::Result<u64> {
    // Only summaries, and only ones not yet observed. `triage_summary` is written by a local model
    // over a body that is then deleted, which makes it the one retained field where a stranger's
    // personal data plausibly survives — and therefore the one worth measuring.
    let pending: Vec<(i64, String)> = sqlx::query_as(
        "SELECT e.id, e.triage_summary
           FROM emails e
          WHERE e.triage_summary IS NOT NULL
            AND e.triage_summary <> ''
            AND NOT EXISTS (SELECT 1 FROM pii_observations o
                             WHERE o.source_table = 'emails'
                               AND o.source_id = e.id
                               AND o.source_column = 'triage_summary')
          ORDER BY e.id DESC
          LIMIT ?",
    )
    .bind(SWEEP_BATCH)
    .fetch_all(pool)
    .await?;

    let mut written = 0;
    for (id, summary) in pending {
        let answer = crate::runner::ollama_chat(
            client,
            base_url,
            model,
            &prompt_for("triage_summary", &summary),
            serde_json::json!({"num_ctx": crate::triage::LOCAL_NUM_CTX}),
            Some(serde_json::json!("json")),
            false,
        )
        .await;

        let observations = match answer {
            Ok(answer) => parse_observations(&answer, &summary),
            Err(error) => {
                // One warning and out. Retrying inside a sweep that runs again in fifteen minutes
                // would turn a stopped Ollama into a log full of the same line.
                tracing::warn!(%error, "pii shadow: local model unreachable, skipping this sweep");
                return Ok(written);
            }
        };

        // An answer nobody could read is recorded as `unreadable`, which is neither a finding nor a
        // clean reading — it is its own outcome and its own measurement.
        //
        // Leaving it unrecorded to be retried was the first attempt and it deadlocks the sweep:
        // the query takes the twenty newest unobserved summaries, so twenty the model garbles
        // deterministically are twenty it garbles again every fifteen minutes, and everything older
        // is never reached. A frozen denominator is worse than an honest gap in it.
        let observations = match observations {
            Some(observations) => observations,
            None => {
                tracing::warn!(email_id = id, "pii shadow: answer could not be read");
                written += record(
                    pool,
                    "emails",
                    id,
                    "triage_summary",
                    &[Observation {
                        class: "unreadable".to_string(),
                        excerpt: String::new(),
                        confidence: None,
                    }],
                    now,
                )
                .await?;
                continue;
            }
        };

        // A summary with nothing in it still has to be marked as looked at, or every sweep for ever
        // reconsiders the same clean ones and never reaches the new. `none` is that mark, and it is
        // also the measurement: it is the denominator.
        let to_write = if observations.is_empty() {
            vec![Observation {
                class: "none".to_string(),
                excerpt: String::new(),
                confidence: None,
            }]
        } else {
            observations
        };
        written += record(pool, "emails", id, "triage_summary", &to_write, now).await?;
    }

    Ok(written)
}

/// Runs a sweep every `SWEEP_INTERVAL` for as long as the daemon is up.
pub async fn run_sweep_loop(pool: sqlx::SqlitePool, base_url: String, model: String) {
    let client = reqwest::Client::new();
    let mut ticker = tokio::time::interval(SWEEP_INTERVAL);
    loop {
        ticker.tick().await;
        let now = chrono::Utc::now().to_rfc3339();
        match observe_pending(&pool, &client, &base_url, &model, &now).await {
            Ok(0) => {}
            Ok(written) => tracing::info!(written, "pii shadow: observations recorded"),
            Err(error) => tracing::warn!(%error, "pii shadow: sweep failed"),
        }
    }
}

/// What the observation window has seen, by class. The report the table exists to produce.
pub async fn tally(pool: &sqlx::SqlitePool) -> sqlx::Result<Vec<(String, i64)>> {
    sqlx::query_as("SELECT class, COUNT(*) FROM pii_observations GROUP BY class ORDER BY 2 DESC")
        .fetch_all(pool)
        .await
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn test_pool() -> sqlx::SqlitePool {
        let pool = sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        pool
    }

    /// The check the whole pass rests on. A small model asked to quote will sometimes produce a
    /// plausible name that is not in the text, and a measurement built from those describes the
    /// model rather than the mail.
    #[test]
    fn an_excerpt_the_source_does_not_contain_is_dropped() {
        let source = "Rita asked about the July invoice";
        let answer = r#"[
            {"class":"name","excerpt":"Rita","confidence":0.9},
            {"class":"name","excerpt":"Joana","confidence":0.9}
        ]"#;

        let found = parse_observations(answer, source).expect("a JSON array parses");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].excerpt, "Rita");
    }

    #[test]
    fn an_unknown_class_is_kept_as_other_rather_than_dropped() {
        let source = "meeting at Rua das Flores 3";
        let answer = r#"[{"class":"postal_code","excerpt":"Rua das Flores 3"}]"#;

        let found = parse_observations(answer, source).expect("a JSON array parses");
        assert_eq!(found[0].class, "other");
        assert_eq!(found[0].confidence, None);
    }

    /// A model that answered `95` meant percent. Clamping would record it as certainty; dropping it
    /// records that there was no usable confidence, which is true.
    #[test]
    fn a_confidence_outside_zero_to_one_is_dropped_not_clamped() {
        let source = "Rita";
        for raw in ["95", "-1", "1.5"] {
            let answer = format!(r#"[{{"class":"name","excerpt":"Rita","confidence":{raw}}}]"#);
            assert_eq!(
                parse_observations(&answer, source).unwrap()[0].confidence,
                None
            );
        }
        let answer = r#"[{"class":"name","excerpt":"Rita","confidence":0.4}]"#;
        assert_eq!(
            parse_observations(answer, source).unwrap()[0].confidence,
            Some(0.4)
        );
    }

    /// An unreadable answer must be distinguishable from "found nothing". Recording it as clean
    /// would be permanent, and would inflate the denominator with summaries nobody could read.
    #[test]
    fn an_answer_that_is_not_a_json_array_is_unreadable_rather_than_clean() {
        for answer in ["", "sorry, I cannot", "{}", "[{\"class\":\"name\"}]"] {
            assert_eq!(parse_observations(answer, "Rita"), None, "{answer:?}");
        }
        // The contrast: an empty array IS a reading, and it is the one that counts as clean.
        assert_eq!(parse_observations("[]", "Rita"), Some(Vec::new()));
    }

    #[test]
    fn empty_excerpts_and_duplicates_are_dropped() {
        let source = "Rita and Rita";
        let answer = r#"[
            {"class":"name","excerpt":"Rita"},
            {"class":"name","excerpt":"Rita"},
            {"class":"name","excerpt":"  "}
        ]"#;
        assert_eq!(parse_observations(answer, source).unwrap().len(), 1);
    }

    #[tokio::test]
    async fn re_observing_the_same_source_does_not_double_the_count() {
        let pool = test_pool().await;
        let observations = vec![Observation {
            class: "name".to_string(),
            excerpt: "Rita".to_string(),
            confidence: Some(0.8),
        }];

        let first = record(&pool, "emails", 1, "subject", &observations, "2026-08-09T00:00:00Z")
            .await
            .unwrap();
        let second = record(&pool, "emails", 1, "subject", &observations, "2026-08-09T01:00:00Z")
            .await
            .unwrap();

        assert_eq!((first, second), (1, 0));
        assert_eq!(tally(&pool).await.unwrap(), vec![("name".to_string(), 1)]);
    }

    /// Stands up a stub Ollama that answers the same body every time, and returns its base URL.
    async fn stub_ollama(answer: &'static str) -> String {
        let app = axum::Router::new().fallback(axum::routing::post(move || async move {
            axum::Json(serde_json::json!({
                "message": {"role": "assistant", "content": answer}
            }))
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        format!("http://{address}")
    }

    async fn triaged_email(pool: &sqlx::SqlitePool, id: i64, summary: &str) {
        sqlx::query(
            "INSERT INTO emails (id, message_id, mailbox, uidvalidity, uid, from_addr, subject,
                                 received_at, ingested_at, direction, triage_class, triage_summary)
             VALUES (?, ?, 'INBOX', 1, ?, 'a@b', 'Assunto', '2026-08-09T00:00:00Z',
                     '2026-08-09T00:00:00Z', 'inbound', 'info', ?)",
        )
        .bind(id)
        .bind(format!("<{id}@b>"))
        .bind(id)
        .bind(summary)
        .execute(pool)
        .await
        .unwrap();
    }

    /// The sweep records a clean reading, so the same summary is not reconsidered for ever.
    #[tokio::test]
    async fn a_summary_the_model_read_as_clean_is_recorded_and_not_revisited() {
        let pool = test_pool().await;
        triaged_email(&pool, 1, "pedido de orçamento").await;
        let base = stub_ollama("[]").await;
        let client = reqwest::Client::new();

        let first = observe_pending(&pool, &client, &base, "m", "2026-08-09T00:00:00Z")
            .await
            .unwrap();
        let second = observe_pending(&pool, &client, &base, "m", "2026-08-09T01:00:00Z")
            .await
            .unwrap();

        assert_eq!((first, second), (1, 0), "a clean reading is written once");
        assert_eq!(tally(&pool).await.unwrap(), vec![("none".to_string(), 1)]);
    }

    /// A garbled answer is its own outcome, neither a finding nor a clean reading.
    ///
    /// Recording it as `none` would permanently count an unread summary as clean. Recording nothing
    /// at all — the first attempt at this fix — deadlocks the sweep instead: the query takes the
    /// twenty newest unobserved summaries, so twenty the model garbles deterministically are twenty
    /// it garbles again every fifteen minutes, and everything older is never reached.
    #[tokio::test]
    async fn a_summary_the_model_garbled_is_recorded_as_unreadable_not_as_clean() {
        let pool = test_pool().await;
        triaged_email(&pool, 1, "pedido de orçamento").await;
        let client = reqwest::Client::new();

        let garbled = stub_ollama("I'm sorry, I cannot help with that.").await;
        let written = observe_pending(&pool, &client, &garbled, "m", "2026-08-09T00:00:00Z")
            .await
            .unwrap();

        assert_eq!(written, 1);
        assert_eq!(
            tally(&pool).await.unwrap(),
            vec![("unreadable".to_string(), 1)],
            "an unread summary must not be counted as clean"
        );

        // And the sweep moves on rather than reconsidering it for ever.
        let again = observe_pending(&pool, &client, &garbled, "m", "2026-08-09T01:00:00Z")
            .await
            .unwrap();
        assert_eq!(again, 0, "the sweep is stuck on a summary it cannot read");
    }

    /// The sweep must reach older summaries even when the newest ones cannot be read. Without a
    /// mark on the unreadable ones, a full batch of them starves everything behind.
    #[tokio::test]
    async fn unreadable_summaries_do_not_starve_the_ones_behind_them() {
        let pool = test_pool().await;
        triaged_email(&pool, 1, "o mais antigo").await;
        triaged_email(&pool, 2, "o mais recente").await;
        let client = reqwest::Client::new();

        let garbled = stub_ollama("not json").await;
        observe_pending(&pool, &client, &garbled, "m", "2026-08-09T00:00:00Z")
            .await
            .unwrap();

        let counts = tally(&pool).await.unwrap();
        assert_eq!(
            counts,
            vec![("unreadable".to_string(), 2)],
            "both summaries must have been looked at in one sweep"
        );
    }

    /// The table must not outlive what it describes. Without the trigger, deleting a message would
    /// leave an excerpt of its subject behind for ever.
    #[tokio::test]
    async fn observations_go_when_their_message_goes() {
        let pool = test_pool().await;
        sqlx::query(
            "INSERT INTO emails (id, message_id, mailbox, uidvalidity, uid, from_addr, subject,
                                 received_at, ingested_at, direction)
             VALUES (7, '<a@b>', 'INBOX', 1, 1, 'a@b', 'Fatura', '2026-08-09T00:00:00Z',
                     '2026-08-09T00:00:00Z', 'inbound')",
        )
        .execute(&pool)
        .await
        .unwrap();
        record(
            &pool,
            "emails",
            7,
            "subject",
            &[Observation {
                class: "name".to_string(),
                excerpt: "Rita".to_string(),
                confidence: None,
            }],
            "2026-08-09T00:00:00Z",
        )
        .await
        .unwrap();

        sqlx::query("DELETE FROM emails WHERE id = 7")
            .execute(&pool)
            .await
            .unwrap();

        assert!(tally(&pool).await.unwrap().is_empty());
    }

    /// The database refuses a column this pass is not allowed to read, and `body_text` is the one
    /// that matters: triage nulls it, so an excerpt of it would outlive the body.
    #[tokio::test]
    async fn the_database_refuses_a_column_that_is_not_retained() {
        let pool = test_pool().await;
        let refused = sqlx::query(
            "INSERT INTO pii_observations
                 (source_table, source_id, source_column, class, excerpt, observed_at)
             VALUES ('emails', 1, 'body_text', 'name', 'Rita', '2026-08-09T00:00:00Z')",
        )
        .execute(&pool)
        .await;

        assert!(refused.is_err(), "body_text must never be observable");
    }

    /// The Rust list and the SQL constraint are two spellings of one rule.
    #[tokio::test]
    async fn column_list_matches_the_migration() {
        let pool = test_pool().await;
        for column in OBSERVABLE_COLUMNS {
            let accepted = sqlx::query(
                "INSERT INTO pii_observations
                     (source_table, source_id, source_column, class, excerpt, observed_at)
                 VALUES ('emails', 1, ?, 'name', 'Rita', '2026-08-09T00:00:00Z')",
            )
            .bind(column)
            .execute(&pool)
            .await;
            assert!(accepted.is_ok(), "{column} is allowed in Rust and not in SQL");
        }
    }

    /// A subject is third-party text, so its length is chosen by whoever sent the mail. Counting a
    /// character the instruction itself never uses is what keeps this measuring the field rather
    /// than the prompt around it.
    #[test]
    fn the_prompt_bounds_a_field_that_arrived_from_outside() {
        let long = "q".repeat(FIELD_CAP * 2);
        let prompt = prompt_for("subject", &long);
        assert_eq!(prompt.matches('q').count(), FIELD_CAP);
    }
}
