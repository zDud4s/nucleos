//! The distiller: a durable queue of closed jobs and runs whose text is worth reading once, and
//! the worker that turns each one into project-scoped learnings.
//!
//! Design source of truth: `.ai/specs/2026-10-05-destilador-design.md` (§3 is the queue schema,
//! §4 the causes and the worker). Phase A, packet P1 covers the queue, the cause vocabulary and
//! the in-transaction enqueue helpers; nothing here reads a job's text yet.

// Wired in P5: until the job loop and the vcs queue call the helpers, only the tests do.
#![cfg_attr(not(test), allow(dead_code))]

use sqlx::SqliteConnection;

/// Why a job or run was queued. The spelling is what `distill_queue.cause` holds; the
/// `distill_review_blocking` trigger writes `review_blocking` as a SQL literal, which a test pins
/// to [`Cause::ReviewBlocking`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Cause {
    JobLanded,
    JobFailed,
    GateRecovered,
    ReviewBlocking,
    RunExhausted,
}

pub const CAUSES: [Cause; 5] = [
    Cause::JobLanded,
    Cause::JobFailed,
    Cause::GateRecovered,
    Cause::ReviewBlocking,
    Cause::RunExhausted,
];

impl Cause {
    pub fn as_str(self) -> &'static str {
        match self {
            Cause::JobLanded => "job_landed",
            Cause::JobFailed => "job_failed",
            Cause::GateRecovered => "gate_recovered",
            Cause::ReviewBlocking => "review_blocking",
            Cause::RunExhausted => "run_exhausted",
        }
    }

    pub fn parse(s: &str) -> Option<Cause> {
        CAUSES.iter().copied().find(|c| c.as_str() == s)
    }
}

pub const STATUS_PENDING: &str = "pending";
pub const STATUS_RUNNING: &str = "running";
pub const STATUS_DONE: &str = "done";
pub const STATUS_FAILED: &str = "failed";

fn now() -> String {
    chrono::Utc::now().to_rfc3339()
}

/// Queue a job that ended without completing. Callers pass `&mut *tx` so the row rides the
/// retirement's own transaction.
pub async fn enqueue_job_ending_in(
    conn: &mut SqliteConnection,
    job_id: i64,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT OR IGNORE INTO distill_queue
             (cause, project_id, job_id, item_id, run_id, status, attempts, created_at)
         SELECT ?, project_id, id, NULL, NULL, 'pending', 0, ? FROM jobs WHERE id = ?",
    )
    .bind(Cause::JobFailed.as_str())
    .bind(now())
    .bind(job_id)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// Queue the item verdict just written for `(job_id, ordinal)`: a pass after at least one red
/// gate is a recovery, and a `gate_failed` item whose reds exceed the job's retries is
/// exhausted. Anything else queues nothing.
pub async fn enqueue_item_verdict_in(
    conn: &mut SqliteConnection,
    job_id: i64,
    ordinal: i64,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT OR IGNORE INTO distill_queue
             (cause, project_id, job_id, item_id, run_id, status, attempts, created_at)
         SELECT ?, j.project_id, j.id, i.id, i.run_id, 'pending', 0, ?
           FROM job_items i JOIN jobs j ON j.id = i.job_id
          WHERE i.job_id = ? AND i.ordinal = ?
            AND i.gate_status = 'passed' AND i.gate_attempts >= 1",
    )
    .bind(Cause::GateRecovered.as_str())
    .bind(now())
    .bind(job_id)
    .bind(ordinal)
    .execute(&mut *conn)
    .await?;
    sqlx::query(
        "INSERT OR IGNORE INTO distill_queue
             (cause, project_id, job_id, item_id, run_id, status, attempts, created_at)
         SELECT ?, j.project_id, j.id, i.id, i.run_id, 'pending', 0, ?
           FROM job_items i JOIN jobs j ON j.id = i.job_id
          WHERE i.job_id = ? AND i.ordinal = ?
            AND i.status = 'gate_failed' AND i.gate_attempts > j.gate_retries",
    )
    .bind(Cause::RunExhausted.as_str())
    .bind(now())
    .bind(job_id)
    .bind(ordinal)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// Queue a land: a succeeded merge of a job's own branch (`nucleos/job-<id>`, same project) into
/// a branch outside `nucleos/`.
pub async fn enqueue_landed_in(
    conn: &mut SqliteConnection,
    vcs_request_id: i64,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT OR IGNORE INTO distill_queue
             (cause, project_id, job_id, item_id, run_id, status, attempts, created_at)
         SELECT ?, j.project_id, j.id, NULL, NULL, 'pending', 0, ?
           FROM vcs_requests v
           JOIN jobs j ON j.project_id = v.project_id
                      AND json_extract(v.args, '$.source') = 'nucleos/job-' || j.id
          WHERE v.id = ? AND v.status = 'succeeded' AND v.op = 'merge'
            AND json_extract(v.args, '$.target') NOT GLOB 'nucleos/*'",
    )
    .bind(Cause::JobLanded.as_str())
    .bind(now())
    .bind(vcs_request_id)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

// ---- P3: the pure parts (dossier, extraction prompt, strict parsing, layer mapping, backoff) ----

use crate::knowledge::{Kind, Layer};

/// The most characters a dossier may hold before whole blocks start to go (spec §4.3).
pub const DOSSIER_CEILING: usize = 24_000;
/// The most characters kept of one stdout or gate entry, tail first; applied when gathering.
pub const ENTRY_CHARS: usize = 4_000;
pub const MAX_ITEMS: usize = 5;
pub const MAX_ATTEMPTS: i64 = 3;
pub const BACKOFF_BASE: chrono::Duration = chrono::Duration::minutes(5);

/// What failed and what finally passed, for a `gate_recovered` cause.
pub struct Recovery {
    pub failed_headline: String,
    pub passing_run: (i64, String),
}

/// Everything the dossier may say, already gathered and already clipped per entry.
pub struct DossierInputs {
    pub cause: Cause,
    pub owner_context: Vec<String>,
    pub reviews: Vec<(i64, String)>,
    pub job_prompt: Option<String>,
    pub items: Vec<String>,
    pub outcome: Option<String>,
    pub recovery: Option<Recovery>,
    pub known_titles: Vec<String>,
    pub gate_outputs: Vec<(i64, String)>,
}

/// The text handed to the model and the runs whose text survived the cut.
pub struct Dossier {
    pub text: String,
    pub runs: Vec<i64>,
}

/// Keep the first `width` characters (char-safe).
fn clip_head(s: &str, width: usize) -> String {
    s.chars().take(width).collect()
}

fn present(s: &str) -> bool {
    !s.trim().is_empty()
}

/// Render the six blocks of spec §4.3 in order, each with the run ids it names. A block with
/// nothing in it is omitted.
fn blocks(inputs: &DossierInputs) -> Vec<(String, Vec<i64>)> {
    let mut out: Vec<(String, Vec<i64>)> = Vec::new();

    // 1. Owner context: understood, never quoted.
    let notes: Vec<&str> = inputs
        .owner_context
        .iter()
        .map(String::as_str)
        .filter(|n| present(n))
        .collect();
    if !notes.is_empty() {
        out.push((
            format!(
                "## Owner context: for your understanding only. Never quote it.\n{}",
                notes.join("\n---\n")
            ),
            Vec::new(),
        ));
    }

    // 2. Review output.
    let reviews: Vec<&(i64, String)> = inputs.reviews.iter().filter(|(_, t)| present(t)).collect();
    if !reviews.is_empty() {
        let body: Vec<String> = reviews
            .iter()
            .map(|(id, text)| format!("Review run #{id}:\n{text}"))
            .collect();
        out.push((
            format!("## Review output\n{}", body.join("\n\n")),
            reviews.iter().map(|(id, _)| *id).collect(),
        ));
    }

    // 3. The job: prompt, items, outcome.
    let mut job: Vec<String> = Vec::new();
    if let Some(prompt) = inputs.job_prompt.as_deref().filter(|p| present(p)) {
        job.push(format!("Job prompt:\n{prompt}"));
    }
    let items: Vec<&str> = inputs
        .items
        .iter()
        .map(String::as_str)
        .filter(|i| present(i))
        .collect();
    if !items.is_empty() {
        job.push(format!("Items:\n- {}", items.join("\n- ")));
    }
    if let Some(outcome) = inputs.outcome.as_deref().filter(|o| present(o)) {
        job.push(format!("Outcome:\n{outcome}"));
    }
    if !job.is_empty() {
        out.push((format!("## The job\n{}", job.join("\n\n")), Vec::new()));
    }

    // 4. A recovery: what failed, what passed.
    if let Some(rec) = &inputs.recovery {
        let (id, stdout) = &rec.passing_run;
        let mut parts: Vec<String> = Vec::new();
        if present(&rec.failed_headline) {
            parts.push(format!("What failed:\n{}", rec.failed_headline));
        }
        if present(stdout) {
            parts.push(format!("The run that passed (run #{id}):\n{stdout}"));
        }
        if !parts.is_empty() {
            out.push((format!("## Recovery\n{}", parts.join("\n\n")), vec![*id]));
        }
    }

    // 5. What is already known, so it is not repeated.
    let known: Vec<&str> = inputs
        .known_titles
        .iter()
        .map(String::as_str)
        .filter(|t| present(t))
        .collect();
    if !known.is_empty() {
        out.push((
            format!("## Already known (do not repeat)\n- {}", known.join("\n- ")),
            Vec::new(),
        ));
    }

    // 6. Gate output.
    let gates: Vec<&(i64, String)> = inputs
        .gate_outputs
        .iter()
        .filter(|(_, t)| present(t))
        .collect();
    if !gates.is_empty() {
        let body: Vec<String> = gates
            .iter()
            .map(|(id, text)| format!("Gate output of run #{id}:\n{text}"))
            .collect();
        out.push((
            format!("## Gate output\n{}", body.join("\n\n")),
            gates.iter().map(|(id, _)| *id).collect(),
        ));
    }

    out
}

/// Build the dossier. While the text is over `ceiling` characters the LAST remaining block goes,
/// whole, until only the first is left; if that alone is still over, it is clipped. A dropped
/// block is announced by one closing line, which may itself pass the ceiling.
pub fn dossier(inputs: &DossierInputs, ceiling: usize) -> Dossier {
    let all = blocks(inputs);
    let total = all.len();
    let join = |n: usize| -> String {
        all[..n]
            .iter()
            .map(|(t, _)| t.as_str())
            .collect::<Vec<_>>()
            .join("\n\n")
    };

    let mut kept = total;
    while kept > 1 && join(kept).chars().count() > ceiling {
        kept -= 1;
    }
    let mut text = join(kept);
    if kept == 1 && text.chars().count() > ceiling {
        text = clip_head(&text, ceiling);
    }
    let dropped = total - kept;
    if dropped > 0 {
        let noun = if dropped == 1 { "block" } else { "blocks" };
        text.push_str(&format!(
            "\n\n[{dropped} {noun} left out to keep this within its size limit]"
        ));
    }

    let mut runs: Vec<i64> = all[..kept]
        .iter()
        .flat_map(|(_, ids)| ids.iter().copied())
        .collect();
    runs.sort_unstable();
    runs.dedup();
    Dossier { text, runs }
}

/// The standing instruction of every extraction call.
pub const STANDING: &str = "You read the dossier below, about one finished piece of work in ONE \
software project, and write down what is worth remembering for the next time. Answer with a JSON \
array and nothing else: 0 to 5 items, each an object with `layer` (episodic, semantic or \
procedural), `title` (one line), `body` (short) and `files` (repository paths it concerns). Only \
lessons about THIS project. Never quote the owner context. Do not repeat the known titles. `[]` \
is a good answer when nothing is worth keeping.";

pub fn extraction_prompt(dossier: &str) -> String {
    format!("{STANDING}\n\n--- DOSSIER ---\n{dossier}\n--- END OF DOSSIER ---")
}

/// The JSON schema of the answer: an array of at most [`MAX_ITEMS`] items.
pub fn items_format() -> serde_json::Value {
    serde_json::json!({
        "type": "array",
        "maxItems": MAX_ITEMS,
        "items": {
            "type": "object",
            "properties": {
                "layer": { "type": "string", "enum": ["episodic", "semantic", "procedural"] },
                "title": { "type": "string" },
                "body": { "type": "string" },
                "files": { "type": "array", "items": { "type": "string" } }
            },
            "required": ["layer", "title", "body"]
        }
    })
}

/// One learning the model proposed, already checked.
#[derive(Debug)]
pub struct Item {
    pub layer: Layer,
    pub title: String,
    pub body: String,
    pub files: Vec<String>,
}

/// Why an answer was refused whole. The text of the answer is never part of it.
#[derive(Debug)]
pub enum ParseError {
    NotAnArray,
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ParseError::NotAnArray => f.write_str("the answer is not a JSON array"),
        }
    }
}

impl std::error::Error for ParseError {}

fn strip_fence(answer: &str) -> &str {
    let s = answer.trim();
    let Some(rest) = s.strip_prefix("```") else {
        return s;
    };
    let rest = rest.strip_prefix("json").unwrap_or(rest);
    match rest.trim_end().strip_suffix("```") {
        Some(inner) => inner.trim(),
        None => s,
    }
}

fn parse_item(value: &serde_json::Value) -> Option<Item> {
    let obj = value.as_object()?;
    let layer = Layer::parse(obj.get("layer")?.as_str()?)?;
    if layer == Layer::Working {
        return None;
    }
    let title = obj.get("title")?.as_str()?.trim();
    let body = obj.get("body")?.as_str()?.trim();
    if title.is_empty() || body.is_empty() {
        return None;
    }
    let files = obj
        .get("files")
        .and_then(|f| f.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    Some(Item {
        layer,
        title: title.to_string(),
        body: body.to_string(),
        files,
    })
}

/// Strict parse: anything that is not a JSON array fails whole; inside an array a bad element is
/// dropped alone, and only the first [`MAX_ITEMS`] good ones are kept.
pub fn parse_items(answer: &str) -> Result<Vec<Item>, ParseError> {
    let values = serde_json::from_str::<Vec<serde_json::Value>>(strip_fence(answer))
        .map_err(|_| ParseError::NotAnArray)?;
    Ok(values
        .iter()
        .filter_map(parse_item)
        .take(MAX_ITEMS)
        .collect())
}

/// How a learning enters the store: recorded as it is, or proposed for a person to approve.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Door {
    Record,
    Propose,
}

/// The model picks only the layer; kind and door follow from it.
pub fn door(layer: Layer) -> (Kind, Door) {
    match layer {
        Layer::Episodic => (Kind::Memory, Door::Record),
        Layer::Semantic => (Kind::Memory, Door::Propose),
        Layer::Procedural => (Kind::Prompt, Door::Propose),
        // The parser never lets `working` through; if one arrives it is proposed like a rule.
        Layer::Working => (Kind::Memory, Door::Propose),
    }
}

/// When to try again after the `attempts_after_failure`-th failure; `None` once it is final.
pub fn retry_at(
    attempts_after_failure: i64,
    now: chrono::DateTime<chrono::Utc>,
) -> Option<chrono::DateTime<chrono::Utc>> {
    if attempts_after_failure >= MAX_ATTEMPTS {
        return None;
    }
    let exp = attempts_after_failure.clamp(0, 16) as u32;
    Some(now + BACKOFF_BASE * (1i32 << exp))
}

#[cfg(test)]
mod tests {
    use super::{
        CAUSES, Cause, enqueue_item_verdict_in, enqueue_job_ending_in, enqueue_landed_in,
    };
    use sqlx::SqlitePool;
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

    async fn test_pool() -> SqlitePool {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(":memory:")
                    .create_if_missing(true),
            )
            .await
            .unwrap();
        sqlx::migrate!().run(&pool).await.unwrap();
        pool
    }

    /// A job row in a status that is not live, so two jobs of one project never meet
    /// `one_live_job_per_project`.
    async fn seed_job(pool: &SqlitePool, id: i64, project: &str, gate_retries: i64) {
        sqlx::query(
            "INSERT INTO jobs (id, project_id, project_root, status, max_items, gate_retries, created_at)
             VALUES (?, ?, 'C:/work', 'failed', 3, ?, '2026-10-05T00:00:00Z')",
        )
        .bind(id)
        .bind(project)
        .bind(gate_retries)
        .execute(pool)
        .await
        .unwrap();
    }

    async fn seed_item(
        pool: &SqlitePool,
        job_id: i64,
        ordinal: i64,
        status: &str,
        gate_status: &str,
        gate_attempts: i64,
    ) -> i64 {
        sqlx::query(
            "INSERT INTO job_items (job_id, ordinal, description, status, gate_status, gate_attempts)
             VALUES (?, ?, 'do the thing', ?, ?, ?)",
        )
        .bind(job_id)
        .bind(ordinal)
        .bind(status)
        .bind(gate_status)
        .bind(gate_attempts)
        .execute(pool)
        .await
        .unwrap()
        .last_insert_rowid()
    }

    async fn seed_run(
        pool: &SqlitePool,
        project: &str,
        status: &str,
        job_id: Option<i64>,
        stage: Option<&str>,
    ) -> i64 {
        sqlx::query(
            "INSERT INTO runs (project_id, prompt, status, created_at, job_id, stage)
             VALUES (?, 'go', ?, '2026-10-05T00:00:00Z', ?, ?)",
        )
        .bind(project)
        .bind(status)
        .bind(job_id)
        .bind(stage)
        .execute(pool)
        .await
        .unwrap()
        .last_insert_rowid()
    }

    async fn seed_merge(
        pool: &SqlitePool,
        project: &str,
        op: &str,
        source: &str,
        target: &str,
        status: &str,
    ) -> i64 {
        let args = serde_json::json!({ "op": op, "source": source, "target": target }).to_string();
        sqlx::query(
            "INSERT INTO vcs_requests (op, args, project_id, project_root, origin, status, created_at)
             VALUES (?, ?, ?, 'C:/work', 'job', ?, '2026-10-05T00:00:00Z')",
        )
        .bind(op)
        .bind(args)
        .bind(project)
        .bind(status)
        .execute(pool)
        .await
        .unwrap()
        .last_insert_rowid()
    }

    async fn queued(pool: &SqlitePool, cause: &str) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM distill_queue WHERE cause = ?")
            .bind(cause)
            .fetch_one(pool)
            .await
            .unwrap()
    }

    async fn queued_total(pool: &SqlitePool) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM distill_queue")
            .fetch_one(pool)
            .await
            .unwrap()
    }

    /// The unique index treats a missing id as -1, so the same cause for the same subject is one
    /// row whether the ids are set or NULL — SQLite alone would call every NULL distinct.
    #[tokio::test]
    async fn the_same_cause_is_queued_once() {
        let pool = test_pool().await;
        seed_job(&pool, 1, "alpha", 0).await;

        let mut conn = pool.acquire().await.unwrap();
        enqueue_job_ending_in(&mut *conn, 1).await.unwrap();
        enqueue_job_ending_in(&mut *conn, 1).await.unwrap();
        drop(conn);
        assert_eq!(
            queued(&pool, Cause::JobFailed.as_str()).await,
            1,
            "the same job ending was queued twice"
        );

        // NULL job, item and run: the expression index is what makes these collide.
        for _ in 0..2 {
            sqlx::query(
                "INSERT OR IGNORE INTO distill_queue (cause, project_id, status, attempts, created_at)
                 VALUES ('job_landed', 'alpha', 'pending', 0, '2026-10-05T00:00:00Z')",
            )
            .execute(&pool)
            .await
            .unwrap();
        }
        assert_eq!(
            queued(&pool, "job_landed").await,
            1,
            "NULL ids defeated the once-only index"
        );

        // A different cause for the same job is a different fact and is kept.
        sqlx::query(
            "INSERT OR IGNORE INTO distill_queue (cause, project_id, job_id, status, attempts, created_at)
             VALUES ('job_landed', 'alpha', 1, 'pending', 0, '2026-10-05T00:00:00Z')",
        )
        .execute(&pool)
        .await
        .unwrap();
        assert_eq!(queued(&pool, "job_landed").await, 2);
        assert_eq!(queued_total(&pool).await, 3);
    }

    /// The trigger rides on the run's own terminal write, so every site that fails a review run
    /// queues it without being edited, and none of the quiet cases may break that write.
    #[tokio::test]
    async fn a_review_that_fails_is_queued_by_the_same_write() {
        let pool = test_pool().await;
        seed_job(&pool, 7, "alpha", 0).await;

        let review = seed_run(&pool, "alpha", "running", Some(7), Some("review")).await;
        let res = sqlx::query("UPDATE runs SET status = 'failed' WHERE id = ?")
            .bind(review)
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(res.rows_affected(), 1, "the run's own write must succeed");

        let rows: Vec<(String, String, Option<i64>, Option<i64>, Option<i64>, String)> =
            sqlx::query_as(
                "SELECT cause, project_id, job_id, item_id, run_id, status FROM distill_queue",
            )
            .fetch_all(&pool)
            .await
            .unwrap();
        assert_eq!(rows.len(), 1, "a failed review queued {} rows", rows.len());
        let (cause, project, job_id, item_id, run_id, status) = &rows[0];
        assert_eq!(cause, Cause::ReviewBlocking.as_str());
        assert_eq!(project, "alpha", "the queue row carries the job's project");
        assert_eq!(*job_id, Some(7));
        assert_eq!(*item_id, None);
        assert_eq!(*run_id, Some(review));
        assert_eq!(status, "pending");

        // Quiet cases: each must leave the queue at exactly one row and its own UPDATE must succeed.
        let implement = seed_run(&pool, "alpha", "running", Some(7), Some("implement")).await;
        let passing = seed_run(&pool, "alpha", "running", Some(7), Some("review")).await;
        let jobless = seed_run(&pool, "alpha", "running", None, Some("review")).await;
        for (id, to) in [
            (implement, "failed"), // not a review
            (passing, "done"),     // a review that did not fail
            (review, "failed"),    // already failed: OLD.status is 'failed'
            (jobless, "failed"),   // a review with no job to name
        ] {
            let res = sqlx::query("UPDATE runs SET status = ? WHERE id = ?")
                .bind(to)
                .bind(id)
                .execute(&pool)
                .await
                .unwrap();
            assert_eq!(res.rows_affected(), 1, "the update of run {id} must succeed");
        }
        assert_eq!(
            queued_total(&pool).await,
            1,
            "a non-review, non-failed, already-failed or job-less run was queued"
        );
    }

    /// The trigger's SQL cannot call Rust, so its cause is a literal that a test pins to the enum.
    #[tokio::test]
    async fn the_review_trigger_speaks_the_cause_vocabulary() {
        let pool = test_pool().await;
        let sql: Option<String> = sqlx::query_scalar(
            "SELECT sql FROM sqlite_master WHERE type = 'trigger' AND name = 'distill_review_blocking'",
        )
        .fetch_optional(&pool)
        .await
        .unwrap();
        let sql = sql.expect("the distill_review_blocking trigger is missing from the schema");
        let literal = format!("'{}'", Cause::ReviewBlocking.as_str());
        assert!(
            sql.contains(&literal),
            "the trigger does not write {literal}, the Rust spelling of the cause"
        );

        let spellings: std::collections::HashSet<&str> =
            CAUSES.iter().map(|c| c.as_str()).collect();
        assert_eq!(CAUSES.len(), 5);
        assert_eq!(spellings.len(), 5, "two causes share a spelling");
        for expected in [
            "job_landed",
            "job_failed",
            "gate_recovered",
            "review_blocking",
            "run_exhausted",
        ] {
            assert!(spellings.contains(expected), "{expected} is not a cause");
        }
    }

    /// `gate_attempts` counts RED gates: a pass after one red is a recovery, and an item is
    /// exhausted only once its reds exceed the job's retries.
    #[tokio::test]
    async fn an_item_verdict_queues_recovery_or_exhaustion_and_nothing_else() {
        let pool = test_pool().await;
        seed_job(&pool, 3, "alpha", 1).await;

        // The job ending is queued with the job's project.
        let mut conn = pool.acquire().await.unwrap();
        enqueue_job_ending_in(&mut *conn, 3).await.unwrap();
        drop(conn);
        let ending: Vec<(String, i64)> = sqlx::query_as(
            "SELECT project_id, job_id FROM distill_queue WHERE cause = 'job_failed'",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(ending, vec![("alpha".to_string(), 3)]);

        let recovered = seed_item(&pool, 3, 1, "done", "passed", 1).await;
        let exhausted = seed_item(&pool, 3, 2, "gate_failed", "failed", 2).await;
        let first_time = seed_item(&pool, 3, 3, "done", "passed", 0).await;
        let still_retrying = seed_item(&pool, 3, 4, "gate_failed", "failed", 1).await;

        let mut conn = pool.acquire().await.unwrap();
        for ordinal in 1..=4 {
            enqueue_item_verdict_in(&mut *conn, 3, ordinal).await.unwrap();
        }
        // Writing the same verdict again is not a second cause.
        enqueue_item_verdict_in(&mut *conn, 3, 1).await.unwrap();
        enqueue_item_verdict_in(&mut *conn, 3, 2).await.unwrap();
        drop(conn);

        let items_for = |cause: &'static str| {
            let pool = pool.clone();
            async move {
                sqlx::query_as::<_, (Option<i64>, String)>(
                    "SELECT item_id, project_id FROM distill_queue WHERE cause = ?",
                )
                .bind(cause)
                .fetch_all(&pool)
                .await
                .unwrap()
            }
        };
        assert_eq!(
            items_for(Cause::GateRecovered.as_str()).await,
            vec![(Some(recovered), "alpha".to_string())],
            "only the item that passed after a red recovers"
        );
        assert_eq!(
            items_for(Cause::RunExhausted.as_str()).await,
            vec![(Some(exhausted), "alpha".to_string())],
            "only the item whose reds exceed gate_retries is exhausted"
        );
        for untouched in [first_time, still_retrying] {
            let n: i64 =
                sqlx::query_scalar("SELECT COUNT(*) FROM distill_queue WHERE item_id = ?")
                    .bind(untouched)
                    .fetch_one(&pool)
                    .await
                    .unwrap();
            assert_eq!(n, 0, "item {untouched} queued something it should not");
        }
    }

    /// A land's success is a merge of the job's own branch into a branch a person owns; a merge
    /// between `nucleos/` branches, a feature branch, or a request that did not succeed is not.
    #[tokio::test]
    async fn only_a_landed_job_branch_is_queued_as_landed() {
        let pool = test_pool().await;
        seed_job(&pool, 5, "alpha", 0).await;
        seed_job(&pool, 6, "bravo", 0).await;

        let landed = seed_merge(&pool, "alpha", "merge", "nucleos/job-5", "master", "succeeded").await;
        let not_succeeded =
            seed_merge(&pool, "alpha", "merge", "nucleos/job-5", "master", "failed").await;
        let feature = seed_merge(&pool, "alpha", "merge", "feat/x", "master", "succeeded").await;
        let into_nucleos =
            seed_merge(&pool, "alpha", "merge", "nucleos/job-5", "nucleos/staging", "succeeded")
                .await;
        let not_a_merge =
            seed_merge(&pool, "alpha", "rebase", "nucleos/job-5", "master", "succeeded").await;
        // Job 6 belongs to another project, so a request of `alpha` naming it matches nothing.
        let foreign = seed_merge(&pool, "alpha", "merge", "nucleos/job-6", "master", "succeeded").await;

        let mut conn = pool.acquire().await.unwrap();
        for id in [not_succeeded, feature, into_nucleos, not_a_merge, foreign] {
            enqueue_landed_in(&mut *conn, id).await.unwrap();
        }
        drop(conn);
        assert_eq!(
            queued_total(&pool).await,
            0,
            "something that did not land was queued as landed"
        );

        let mut conn = pool.acquire().await.unwrap();
        enqueue_landed_in(&mut *conn, landed).await.unwrap();
        enqueue_landed_in(&mut *conn, landed).await.unwrap();
        drop(conn);
        let rows: Vec<(String, String, Option<i64>)> =
            sqlx::query_as("SELECT cause, project_id, job_id FROM distill_queue")
                .fetch_all(&pool)
                .await
                .unwrap();
        assert_eq!(
            rows,
            vec![(
                Cause::JobLanded.as_str().to_string(),
                "alpha".to_string(),
                Some(5)
            )],
            "a landed job branch is queued once, with its job and project"
        );
    }

    // ---- P3: the pure parts (dossier, parsing, layer mapping, backoff) ----

    use super::{
        BACKOFF_BASE, Door, DossierInputs, MAX_ATTEMPTS, MAX_ITEMS, ParseError, Recovery, door,
        dossier, parse_items, retry_at,
    };
    use crate::knowledge::{Kind, Layer};
    use chrono::TimeZone;

    /// Every block filled, each with a marker no other block carries, so a test can tell which
    /// blocks a rendering kept. Run 1 appears in a review AND a gate output, to pin the dedup.
    fn full_inputs() -> DossierInputs {
        DossierInputs {
            cause: Cause::GateRecovered,
            owner_context: vec!["OWNER-NOTE".to_string()],
            reviews: vec![(1, "REVIEW-ONE".to_string()), (2, "REVIEW-TWO".to_string())],
            job_prompt: Some("JOB-PROMPT".to_string()),
            items: vec!["ITEM-A".to_string()],
            outcome: Some("OUTCOME-X".to_string()),
            recovery: Some(Recovery {
                failed_headline: "FAILED-HEADLINE".to_string(),
                passing_run: (3, "PASSING-STDOUT".to_string()),
            }),
            known_titles: vec!["KNOWN-TITLE".to_string()],
            gate_outputs: vec![(4, "g".repeat(500)), (1, "GATE-OUT-AGAIN".to_string())],
        }
    }

    fn at(text: &str, needle: &str) -> usize {
        text.find(needle)
            .unwrap_or_else(|| panic!("`{needle}` is missing from the dossier:\n{text}"))
    }

    /// Spec §4.3: blocks 1..6 in a fixed order, and a ceiling cuts whole blocks from the END, never
    /// mid-block, saying so in the text; the run ids of a dropped block leave `runs`.
    #[test]
    fn the_dossier_keeps_its_order_and_cuts_whole_blocks_from_the_end() {
        let inputs = full_inputs();
        let full = dossier(&inputs, 1_000_000);

        let order = [
            "OWNER-NOTE",
            "REVIEW-ONE",
            "JOB-PROMPT",
            "FAILED-HEADLINE",
            "KNOWN-TITLE",
            "GATE-OUT-AGAIN",
        ];
        let positions: Vec<usize> = order.iter().map(|m| at(&full.text, m)).collect();
        assert!(
            positions.windows(2).all(|w| w[0] < w[1]),
            "blocks are out of the spec order: {positions:?}"
        );
        assert!(full.text.contains("ITEM-A") && full.text.contains("OUTCOME-X"));
        assert!(full.text.contains("PASSING-STDOUT") && full.text.contains("REVIEW-TWO"));
        assert_eq!(
            full.runs,
            vec![1, 2, 3, 4],
            "runs are the surviving review/recovery/gate ids, deduplicated, ascending"
        );

        // One char under the full length: only block 6 (the long gate output) has to go.
        let ceiling = full.text.chars().count() - 1;
        let one_cut = dossier(&inputs, ceiling);
        assert!(one_cut.text.contains("KNOWN-TITLE"), "block 5 was cut too early");
        assert!(!one_cut.text.contains("GATE-OUT-AGAIN"), "block 6 survived the cut");
        assert!(
            one_cut.text.lines().last().unwrap().contains('1'),
            "the notice names one dropped block:\n{}",
            one_cut.text
        );
        assert_eq!(
            one_cut.runs,
            vec![1, 2, 3],
            "run 4 lived only in the dropped block; run 1 survives through its review"
        );

        // A ceiling that stops just before block 4's text keeps blocks 1-3 whole and drops 4, 5, 6.
        let ceiling = at(&full.text, "FAILED-HEADLINE");
        let three_cut = dossier(&inputs, ceiling);
        for kept in ["OWNER-NOTE", "REVIEW-ONE", "REVIEW-TWO", "JOB-PROMPT", "ITEM-A", "OUTCOME-X"] {
            assert!(three_cut.text.contains(kept), "`{kept}` was lost with the tail");
        }
        for gone in ["FAILED-HEADLINE", "PASSING-STDOUT", "KNOWN-TITLE", "GATE-OUT-AGAIN"] {
            assert!(!three_cut.text.contains(gone), "`{gone}` survived the cut");
        }
        assert!(
            three_cut.text.lines().last().unwrap().contains('3'),
            "the notice names three dropped blocks:\n{}",
            three_cut.text
        );
        assert_eq!(three_cut.runs, vec![1, 2]);

        // Block 1 is never dropped: a ceiling below it clips it and keeps nothing after it.
        let floor = dossier(&inputs, 10);
        assert!(!floor.text.contains("REVIEW-ONE"));
        assert!(floor.runs.is_empty(), "no run id survives when only block 1 is left");
    }

    /// Empty blocks are omitted rather than rendered as a bare heading.
    #[test]
    fn an_empty_block_is_left_out_of_the_dossier() {
        let inputs = DossierInputs {
            cause: Cause::JobFailed,
            owner_context: vec![],
            reviews: vec![],
            job_prompt: Some("JOB-PROMPT".to_string()),
            items: vec![],
            outcome: None,
            recovery: None,
            known_titles: vec![],
            gate_outputs: vec![],
        };
        let d = dossier(&inputs, 1_000_000);
        assert!(d.text.contains("JOB-PROMPT"));
        assert!(!d.text.contains("Never quote it."), "an empty block 1 still printed its heading");
        assert!(d.runs.is_empty());
    }

    /// Block 1 is the owner's own words: the model reads them to understand, and must not repeat
    /// them in what it writes down.
    #[test]
    fn owner_context_is_marked_not_to_be_quoted() {
        let d = dossier(&full_inputs(), 1_000_000);
        let heading = at(&d.text, "Never quote it.");
        let note = at(&d.text, "OWNER-NOTE");
        assert!(heading < note, "the owner text must sit UNDER the do-not-quote heading");
        assert!(
            heading < at(&d.text, "REVIEW-ONE"),
            "the marking belongs to block 1, ahead of the reviews"
        );
    }

    #[test]
    fn a_bad_item_is_dropped_alone() {
        let answer = r#"[
            {"layer": "episodic", "title": "kept one", "body": "b1", "files": ["a.rs", 7, null, "b.rs"]},
            "not an object",
            {"layer": "working", "title": "wrong layer", "body": "b"},
            {"layer": "dream", "title": "unknown layer", "body": "b"},
            {"layer": "semantic", "title": "   ", "body": "blank title"},
            {"layer": "semantic", "title": "blank body", "body": "  \n"},
            {"title": "no layer", "body": "b"},
            {"layer": "procedural", "title": "kept two", "body": "b2"}
        ]"#;
        let items = parse_items(answer).expect("a bad element must not fail the whole answer");
        assert_eq!(items.len(), 2, "only the two good items survive: {items:?}");
        assert_eq!(items[0].layer, Layer::Episodic);
        assert_eq!(items[0].title, "kept one");
        assert_eq!(items[0].body, "b1");
        assert_eq!(
            items[0].files,
            vec!["a.rs".to_string(), "b.rs".to_string()],
            "`files` keeps its string elements and nothing else"
        );
        assert_eq!(items[1].layer, Layer::Procedural);
        assert_eq!(items[1].title, "kept two");
        assert!(items[1].files.is_empty(), "a missing `files` is an empty list");

        assert_eq!(MAX_ITEMS, 5);
        let seven: Vec<String> = (0..7)
            .map(|n| format!(r#"{{"layer":"semantic","title":"t{n}","body":"b{n}"}}"#))
            .collect();
        let many = parse_items(&format!("[{}]", seven.join(","))).unwrap();
        let titles: Vec<&str> = many.iter().map(|i| i.title.as_str()).collect();
        assert_eq!(titles, vec!["t0", "t1", "t2", "t3", "t4"], "the FIRST five are kept");
    }

    #[test]
    fn an_answer_that_is_not_a_json_array_fails_whole() {
        for answer in [
            "I could not find anything worth keeping.",
            r#"{"layer": "semantic", "title": "t", "body": "b"}"#,
            r#"[{"layer": "semantic", "title": "t""#,
            "",
        ] {
            let err = parse_items(answer).expect_err(answer);
            assert!(matches!(err, ParseError::NotAnArray), "{answer:?} gave {err:?}");
            let shown = err.to_string();
            let fragment = answer.trim();
            assert!(
                fragment.is_empty() || !shown.contains(fragment),
                "the error must name the category, never the answer text: {shown}"
            );
        }

        assert!(
            parse_items("[]").expect("an empty array is a good answer").is_empty(),
            "`[]` means nothing was worth keeping"
        );
        assert!(parse_items("  \n[]\n ").unwrap().is_empty(), "surrounding whitespace is trimmed");

        let fenced = "```json\n[{\"layer\":\"semantic\",\"title\":\"t\",\"body\":\"b\"}]\n```";
        let items = parse_items(fenced).expect("a ```json fence around the array is stripped");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].title, "t");
        let bare_fence = "```\n[{\"layer\":\"episodic\",\"title\":\"t\",\"body\":\"b\"}]\n```";
        assert_eq!(parse_items(bare_fence).unwrap().len(), 1, "a bare ``` fence too");
    }

    /// The model picks only the layer; kind and the door it goes through are derived from it, so
    /// `skill` and `subagent` can never come out of the distiller.
    #[test]
    fn the_layer_decides_kind_and_door() {
        assert_eq!(door(Layer::Episodic), (Kind::Memory, Door::Record));
        assert_eq!(door(Layer::Semantic), (Kind::Memory, Door::Propose));
        assert_eq!(door(Layer::Procedural), (Kind::Prompt, Door::Propose));
    }

    /// Table: the delay after the n-th failure is `BACKOFF_BASE * 2^n`, and the third failure ends
    /// the retrying.
    #[test]
    fn backoff_doubles_and_gives_up_after_three() {
        let now = chrono::Utc.with_ymd_and_hms(2026, 10, 5, 12, 0, 0).unwrap();
        assert_eq!(MAX_ATTEMPTS, 3);
        assert_eq!(BACKOFF_BASE, chrono::Duration::minutes(5));
        assert_eq!(retry_at(1, now), Some(now + chrono::Duration::minutes(10)));
        assert_eq!(retry_at(2, now), Some(now + chrono::Duration::minutes(20)));
        assert_eq!(retry_at(3, now), None);
        assert_eq!(retry_at(4, now), None, "past the limit never schedules again");
    }
}
