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

/// Columns this pass is allowed to read, matching the CHECK constraint in migration 0059.
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

/// How many fields one sweep looks at, across every column together.
///
/// Bounded because each one is a model call: an unbounded sweep on a mailbox that has just been
/// backfilled would occupy the local model for hours, and the local model is also what answers
/// triage and, when configured, the chat.
///
/// Six, not twenty, and the number comes from a measurement rather than a guess. A reasoning pass
/// over one field takes about forty seconds here, so twenty of them is thirteen minutes out of
/// every fifteen — the earlier comment claimed that fitted "comfortably inside its own interval",
/// which was arithmetic nobody had done. Six is four minutes, leaving the model free for the
/// things that are actually waiting on it. The mailbox is caught up more slowly and that costs
/// nothing: this is a measurement, and the only thing a slower one delays is a decision that is
/// months away.
///
/// Migration `0060` still says twenty, and has to. Its comment is what an operator reads to decide
/// whether deleting the table was safe, so the wrong number there is worth correcting — but the
/// file had already been applied, and `sqlx` checksums applied migrations and refuses to start
/// against one whose bytes have changed. Editing the prose cost a `VersionMismatch` panic at the
/// next launch, which is a strictly worse outcome than a stale sentence. An applied migration is
/// immutable including its comments; this is the place the number is allowed to be right.
const SWEEP_BATCH: i64 = 6;

/// Whether the model reasons before answering, and it is the difference between measuring and not.
///
/// Shipped as `false`, copied from triage where it is there for speed, and the result was 59
/// summaries in a row recorded as `none` — including "Rafael responde às tuas dúvidas sobre PADLE
/// Leitura", where the name is the first word. Asked the identical prompt with the identical
/// schema, qwen3.5:4b answers `[]` without thinking and
/// `[{"class":"name","excerpt":"Rafael","confidence":1.0}]` with it.
///
/// The two settings are not the same trade in the two places. Triage is on the path of mail a
/// person is waiting for, and a verdict that is a little worse but arrives is the right call. This
/// is a background sweep nobody waits for, where a fast answer that finds nothing is not cheaper
/// than a slow one — it is worthless, and worse than worthless, because it reads as evidence there
/// was nothing to find.
const THINK: bool = true;

/// How many times a summary the model garbled is offered to it again.
///
/// Three, because the two failure modes are on either side of this number. Zero retries means one
/// truncated answer removes a summary from the denominator for ever. Unlimited retries means a
/// summary the model garbles deterministically — a prompt it always refuses, say — is reconsidered
/// every fifteen minutes and blocks the batch from ever reaching older mail.
const MAX_UNREADABLE_ATTEMPTS: i64 = 3;

/// How many timeouts in a row mean the endpoint is gone rather than the field being slow.
///
/// Treating a timeout as a fact about the field was half right, and the wrong half is what a hung
/// endpoint does to this table. `SWEEP_REQUEST_TIMEOUT` exists to catch a generation that stalled;
/// an Ollama that is blackholed rather than refusing stalls on EVERY field, and each one then got an
/// `unreadable` attempt recorded without the model having seen it. Three sweeps of that and a cohort
/// of fields is past `MAX_UNREADABLE_ATTEMPTS` and excluded for ever — rows that only a successful
/// read deletes, and the successful read can never come. The measurement would have been silently,
/// permanently wrong in the one table whose entire purpose is measurement.
///
/// So a timeout is held rather than written, and only becomes an attempt once some later field in
/// the same sweep comes back — which is the endpoint proving it is alive. Two in a row without that
/// proof end the sweep with nothing recorded, exactly as a refused connection does.
///
/// Two, not three, and the asymmetry with `MAX_UNREADABLE_ATTEMPTS` is deliberate: being wrong here
/// costs one sweep that is retried in fifteen minutes, and being wrong the other way costs data
/// that no later pass repairs.
const MAX_CONSECUTIVE_TIMEOUTS: usize = 2;

/// How often a sweep runs. Slow on purpose — this is measurement, and nothing waits for it.
const SWEEP_INTERVAL: std::time::Duration = std::time::Duration::from_secs(900);

/// How long one whole sweep may run before it stops and waits for the next tick.
///
/// The batch count alone bounds nothing: six fields at the per-request timeout is eighteen minutes
/// against a fifteen-minute period, because the batch was sized from the forty seconds a call
/// typically takes rather than from the ceiling the code actually enforces. Two thirds of the
/// period leaves the local model free for triage and the chat even when every field is slow, and
/// what does not get looked at simply waits — this is a measurement, and nothing is behind it.
const SWEEP_DEADLINE: std::time::Duration = std::time::Duration::from_secs(600);

/// How long one field may occupy the model before the sweep gives up on it.
///
/// This is a hang detector, not a budget, and the first version confused the two. It was set to 40
/// seconds in the same commit that turned reasoning on, and a reasoning pass over a single subject
/// line on this machine measures 36 to 43 seconds — so roughly half of them expired, and since a
/// failed request stops the whole sweep, the sweep simply never completed. A timeout added to stop
/// the table silently not filling was what stopped it filling.
///
/// Three minutes is far above anything observed and still finite, which is all it needs to be: the
/// thing it exists to catch is a generation that has stalled, not one that is slow.
const SWEEP_REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(180);

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
    // One budget across every column, because the cost is a model call and the interval is what it
    // has to fit inside — but SHARED OUT, not handed to the first column that asks.
    //
    // Serving them in order was the first version and it starves the very column this table was
    // built for. `subject` has a row for every message ever ingested, so on a backfilled mailbox it
    // takes the whole budget every sweep: the first live run of it spent the whole batch on subjects
    // and reached neither the sender names nor the summaries. At two thousand messages that is weeks
    // before the second column is touched. An equal share bounds it; the remainder goes to whoever
    // still has work, so a caught-up column costs nothing.
    //
    // Bounded in TIME as well as in count, because the count alone does not bound anything. Six
    // fields at the per-request timeout is eighteen minutes against a fifteen-minute period — the
    // batch was sized from the forty seconds a call measures, which is the typical case and not the
    // one the code enforces. `MissedTickBehavior::Delay` stops a burst of missed ticks; only a
    // deadline stops one sweep from running into the next.
    let deadline = tokio::time::Instant::now() + SWEEP_DEADLINE;
    let local = LocalModel {
        client,
        base_url,
        model,
    };
    let mut written = 0;
    let share = (SWEEP_BATCH / OBSERVABLE_COLUMNS.len() as i64).max(1);
    let mut spare = SWEEP_BATCH - share * OBSERVABLE_COLUMNS.len() as i64;
    for column in OBSERVABLE_COLUMNS {
        if tokio::time::Instant::now() >= deadline {
            tracing::info!(
                written,
                "pii shadow: sweep hit its deadline; the remaining columns wait for the next one"
            );
            break;
        }
        let (rows, spent, model_is_out) =
            observe_column(pool, &local, column, share + spare, deadline, now).await?;
        written += rows;
        if model_is_out {
            break;
        }
        spare = (share + spare - spent).max(0);
    }
    Ok(written)
}

/// Where the local model is and which one it is — the three things every request needs and none of
/// them a decision. Bundled because they always travel together and separating them is what pushed
/// `observe_column` past the point where its signature said anything.
struct LocalModel<'a> {
    client: &'a reqwest::Client,
    base_url: &'a str,
    model: &'a str,
}

/// One column's share of a sweep. Returns what it wrote and how much of the budget it spent.
///
/// Split out when the sweep stopped being about one column. The column name is interpolated into
/// the SQL rather than bound, which is safe for exactly one reason and it is worth naming: it comes
/// from `OBSERVABLE_COLUMNS`, a const list in this file, and never from anything a caller chose.
/// The check at the top of the body is what keeps that true if a caller ever appears — a real one,
/// not a `debug_assert`, because a guard that compiles out of release is not a guard.
async fn observe_column(
    pool: &sqlx::SqlitePool,
    local: &LocalModel<'_>,
    column: &str,
    budget: i64,
    deadline: tokio::time::Instant,
    now: &str,
) -> sqlx::Result<(u64, i64, bool)> {
    // A real check, not a `debug_assert`. This one guards a column name interpolated into SQL, and
    // `debug_assert` compiles to nothing in release — so the audit `AssertSqlSafe` below rests on
    // would have been absent from the only build that matters.
    if !OBSERVABLE_COLUMNS.contains(&column) {
        tracing::error!(
            column,
            "pii shadow: refusing to sweep a column this pass may not read"
        );
        return Ok((0, 0, false));
    }

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
    // `AssertSqlSafe` is the audit sqlx demands for a query string it cannot see is constant, and
    // the audit is the check above plus this: `column` is an element of
    // `OBSERVABLE_COLUMNS`, a const list of three identifiers in this file. It is never a caller's
    // string, never a row's contents, and never anything that crossed the network. Everything that
    // varies with data — the column NAME as a value, the cap, the budget — is bound below.
    let pending: Vec<(i64, String, i64)> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT e.id,
                e.{column},
                (SELECT COALESCE(MAX(o.attempt) + 1, 0) FROM pii_observations o
                  WHERE o.source_table = 'emails'
                    AND o.source_id = e.id
                    AND o.source_column = ?
                    AND o.class = 'unreadable') AS attempts
           FROM emails e
          WHERE e.{column} IS NOT NULL
            AND e.{column} <> ''
            AND NOT EXISTS (SELECT 1 FROM pii_observations o
                             WHERE o.source_table = 'emails'
                               AND o.source_id = e.id
                               AND o.source_column = ?
                               AND o.class <> 'unreadable')
            AND attempts < ?
          ORDER BY e.id DESC
          LIMIT ?"
    )))
    .bind(column)
    .bind(column)
    .bind(MAX_UNREADABLE_ATTEMPTS)
    .bind(budget)
    .fetch_all(pool)
    .await?;

    let mut written = 0;
    let mut spent = 0;
    // Timed-out fields waiting to learn whether the endpoint was alive. Emptied by the first field
    // that comes back, which is what turns them into honest attempts; see `MAX_CONSECUTIVE_TIMEOUTS`.
    // Its length IS the consecutive count, because a success drains it.
    let mut deferred_timeouts: Vec<(i64, i64)> = Vec::new();
    for (id, summary, attempts) in pending {
        // Checked per field, not only per column: one column's share can outlast the whole period
        // on its own if the model is slow.
        if tokio::time::Instant::now() >= deadline {
            break;
        }
        spent += 1;
        let answer = crate::runner::ollama_chat(
            local.client,
            local.base_url,
            local.model,
            &prompt_for(column, &summary),
            serde_json::json!({"num_ctx": crate::triage::LOCAL_NUM_CTX}),
            Some(answer_schema()),
            THINK,
        )
        .await;

        let observations = match answer {
            Ok(answer) => parse_observations(&answer, &summary),

            // A timeout is about THIS field and not about the endpoint, and telling the two apart
            // is what stops one field from stopping everything. A subject the model chews on past
            // the deadline used to abort the sweep with nothing recorded — and since the query is
            // `ORDER BY id DESC` and `subject` is swept first, that same field came back first on
            // the next sweep and aborted that one too, for ever, with the other two columns never
            // reached. Recorded as an attempt instead, so the same cap that governs an answer
            // nobody could parse moves this one along as well.
            Err(error) if error.kind() == std::io::ErrorKind::TimedOut => {
                tracing::warn!(
                    email_id = id,
                    column,
                    attempt = attempts + 1,
                    "pii shadow: the model did not answer in time for this field"
                );
                deferred_timeouts.push((id, attempts));
                if deferred_timeouts.len() >= MAX_CONSECUTIVE_TIMEOUTS {
                    tracing::warn!(
                        column,
                        timeouts = deferred_timeouts.len(),
                        "pii shadow: nothing has come back at all, so this is the endpoint and not \
                         the fields — ending the sweep without recording them"
                    );
                    return Ok((written, spent, true));
                }
                continue;
            }

            Err(error) => {
                // Named rather than guessed. "Unreachable" was the only diagnosis this branch could
                // give, and the likeliest cause of a permanent failure here is not a stopped Ollama
                // — it is `THINK` being sent to a model that has no thinking mode, which the
                // endpoint refuses. `ollama_message` carries the response body out for this: it
                // used to call `error_for_status`, whose message is only the status line, so this
                // branch could never fire and a configuration mistake was logged as a network one.
                let text = error.to_string();
                if text.contains("think") || text.contains("Think") {
                    tracing::error!(
                        %error,
                        model = local.model,
                        "pii shadow: this model has no thinking mode, and without one it finds \
                         nothing — name a model that reasons, or this pass cannot measure anything"
                    );
                } else {
                    tracing::warn!(%error, "pii shadow: local model unreachable, skipping this sweep");
                }
                // Out of the whole sweep, not just this column. "One warning and out" was written
                // when a sweep was one column; splitting it into three turned a stopped Ollama into
                // three connection attempts and three identical lines every fifteen minutes.
                return Ok((written, spent, true));
            }
        };

        // The endpoint answered, so it was alive, so the fields that timed out before it really were
        // slow fields. Only now do they become attempts — a garbled answer counts as proof of life
        // just as a good one does, because the question this settles is whether anything came back.
        for (timed_out, timed_out_attempts) in deferred_timeouts.drain(..) {
            written += record_at_attempt(
                pool,
                "emails",
                timed_out,
                column,
                &[Observation {
                    class: "unreadable".to_string(),
                    excerpt: String::new(),
                    confidence: None,
                }],
                timed_out_attempts,
                now,
            )
            .await?;
        }

        // An answer nobody could read is recorded as `unreadable`, which is neither a finding nor a
        // clean reading — it is its own outcome and its own measurement.
        //
        // Leaving it unrecorded to be retried was the first attempt and it deadlocks the sweep:
        // the query takes the newest unobserved fields in batch order, so a batch the model garbles
        // deterministically is one it garbles again every fifteen minutes, and everything older is
        // never reached. A frozen denominator is worse than an honest gap in it.
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
                    column,
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
              WHERE source_table = 'emails' AND source_id = ? AND source_column = ?
                AND class = 'unreadable'",
        )
        .bind(id)
        .bind(column)
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
        written += record(pool, "emails", id, column, &to_write, now).await?;
    }

    // Anything still deferred is dropped rather than written, and that is the conservative end of
    // the trade. A column whose last field timed out ends without proof the endpoint was alive, so
    // the attempt is not recorded and that field is simply offered again next sweep. The cost is one
    // retry; the cost of guessing the other way is a permanent hole in the denominator.
    Ok((written, spent, false))
}

/// Runs a sweep every `SWEEP_INTERVAL` for as long as the daemon is up.
pub async fn run_sweep_loop(pool: sqlx::SqlitePool, base_url: String, model: String) {
    // A client with a timeout, not the default one. `assistants::ConfiguredAssistants::new` records
    // why at its `local_client`, in one line — "`reqwest::Client::new()` waits for ever" — and
    // reasoning mode is what makes it bite: a 4B can stall mid-trace, and a request that never
    // returns is a sweep loop that never ticks again, filling nothing and warning about nothing.
    // `expect`, not a fallback, for the reason the same comment sets out at that exact line: the
    // fallback here was `Client::new()`, which has no timeout — so the downgrade path
    // silently produced the very thing the timeout exists to prevent, and nothing logged it. The
    // causes of a builder failure are TLS backend and proxy misconfiguration, which are startup
    // problems and should look like one.
    let client = reqwest::Client::builder()
        .timeout(SWEEP_REQUEST_TIMEOUT)
        .build()
        .expect("HTTP client for the shadow sweep (check TLS and proxy environment)");
    let mut ticker = tokio::time::interval(SWEEP_INTERVAL);
    // A sweep that overruns its period must not be followed by a burst of the ticks it missed,
    // which is `interval`'s default. Twenty reasoning calls can outlast fifteen minutes on a cold
    // or contended model, and bursting would then run sweeps back to back — occupying the local
    // model continuously with triage and the chat queued behind a measurement nobody waits for.
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
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

/// What the observation window has seen, by column and class. The report the table exists to
/// produce.
///
/// Broken down by column, not summed across them, and that is the difference between a measurement
/// and a number. `from_name` is a person's name often enough that a `name` finding there is nearly
/// structural; `subject` and `triage_summary` are where the question "is there personal data here?"
/// is a real one. Summed together, the answer to "would redacting class `name` have cost anything?"
/// is dominated by the column where the answer is trivially yes, and the `none` denominator mixes
/// three populations with completely different base rates.
pub async fn tally(pool: &sqlx::SqlitePool) -> sqlx::Result<Vec<(String, String, i64)>> {
    sqlx::query_as(
        "SELECT source_column, class, COUNT(*) FROM pii_observations
          GROUP BY source_column, class
          ORDER BY source_column, 3 DESC",
    )
    .fetch_all(pool)
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn test_pool() -> sqlx::SqlitePool {
        crate::testdb::fresh_shared_pool().await
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
        assert_eq!(
            tally(&pool).await.unwrap(),
            vec![("subject".to_string(), "name".to_string(), 1)]
        );
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
        // The flag that decides whether this pass can see anything at all. With it false the same
        // model, prompt and grammar answered `[]` to a sentence beginning with a person's name, and
        // 59 fields in a row were recorded clean — a wrong value here costs a migration to undo and
        // is invisible until somebody reads the table months later. Nothing else in the suite would
        // notice it flipping back.
        assert_eq!(
            body["think"], true,
            "the sweep stopped asking the model to reason; {body}"
        );
    }

    /// A message with exactly ONE observable field filled, so a test about the retry mechanics
    /// counts one thing per message.
    ///
    /// `subject` and `from_name` are left NULL deliberately: the sweep reads all three columns, so
    /// a fixture that filled them would make every count below a multiple of three and every
    /// assertion about "was it written once" a statement about column coverage instead. Coverage
    /// has its own test.
    async fn triaged_email(pool: &sqlx::SqlitePool, id: i64, summary: &str) {
        sqlx::query(
            "INSERT INTO emails (id, message_id, mailbox, uidvalidity, uid, from_addr,
                                 received_at, ingested_at, direction, triage_class, triage_summary)
             VALUES (?, ?, 'INBOX', 1, ?, 'a@b', '2026-08-09T00:00:00Z',
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

    /// All three observable columns are swept, not just the summary.
    ///
    /// The regression this exists for is not a crash. The sweep read `triage_summary` alone, on the
    /// reasoning that it was the retained field where a stranger's data plausibly survives — and in
    /// production that column holds triage VERDICTS, "noise: mailing list (List-Unsubscribe)", 37
    /// characters on average. Fifty-nine of them were recorded `none`, correctly and uselessly,
    /// while the sender's name and the subject line sat unread. A measurement pointed at the one
    /// column with nothing in it reads exactly like a measurement that found nothing.
    #[tokio::test]
    async fn every_observable_column_is_swept() {
        let pool = test_pool().await;
        sqlx::query(
            "INSERT INTO emails (id, message_id, mailbox, uidvalidity, uid, from_addr, from_name,
                                 subject, received_at, ingested_at, direction, triage_class,
                                 triage_summary)
             VALUES (1, '<a@b>', 'INBOX', 1, 1, 'a@b', 'Rita Melo', 'Consulta de quinta',
                     '2026-08-09T00:00:00Z', '2026-08-09T00:00:00Z', 'inbound', 'info', 'resumo')",
        )
        .execute(&pool)
        .await
        .unwrap();

        let stub = stub_ollama("[]").await;
        observe_pending(
            &pool,
            &reqwest::Client::new(),
            &stub,
            "m",
            "2026-08-09T00:00:00Z",
        )
        .await
        .unwrap();

        let mut seen: Vec<String> =
            sqlx::query_scalar("SELECT source_column FROM pii_observations ORDER BY source_column")
                .fetch_all(&pool)
                .await
                .unwrap();
        seen.dedup();
        let mut expected: Vec<String> = OBSERVABLE_COLUMNS.iter().map(|c| c.to_string()).collect();
        expected.sort();
        assert_eq!(seen, expected, "a column the table permits was never read");
    }

    /// One sweep spends one budget, however many columns it is spread across, and every column gets
    /// a share of it.
    ///
    /// The first version of this test filled only `triage_summary`, so the other two columns had
    /// nothing pending and the total came to `SWEEP_BATCH` whether the budget was shared or handed
    /// out per column — it passed under the bug it was named for. Every column has to have more
    /// work than it can do for the assertion to mean anything.
    ///
    /// The starvation half is not hypothetical: the first live sweep after this change spent all
    /// its whole batch on `subject` and never reached the sender names or the summaries.
    #[tokio::test]
    async fn the_batch_is_a_budget_shared_across_columns() {
        let pool = test_pool().await;
        for id in 1..=(SWEEP_BATCH + 5) {
            sqlx::query(
                "INSERT INTO emails (id, message_id, mailbox, uidvalidity, uid, from_addr,
                                     from_name, subject, received_at, ingested_at, direction,
                                     triage_class, triage_summary)
                 VALUES (?, ?, 'INBOX', 1, ?, 'a@b', 'Rita', 'Assunto', '2026-08-11T00:00:00Z',
                         '2026-08-11T00:00:00Z', 'inbound', 'info', 'resumo')",
            )
            .bind(id)
            .bind(format!("<{id}@b>"))
            .bind(id)
            .execute(&pool)
            .await
            .unwrap();
        }

        let stub = stub_ollama("[]").await;
        let written = observe_pending(
            &pool,
            &reqwest::Client::new(),
            &stub,
            "m",
            "2026-08-11T00:00:00Z",
        )
        .await
        .unwrap();

        assert_eq!(written, SWEEP_BATCH as u64, "the sweep exceeded its budget");

        let mut per_column: Vec<(String, i64)> = sqlx::query_as(
            "SELECT source_column, COUNT(*) FROM pii_observations GROUP BY source_column
              ORDER BY source_column",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        per_column.sort();
        assert_eq!(
            per_column.len(),
            OBSERVABLE_COLUMNS.len(),
            "a column got no share of the budget: {per_column:?}"
        );
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
        assert_eq!(
            tally(&pool).await.unwrap(),
            vec![("triage_summary".to_string(), "none".to_string(), 1)]
        );
    }

    /// A garbled answer is its own outcome, neither a finding nor a clean reading.
    ///
    /// Recording it as `none` would permanently count an unread summary as clean. Recording nothing
    /// at all — the first attempt at this fix — deadlocks the sweep instead: the query takes the
    /// newest unobserved fields, so a batch the model garbles deterministically is one it garbles
    /// again every fifteen minutes, and everything older is never reached.
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
            vec![("triage_summary".to_string(), "unreadable".to_string(), 1)],
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
            vec![(
                "triage_summary".to_string(),
                "unreadable".to_string(),
                MAX_UNREADABLE_ATTEMPTS
            )]
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
            vec![("triage_summary".to_string(), "none".to_string(), 1)],
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
            vec![("triage_summary".to_string(), "unreadable".to_string(), 2)],
            "both summaries must have been looked at in one sweep"
        );
    }

    /// Answers nothing for the first `hang` requests, then answers normally.
    ///
    /// The sleep is far longer than any timeout a test configures, so the caller gives up rather
    /// than the server refusing — which is the distinction the sweep now turns on, and the one a
    /// stopped listener cannot reproduce.
    async fn stub_ollama_hanging_first(hang: usize, answer: &'static str) -> String {
        let seen = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let app = axum::Router::new().fallback(axum::routing::post(
            move |axum::Json(_): axum::Json<serde_json::Value>| {
                let seen = seen.clone();
                async move {
                    if seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst) < hang {
                        tokio::time::sleep(std::time::Duration::from_secs(60)).await;
                    }
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
        format!("http://{address}")
    }

    fn impatient_client() -> reqwest::Client {
        reqwest::Client::builder()
            .timeout(std::time::Duration::from_millis(300))
            .build()
            .expect("a client with a timeout should build")
    }

    /// A hung endpoint must not be written down as fields the model read and could not parse.
    ///
    /// This is the direction that cannot be undone. An `unreadable` row is only ever deleted by a
    /// later successful read of the same field, so observations invented while Ollama was blackholed
    /// push fields past `MAX_UNREADABLE_ATTEMPTS` and out of the sweep permanently — a hole in the
    /// denominator of the one table whose entire purpose is to be a denominator.
    #[tokio::test]
    async fn an_endpoint_that_never_answers_records_nothing() {
        let pool = test_pool().await;
        triaged_email(&pool, 1, "o mais antigo").await;
        triaged_email(&pool, 2, "o mais recente").await;
        let hung = stub_ollama_hanging_first(usize::MAX, "[]").await;

        observe_pending(
            &pool,
            &impatient_client(),
            &hung,
            "m",
            "2026-08-09T00:00:00Z",
        )
        .await
        .unwrap();

        assert_eq!(
            tally(&pool).await.unwrap(),
            vec![],
            "a hung endpoint was recorded as though the model had looked and failed"
        );
    }

    /// And the stall stays fixed: one slow field must still be counted, not retried for ever.
    ///
    /// The two tests are a pair and neither is meaningful alone — the first says a timeout is not
    /// evidence, this one says it becomes evidence as soon as the endpoint proves it was alive.
    /// Without this one, "record nothing on a timeout" passes the first test and reinstates the
    /// deadlock where the newest field times out, is offered first again, and blocks everything
    /// older for ever.
    #[tokio::test]
    async fn a_field_that_times_out_is_counted_once_the_endpoint_answers() {
        let pool = test_pool().await;
        triaged_email(&pool, 1, "o mais antigo").await;
        triaged_email(&pool, 2, "o mais recente").await;
        let slow_once = stub_ollama_hanging_first(1, "[]").await;

        observe_pending(
            &pool,
            &impatient_client(),
            &slow_once,
            "m",
            "2026-08-09T00:00:00Z",
        )
        .await
        .unwrap();

        let mut counts = tally(&pool).await.unwrap();
        counts.sort();
        assert_eq!(
            counts,
            vec![
                ("triage_summary".to_string(), "none".to_string(), 1),
                ("triage_summary".to_string(), "unreadable".to_string(), 1),
            ],
            "the slow field should be an attempt and the one behind it should have been read"
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
