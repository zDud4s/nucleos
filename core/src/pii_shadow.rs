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
//! It is best-effort throughout. Nothing here blocks a verdict or fails a triage run. The one thing
//! it does retry is an answer it could not read, up to `MAX_UNREADABLE_ATTEMPTS`, because that is a
//! gap in the denominator rather than a finding — everything else is left as it fell. An
//! observation that did not happen is a gap in a sample; a triage that did not happen is mail
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

/// The sampling grammar handed to Ollama, forcing the answer to BE the array this parses.
///
/// A schema, not `"format": "json"`, and the difference is not cosmetic: `"json"` constrains the
/// answer to a JSON *object*, so a model asked for a list of findings returns the first one alone —
/// `{"class":"financial","excerpt":"IBAN",...}`. `parse_observations` reads that as unreadable, so
/// against a real model every summary would have been recorded `unreadable` and the table would
/// have measured nothing at all. Verified against qwen3.5:4b, which returns a bare object under
/// `"json"` and a correct array under this.
fn answer_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "array",
        "items": {
            "type": "object",
            "properties": {
                "class": {"type": "string"},
                "excerpt": {"type": "string"},
                "confidence": {"type": "number"}
            },
            "required": ["class", "excerpt"]
        }
    })
}

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
            confidence: entry.confidence.filter(|value| (0.0..=1.0).contains(value)),
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
    record_at_attempt(
        pool,
        source_table,
        source_id,
        source_column,
        observations,
        0,
        observed_at,
    )
    .await
}

/// The same write, at a numbered attempt.
///
/// Private, and the reason is the unique index. `attempt` is part of it, so a caller that passes a
/// non-zero one for a real observation quietly defeats the de-duplication that `record` exists to
/// provide — the same finding would be recorded again under a different number every time triage
/// re-ran. Only the retry of an unreadable answer has any business numbering a row, so only it can.
async fn record_at_attempt(
    pool: &sqlx::SqlitePool,
    source_table: &str,
    source_id: i64,
    source_column: &str,
    observations: &[Observation],
    attempt: i64,
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
                 (source_table, source_id, source_column, class, excerpt, confidence, attempt,
                  observed_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(source_table)
        .bind(source_id)
        .bind(source_column)
        .bind(&observation.class)
        .bind(&observation.excerpt)
        .bind(observation.confidence)
        .bind(attempt)
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

/// How many times a summary the model garbled is offered to it again.
///
/// Three, because the two failure modes are on either side of this number. Zero retries means one
/// truncated answer removes a summary from the denominator for ever. Unlimited retries means a
/// summary the model garbles deterministically — a prompt it always refuses, say — is reconsidered
/// every fifteen minutes and blocks the batch from ever reaching older mail.
const MAX_UNREADABLE_ATTEMPTS: i64 = 3;

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
    // Two conditions, not one. A summary that was READ is done, whatever was found. A summary the
    // model garbled is retried, up to a cap — because retrying for ever lets a deterministically
    // unreadable summary block the sweep from anything older, and not retrying at all lets one
    // truncated answer exclude it from the denominator permanently.
    //
    // The next attempt number comes back with it, as `MAX(attempt) + 1` and not as a count. They
    // agree while the rows are contiguous, and only one of them survives a row going missing: a
    // count would then re-use a number that is already taken, `INSERT OR IGNORE` would swallow the
    // write, and the summary would be re-sent to the model every fifteen minutes for ever — the
    // starvation the cap exists to prevent, reintroduced by the arithmetic meant to enforce it.
    let pending: Vec<(i64, String, i64)> = sqlx::query_as(
        "SELECT e.id,
                e.triage_summary,
                (SELECT COALESCE(MAX(o.attempt) + 1, 0) FROM pii_observations o
                  WHERE o.source_table = 'emails'
                    AND o.source_id = e.id
                    AND o.source_column = 'triage_summary'
                    AND o.class = 'unreadable') AS attempts
           FROM emails e
          WHERE e.triage_summary IS NOT NULL
            AND e.triage_summary <> ''
            AND NOT EXISTS (SELECT 1 FROM pii_observations o
                             WHERE o.source_table = 'emails'
                               AND o.source_id = e.id
                               AND o.source_column = 'triage_summary'
                               AND o.class <> 'unreadable')
            AND attempts < ?
          ORDER BY e.id DESC
          LIMIT ?",
    )
    .bind(MAX_UNREADABLE_ATTEMPTS)
    .bind(SWEEP_BATCH)
    .fetch_all(pool)
    .await?;

    let mut written = 0;
    for (id, summary, attempts) in pending {
        let answer = crate::runner::ollama_chat(
            client,
            base_url,
            model,
            &prompt_for("triage_summary", &summary),
            serde_json::json!({"num_ctx": crate::triage::LOCAL_NUM_CTX}),
            Some(answer_schema()),
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
                tracing::warn!(
                    email_id = id,
                    attempt = attempts + 1,
                    "pii shadow: answer could not be read"
                );
                written += record_at_attempt(
                    pool,
                    "emails",
                    id,
                    "triage_summary",
                    &[Observation {
                        class: "unreadable".to_string(),
                        excerpt: String::new(),
                        confidence: None,
                    }],
                    attempts,
                    now,
                )
                .await?;
                continue;
            }
        };

        // The failed attempts go, now that the summary has been read. Leaving them made `tally`
        // count attempts where it is read as counting summaries: one summary garbled twice and then
        // read clean appeared as two `unreadable` and one `none`, so the denominator a decision is
        // divided by was inflated by the sweep's own flakiness, and `unreadable: 30` could mean
        // thirty summaries nobody read or ten that eventually were.
        sqlx::query(
            "DELETE FROM pii_observations
              WHERE source_table = 'emails' AND source_id = ? AND source_column = 'triage_summary'
                AND class = 'unreadable'",
        )
        .bind(id)
        .execute(pool)
        .await?;

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

        let first = record(
            &pool,
            "emails",
            1,
            "subject",
            &observations,
            "2026-08-09T00:00:00Z",
        )
        .await
        .unwrap();
        let second = record(
            &pool,
            "emails",
            1,
            "subject",
            &observations,
            "2026-08-09T01:00:00Z",
        )
        .await
        .unwrap();

        assert_eq!((first, second), (1, 0));
        assert_eq!(tally(&pool).await.unwrap(), vec![("name".to_string(), 1)]);
    }

    type SeenBodies = std::sync::Arc<std::sync::Mutex<Vec<serde_json::Value>>>;

    /// Stands up a stub Ollama that answers the same body every time, and records what it was sent.
    async fn stub_ollama_capturing(answer: &'static str) -> (String, SeenBodies) {
        let seen: SeenBodies = SeenBodies::default();
        let recorder = seen.clone();
        let app = axum::Router::new().fallback(axum::routing::post(
            move |axum::Json(body): axum::Json<serde_json::Value>| {
                let recorder = recorder.clone();
                async move {
                    recorder.lock().unwrap().push(body);
                    axum::Json(serde_json::json!({
                        "message": {"role": "assistant", "content": answer}
                    }))
                }
            },
        ));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (format!("http://{address}"), seen)
    }

    async fn stub_ollama(answer: &'static str) -> String {
        stub_ollama_capturing(answer).await.0
    }

    /// The wire format, read from the wire. This is the assertion that would have caught the bug a
    /// real model found: `"format": "json"` constrains Ollama to a JSON OBJECT, so a model asked
    /// for a list returned the first finding alone, `parse_observations` read it as unreadable, and
    /// every summary would have been recorded `unreadable` for ever. Asserting on the OUTCOME could
    /// not catch it, because a stub answers whatever it was told to.
    #[tokio::test]
    async fn the_request_asks_for_an_array_and_not_merely_for_json() {
        let pool = test_pool().await;
        triaged_email(&pool, 1, "resumo").await;
        let (base, seen) = stub_ollama_capturing("[]").await;

        observe_pending(
            &pool,
            &reqwest::Client::new(),
            &base,
            "m",
            "2026-08-09T00:00:00Z",
        )
        .await
        .unwrap();

        let body = &seen.lock().unwrap()[0];
        assert_eq!(
            body["format"]["type"], "array",
            "the grammar must force an array; {body}"
        );
        assert_eq!(body["format"]["items"]["required"][0], "class");
        // Stated explicitly for the reason `voice.rs` records: Ollama truncates silently against
        // its own default window.
        assert_eq!(
            body["options"]["num_ctx"],
            crate::triage::LOCAL_NUM_CTX as i64
        );
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

        // Retried, because one truncated answer must not remove a summary from the denominator for
        // ever — but only up to the cap, because a summary the model garbles every time would
        // otherwise block the batch from reaching anything older.
        for sweep in 1..MAX_UNREADABLE_ATTEMPTS {
            let again = observe_pending(&pool, &client, &garbled, "m", "2026-08-09T01:00:00Z")
                .await
                .unwrap();
            assert_eq!(
                again, 1,
                "sweep {sweep} did not retry an unreadable summary"
            );
        }

        let past_the_cap = observe_pending(&pool, &client, &garbled, "m", "2026-08-09T09:00:00Z")
            .await
            .unwrap();
        assert_eq!(past_the_cap, 0, "the sweep retries past its own cap");
        assert_eq!(
            tally(&pool).await.unwrap(),
            vec![("unreadable".to_string(), MAX_UNREADABLE_ATTEMPTS)]
        );
    }

    /// A missing attempt number must not restart the counter. Deriving the next attempt from
    /// `COUNT(*)` re-uses a number a surviving row already holds, `INSERT OR IGNORE` swallows the
    /// write, the count never grows, and the summary sits at the head of the batch being re-sent to
    /// the model for ever — the starvation the cap exists to prevent, caused by the arithmetic
    /// meant to enforce it. Nothing deletes a single row today, which is exactly why this is
    /// pinned: the invariant is invisible until something does.
    #[tokio::test]
    async fn a_gap_in_the_attempt_numbers_does_not_restart_the_count() {
        let pool = test_pool().await;
        triaged_email(&pool, 1, "resumo").await;

        for attempt in [0, 2] {
            sqlx::query(
                "INSERT INTO pii_observations
                     (source_table, source_id, source_column, class, excerpt, attempt, observed_at)
                 VALUES ('emails', 1, 'triage_summary', 'unreadable', '', ?,
                         '2026-08-09T00:00:00Z')",
            )
            .bind(attempt)
            .execute(&pool)
            .await
            .unwrap();
        }

        let working = stub_ollama("[]").await;
        let written = observe_pending(
            &pool,
            &reqwest::Client::new(),
            &working,
            "m",
            "2026-08-09T01:00:00Z",
        )
        .await
        .unwrap();

        assert_eq!(
            written, 0,
            "three attempts were made and the summary was offered a fourth"
        );
    }

    /// A summary that failed once and then succeeded is counted for what it is, not left as a
    /// permanent `unreadable` — which is the whole point of retrying at all.
    #[tokio::test]
    async fn a_summary_that_reads_on_a_later_sweep_is_counted_properly() {
        let pool = test_pool().await;
        triaged_email(&pool, 1, "resumo").await;
        let client = reqwest::Client::new();

        let garbled = stub_ollama("not json").await;
        observe_pending(&pool, &client, &garbled, "m", "2026-08-09T00:00:00Z")
            .await
            .unwrap();

        let working = stub_ollama("[]").await;
        observe_pending(&pool, &client, &working, "m", "2026-08-09T01:00:00Z")
            .await
            .unwrap();

        // One summary, one row. Keeping the failed attempt alongside the reading made `tally` count
        // attempts while every reader of it — `get_pii_observations` calls `none` the denominator —
        // counts summaries, so the total a class would be divided by grew with the sweep's own
        // flakiness rather than with the mailbox.
        assert_eq!(
            tally(&pool).await.unwrap(),
            vec![("none".to_string(), 1)],
            "a summary that eventually read is still counted as unread"
        );

        // And it is finished: a summary that was read is not offered again, whatever was found.
        let after = observe_pending(&pool, &client, &working, "m", "2026-08-09T02:00:00Z")
            .await
            .unwrap();
        assert_eq!(after, 0);
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
            assert!(
                accepted.is_ok(),
                "{column} is allowed in Rust and not in SQL"
            );
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
