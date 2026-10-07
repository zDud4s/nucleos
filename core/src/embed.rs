//! Local (Ollama) embeddings of knowledge rows (spec 5.4, D6).
//!
//! Embeddings are always local: no text of a project goes to a hosted service for this. Nothing in
//! this module logs a title, a body, a query or a vector.

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, RwLock};
use std::time::Duration;

use sqlx::{Row, SqliteConnection, SqlitePool};

/// The deadline a briefing gives an embedding: past it the briefing goes without similarity.
pub const EMBED_TIMEOUT: Duration = Duration::from_secs(2);

/// The client's own timeout, long enough for a cold model load on a write or a backfill.
const CLIENT_TIMEOUT: Duration = Duration::from_secs(30);

#[async_trait::async_trait]
pub trait Embedder: Send + Sync {
    fn model(&self) -> &str;
    async fn embed(&self, text: &str) -> std::io::Result<Vec<f32>>;
}

pub struct OllamaEmbedder {
    client: reqwest::Client,
    base_url: String,
    model: String,
}

impl OllamaEmbedder {
    pub fn new(base_url: String, model: String) -> Self {
        let client = reqwest::Client::builder()
            .timeout(CLIENT_TIMEOUT)
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self {
            client,
            base_url,
            model,
        }
    }
}

fn classify(error: reqwest::Error) -> std::io::Error {
    if error.is_timeout() {
        std::io::Error::new(std::io::ErrorKind::TimedOut, error)
    } else {
        std::io::Error::other(error)
    }
}

#[async_trait::async_trait]
impl Embedder for OllamaEmbedder {
    fn model(&self) -> &str {
        &self.model
    }

    async fn embed(&self, text: &str) -> std::io::Result<Vec<f32>> {
        let response = self
            .client
            .post(format!("{}/api/embed", self.base_url))
            .json(&serde_json::json!({"model": self.model, "input": text}))
            .send()
            .await
            .map_err(classify)?;
        let status = response.status();
        if !status.is_success() {
            let detail = response.text().await.unwrap_or_default();
            return Err(std::io::Error::other(format!(
                "Ollama returned {status}: {}",
                detail.trim()
            )));
        }
        let body = response
            .json::<serde_json::Value>()
            .await
            .map_err(classify)?;
        let first = body
            .get("embeddings")
            .and_then(|embeddings| embeddings.get(0))
            .and_then(|vector| vector.as_array())
            .ok_or_else(|| std::io::Error::other("Ollama response did not contain an embedding"))?;
        first
            .iter()
            .map(|number| {
                number
                    .as_f64()
                    .map(|value| value as f32)
                    .ok_or_else(|| std::io::Error::other("Ollama embedding held a non-number"))
            })
            .collect()
    }
}

/// The text a knowledge row is embedded from.
pub fn text_of(title: &str, body: &str) -> String {
    format!("{title}\n{body}")
}

/// f32 little-endian, `vector.len() * 4` bytes.
pub fn encode(vector: &[f32]) -> Vec<u8> {
    vector
        .iter()
        .flat_map(|value| value.to_le_bytes())
        .collect()
}

/// The inverse of [`encode`]; `None` unless the blob is exactly `dim * 4` bytes and `dim > 0`.
pub fn decode(blob: &[u8], dim: i64) -> Option<Vec<f32>> {
    if dim <= 0 || blob.len() as i64 != dim.checked_mul(4)? {
        return None;
    }
    Some(
        blob.chunks_exact(4)
            .map(|chunk| f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
            .collect(),
    )
}

/// Cosine similarity; `None` on empty input, a length mismatch, a zero norm or a non-finite value.
pub fn cosine(a: &[f32], b: &[f32]) -> Option<f32> {
    if a.is_empty() || a.len() != b.len() {
        return None;
    }
    let (mut dot, mut norm_a, mut norm_b) = (0.0_f64, 0.0_f64, 0.0_f64);
    for (x, y) in a.iter().zip(b) {
        let (x, y) = (f64::from(*x), f64::from(*y));
        dot += x * y;
        norm_a += x * x;
        norm_b += y * y;
    }
    if norm_a == 0.0 || norm_b == 0.0 {
        return None;
    }
    let result = dot / (norm_a.sqrt() * norm_b.sqrt());
    result.is_finite().then_some(result as f32)
}

pub async fn store_in(
    conn: &mut SqliteConnection,
    knowledge_id: i64,
    model: &str,
    vector: &[f32],
) -> sqlx::Result<()> {
    sqlx::query(
        "INSERT INTO knowledge_embeddings (knowledge_id, model, dim, vector, created_at)
         VALUES (?, ?, ?, ?, ?)
         ON CONFLICT(knowledge_id) DO UPDATE SET
           model = excluded.model,
           dim = excluded.dim,
           vector = excluded.vector,
           created_at = excluded.created_at",
    )
    .bind(knowledge_id)
    .bind(model)
    .bind(vector.len() as i64)
    .bind(encode(vector))
    .bind(chrono::Utc::now().to_rfc3339())
    .execute(conn)
    .await?;
    Ok(())
}

/// The stored vectors of `ids` embedded by `model`; undecodable blobs are skipped.
pub async fn vectors_for(
    pool: &SqlitePool,
    ids: &[i64],
    model: &str,
) -> sqlx::Result<HashMap<i64, Vec<f32>>> {
    if ids.is_empty() {
        return Ok(HashMap::new());
    }
    let placeholders = vec!["?"; ids.len()].join(", ");
    // SAFETY: only literal `?` placeholders are interpolated, as in `brief::fts_ranks`.
    let sql = sqlx::AssertSqlSafe(format!(
        "SELECT knowledge_id, dim, vector FROM knowledge_embeddings
          WHERE model = ? AND knowledge_id IN ({placeholders})"
    ));
    let mut query = sqlx::query(sql).bind(model);
    for id in ids {
        query = query.bind(id);
    }
    let rows = query.fetch_all(pool).await?;
    let mut out = HashMap::new();
    for row in rows {
        let id: i64 = row.get("knowledge_id");
        let dim: i64 = row.get("dim");
        let blob: Vec<u8> = row.get("vector");
        if let Some(vector) = decode(&blob, dim) {
            out.insert(id, vector);
        }
    }
    Ok(out)
}

/// The live row of `project_id` closest to `vector` among those embedded by `model`, with its
/// cosine. A tie goes to the lowest id.
pub async fn nearest_in(
    conn: &mut SqliteConnection,
    project_id: &str,
    model: &str,
    vector: &[f32],
) -> sqlx::Result<Option<(i64, f32)>> {
    let rows = sqlx::query(
        "SELECT e.knowledge_id, e.dim, e.vector
           FROM knowledge_embeddings e
           JOIN knowledge k ON k.id = e.knowledge_id
          WHERE k.scope_kind = 'project' AND k.scope_id = ?
            AND k.status IN ('active', 'proposed')
            AND e.model = ?
          ORDER BY e.knowledge_id",
    )
    .bind(project_id)
    .bind(model)
    .fetch_all(conn)
    .await?;
    let mut best: Option<(i64, f32)> = None;
    for row in rows {
        let id: i64 = row.get("knowledge_id");
        let dim: i64 = row.get("dim");
        let blob: Vec<u8> = row.get("vector");
        let Some(stored) = decode(&blob, dim) else {
            continue;
        };
        let Some(similarity) = cosine(vector, &stored) else {
            continue;
        };
        if best.is_none_or(|(_, top)| similarity > top) {
            best = Some((id, similarity));
        }
    }
    Ok(best)
}

/// Embeds up to `limit` live rows that lack a vector from this embedder's model, oldest first.
/// Stops at the first embed error (Ollama down) and returns the count done so far.
pub async fn backfill(
    pool: &SqlitePool,
    embedder: &dyn Embedder,
    limit: i64,
) -> sqlx::Result<usize> {
    let rows = sqlx::query(
        "SELECT k.id, k.title, k.body
           FROM knowledge k
           LEFT JOIN knowledge_embeddings e ON e.knowledge_id = k.id
          WHERE k.status IN ('active', 'proposed')
            AND (e.knowledge_id IS NULL OR e.model <> ?)
          ORDER BY k.id
          LIMIT ?",
    )
    .bind(embedder.model())
    .bind(limit)
    .fetch_all(pool)
    .await?;
    let mut done = 0;
    for row in rows {
        let id: i64 = row.get("id");
        let title: String = row.get("title");
        let body: String = row.get("body");
        let Ok(vector) = embedder.embed(&text_of(&title, &body)).await else {
            break;
        };
        let mut conn = pool.acquire().await?;
        store_in(&mut conn, id, embedder.model(), &vector).await?;
        done += 1;
    }
    Ok(done)
}

static EMBEDDER: RwLock<Option<Arc<dyn Embedder>>> = RwLock::new(None);

/// Installs the process-wide embedder, replacing an earlier one: the daemon at start, and the
/// settings door when the owner picks another model. Never from a test that shares the process.
///
/// Swapping the model needs nothing else. Every reader asks `installed()` per use and filters
/// vectors by `model()` (`vectors_for`, `nearest_in`), so rows embedded by the old model are simply
/// not compared; `backfill` selects rows whose vector is missing or from another model, so the
/// worker re-embeds them under the new one once it is nudged.
pub fn install(embedder: Arc<dyn Embedder>) {
    *EMBEDDER.write().unwrap_or_else(|e| e.into_inner()) = Some(embedder);
}

pub fn installed() -> Option<Arc<dyn Embedder>> {
    EMBEDDER.read().unwrap_or_else(|e| e.into_inner()).clone()
}

static WAKE: LazyLock<tokio::sync::Notify> = LazyLock::new(tokio::sync::Notify::new);

/// Wakes the backfill worker.
pub fn nudge() {
    WAKE.notify_one();
}

pub async fn nudged() {
    WAKE.notified().await;
}

#[cfg(test)]
pub(crate) use fake::FakeEmbedder;

#[cfg(test)]
pub(crate) mod fake {
    use std::sync::Mutex;
    use std::time::Duration;

    use super::Embedder;

    /// A scripted embedder for tests: the first `vectors` entry whose key the text STARTS WITH
    /// wins, else `default`, else an error.
    pub(crate) struct FakeEmbedder {
        pub model: String,
        pub vectors: Vec<(String, Vec<f32>)>,
        pub default: Option<Vec<f32>>,
        pub fail: bool,
        pub delay: Option<Duration>,
        pub calls: Mutex<Vec<String>>,
    }

    impl FakeEmbedder {
        pub(crate) fn new(
            model: &str,
            vectors: Vec<(String, Vec<f32>)>,
            default: Option<Vec<f32>>,
        ) -> Self {
            Self {
                model: model.to_string(),
                vectors,
                default,
                fail: false,
                delay: None,
                calls: Mutex::default(),
            }
        }

        pub(crate) fn failing() -> Self {
            Self {
                fail: true,
                ..Self::new("fake", Vec::new(), None)
            }
        }

        pub(crate) fn slow(delay: Duration) -> Self {
            Self {
                delay: Some(delay),
                ..Self::new("fake", Vec::new(), Some(vec![1.0, 0.0]))
            }
        }
    }

    #[async_trait::async_trait]
    impl Embedder for FakeEmbedder {
        fn model(&self) -> &str {
            &self.model
        }

        async fn embed(&self, text: &str) -> std::io::Result<Vec<f32>> {
            self.calls.lock().unwrap().push(text.to_string());
            if let Some(delay) = self.delay {
                tokio::time::sleep(delay).await;
            }
            if self.fail {
                return Err(std::io::Error::other("fake embedder failure"));
            }
            self.vectors
                .iter()
                .find(|(key, _)| text.starts_with(key.as_str()))
                .map(|(_, vector)| vector.clone())
                .or_else(|| self.default.clone())
                .ok_or_else(|| std::io::Error::other("fake embedder has no vector"))
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use sqlx::SqlitePool;
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

    use super::{Embedder, OllamaEmbedder, backfill, cosine, decode, encode, nearest_in, store_in};

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

    async fn seed(pool: &SqlitePool, scope_id: &str, status: &str, title: &str) -> i64 {
        sqlx::query(
            "INSERT INTO knowledge
               (layer, scope_kind, scope_id, source, kind, title, body, status, created_at)
             VALUES ('semantic', 'project', ?, 'owner', 'memory', ?, 'body', ?,
                     '2026-08-19T00:00:00+00:00')",
        )
        .bind(scope_id)
        .bind(title)
        .bind(status)
        .execute(pool)
        .await
        .unwrap()
        .last_insert_rowid()
    }

    async fn store(pool: &SqlitePool, id: i64, model: &str, vector: &[f32]) {
        let mut conn = pool.acquire().await.unwrap();
        store_in(&mut conn, id, model, vector).await.unwrap();
    }

    async fn stored_model(pool: &SqlitePool, id: i64) -> Option<String> {
        sqlx::query_scalar("SELECT model FROM knowledge_embeddings WHERE knowledge_id = ?")
            .bind(id)
            .fetch_optional(pool)
            .await
            .unwrap()
    }

    /// An embedder that answers the same vector for every text and records what it was asked.
    struct Constant {
        model: String,
        calls: Mutex<Vec<String>>,
    }

    #[async_trait::async_trait]
    impl Embedder for Constant {
        fn model(&self) -> &str {
            &self.model
        }

        async fn embed(&self, text: &str) -> std::io::Result<Vec<f32>> {
            self.calls.lock().unwrap().push(text.to_string());
            Ok(vec![1.0, 0.0])
        }
    }

    #[test]
    fn a_vector_survives_the_blob_round_trip() {
        let vector = vec![0.25_f32, -1.5, 0.0, 3.125];

        let blob = encode(&vector);

        assert_eq!(blob.len(), vector.len() * 4);
        assert_eq!(&blob[..4], &0.25_f32.to_le_bytes());
        assert_eq!(decode(&blob, vector.len() as i64), Some(vector));
    }

    #[test]
    fn a_blob_whose_length_disagrees_with_dim_is_refused() {
        let blob = encode(&[1.0, 2.0, 3.0]);

        assert_eq!(decode(&blob, 2), None);
        assert_eq!(decode(&blob, 4), None);
        assert_eq!(decode(&blob[..11], 3), None);
        assert_eq!(decode(&[], 0), None);
        assert_eq!(decode(&blob, 0), None);
        assert_eq!(decode(&blob, -3), None);
    }

    #[test]
    fn cosine_answers_the_table() {
        let close = |got: Option<f32>, want: f32| {
            let got = got.expect("a defined cosine");
            assert!((got - want).abs() < 1e-6, "got {got}, want {want}");
        };

        close(cosine(&[1.0, 2.0], &[2.0, 4.0]), 1.0);
        close(cosine(&[1.0, 0.0], &[0.0, 1.0]), 0.0);
        close(cosine(&[1.0, 2.0], &[-1.0, -2.0]), -1.0);

        assert_eq!(cosine(&[1.0, 2.0], &[1.0, 2.0, 3.0]), None);
        assert_eq!(cosine(&[0.0, 0.0], &[1.0, 2.0]), None);
        assert_eq!(cosine(&[1.0, 2.0], &[0.0, 0.0]), None);
        assert_eq!(cosine(&[], &[]), None);
        assert_eq!(cosine(&[f32::NAN, 1.0], &[1.0, 1.0]), None);
        assert_eq!(cosine(&[f32::INFINITY, 1.0], &[1.0, 1.0]), None);
    }

    #[tokio::test]
    async fn the_ollama_embedder_posts_model_and_input_to_api_embed() {
        let seen: Arc<Mutex<Vec<(String, serde_json::Value)>>> = Arc::default();
        let recorder = seen.clone();
        let app = axum::Router::new().fallback(axum::routing::post(
            move |uri: axum::http::Uri, axum::Json(body): axum::Json<serde_json::Value>| {
                let recorder = recorder.clone();
                async move {
                    recorder
                        .lock()
                        .unwrap()
                        .push((uri.path().to_string(), body));
                    axum::Json(serde_json::json!({"embeddings": [[0.1, 0.2]]}))
                }
            },
        ));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let _server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let embedder = OllamaEmbedder::new(format!("http://{address}"), "tiny-embed".to_string());
        let vector = embedder.embed("a title\na body").await.unwrap();

        assert_eq!(embedder.model(), "tiny-embed");
        assert_eq!(vector, vec![0.1_f32, 0.2_f32]);
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].0, "/api/embed");
        assert_eq!(
            seen[0].1,
            serde_json::json!({"model": "tiny-embed", "input": "a title\na body"})
        );
    }

    #[tokio::test]
    async fn backfill_embeds_only_live_rows_lacking_this_models_vector_up_to_the_limit() {
        let pool = test_pool().await;
        let rejected = seed(&pool, "alpha", "rejected", "rejected").await;
        let current = seed(&pool, "alpha", "active", "already embedded").await;
        let stale = seed(&pool, "alpha", "proposed", "embedded by another model").await;
        let first_missing = seed(&pool, "alpha", "active", "missing one").await;
        let second_missing = seed(&pool, "beta", "proposed", "missing two").await;
        let third_missing = seed(&pool, "alpha", "active", "missing three").await;
        store(&pool, current, "m", &[0.0, 1.0]).await;
        store(&pool, stale, "old-model", &[0.0, 1.0]).await;
        let embedder = Constant {
            model: "m".to_string(),
            calls: Mutex::default(),
        };

        let done = backfill(&pool, &embedder, 3).await.unwrap();

        // Oldest id first: the stale row, then two missing ones; the third waits for the next pass.
        assert_eq!(done, 3);
        assert_eq!(stored_model(&pool, stale).await.as_deref(), Some("m"));
        assert_eq!(
            stored_model(&pool, first_missing).await.as_deref(),
            Some("m")
        );
        assert_eq!(
            stored_model(&pool, second_missing).await.as_deref(),
            Some("m")
        );
        assert_eq!(stored_model(&pool, third_missing).await, None);
        assert_eq!(stored_model(&pool, rejected).await, None);
        assert_eq!(embedder.calls.lock().unwrap().len(), 3);

        let rest = backfill(&pool, &embedder, 10).await.unwrap();

        assert_eq!(rest, 1);
        assert_eq!(
            stored_model(&pool, third_missing).await.as_deref(),
            Some("m")
        );
        assert_eq!(stored_model(&pool, rejected).await, None);
        assert_eq!(backfill(&pool, &embedder, 10).await.unwrap(), 0);
    }

    #[tokio::test]
    async fn nearest_looks_only_at_live_rows_of_the_same_project_and_model() {
        let pool = test_pool().await;
        let query = [1.0_f32, 0.0];
        // Perfect matches that must NOT count: another project, another model, a dead status.
        let other_project = seed(&pool, "beta", "active", "other project").await;
        let other_model = seed(&pool, "alpha", "active", "other model").await;
        let dead = seed(&pool, "alpha", "rejected", "rejected").await;
        store(&pool, other_project, "m", &[1.0, 0.0]).await;
        store(&pool, other_model, "old-model", &[1.0, 0.0]).await;
        store(&pool, dead, "m", &[1.0, 0.0]).await;
        let far = seed(&pool, "alpha", "active", "far").await;
        let near = seed(&pool, "alpha", "proposed", "near").await;
        let tied = seed(&pool, "alpha", "active", "tied with near").await;
        store(&pool, far, "m", &[0.0, 1.0]).await;
        store(&pool, near, "m", &[2.0, 1.0]).await;
        store(&pool, tied, "m", &[2.0, 1.0]).await;

        let mut conn = pool.acquire().await.unwrap();
        let (id, similarity) = nearest_in(&mut conn, "alpha", "m", &query)
            .await
            .unwrap()
            .expect("a live neighbour");

        // The lowest id wins a tie, and only the same project, model and live status are looked at.
        assert_eq!(id, near);
        assert!((similarity - 2.0 / 5.0_f32.sqrt()).abs() < 1e-5);
        assert_eq!(
            nearest_in(&mut conn, "gamma", "m", &query).await.unwrap(),
            None
        );
        assert_eq!(
            nearest_in(&mut conn, "alpha", "unknown-model", &query)
                .await
                .unwrap(),
            None
        );
    }
}
