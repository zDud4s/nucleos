//! Invisible root rotation: ending a root chat's CLI session when its context is large, and
//! carrying the work forward in a rehydration prefix on the next turn.
//!
//! This module only decides, builds and stores. Calling `rotate` at the end of a turn and
//! `take_pending` / `with_prefix` at the start of the next one is wired in `turn.rs` / `send.rs`.

use async_trait::async_trait;
use sqlx::SqlitePool;

/// Context fill (tokens) past which a root chat rotates, unless `NUCLEOS_ROOT_ROTATE_SOFT` says
/// otherwise.
pub const DEFAULT_SOFT_FILL: i64 = 120_000;

/// The rotation threshold: the env override when it is a positive integer, else the default.
pub fn soft_fill() -> i64 {
    std::env::var("NUCLEOS_ROOT_ROTATE_SOFT")
        .ok()
        .and_then(|v| v.trim().parse::<i64>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(DEFAULT_SOFT_FILL)
}

/// The work-tree node store, as rotation needs to see it.
#[async_trait]
pub trait RootNodes: Send + Sync {
    /// Id of the `running` root node bound to this chat (A: GET /nodes/active?chat_id=), or None.
    async fn active_root_for_chat(&self, chat_id: &str) -> Option<String>;
    /// True when some child of this root is in synchronous wait (rotation must not happen then).
    async fn has_waiting_child(&self, root_id: &str) -> bool;
    /// The `text` field of GET /nodes/tree/{root_id}?view=brief (<= 16000 chars), or None.
    async fn brief_text(&self, root_id: &str) -> Option<String>;
}

/// Fallback adapter with no node store behind it: no chat has a root, so nothing rotates.
#[cfg_attr(not(test), allow(dead_code))]
pub struct NoRootNodes;

#[async_trait]
impl RootNodes for NoRootNodes {
    async fn active_root_for_chat(&self, _chat_id: &str) -> Option<String> {
        None
    }
    async fn has_waiting_child(&self, _root_id: &str) -> bool {
        false
    }
    async fn brief_text(&self, _root_id: &str) -> Option<String> {
        None
    }
}

/// The production adapter: [`RootNodes`] over front A's store (`work_nodes`).
///
/// A store error reads as "no root" / "not waiting" / "no brief": rotation then simply does not
/// fire, which is the safe side (the session keeps running).
pub struct DaemonRootNodes {
    pool: SqlitePool,
}

impl DaemonRootNodes {
    pub fn new(pool: &SqlitePool) -> Self {
        Self { pool: pool.clone() }
    }
}

#[async_trait]
impl RootNodes for DaemonRootNodes {
    async fn active_root_for_chat(&self, chat_id: &str) -> Option<String> {
        crate::work_nodes::active(&self.pool, None, Some(chat_id))
            .await
            .unwrap_or_else(|error| {
                tracing::warn!(%error, chat_id, "could not look up the chat's root node");
                None
            })
    }

    /// Waiting child: any non-root node of the root's tree still `planned` or `running`, i.e. a
    /// child that has not closed and will report back, so rotating the root now would cut it off.
    /// Closed (`done`/`blocked`/`handoff`/`failed`) children do not hold rotation: their summary is
    /// already in the brief. The root itself (parent_id NULL) never counts, being the one rotating.
    async fn has_waiting_child(&self, root_id: &str) -> bool {
        let found: sqlx::Result<i64> = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM work_nodes WHERE root_id = ? AND parent_id IS NOT NULL              AND status IN ('planned', 'running'))",
        )
        .bind(root_id)
        .fetch_one(&self.pool)
        .await;
        found.unwrap_or_else(|error| {
            tracing::warn!(%error, root_id, "could not look up waiting children");
            // Unknown: do not rotate.
            1
        }) != 0
    }

    async fn brief_text(&self, root_id: &str) -> Option<String> {
        match crate::work_nodes::tree(&self.pool, root_id, crate::work_nodes::View::Brief).await {
            Ok(tree) if !tree.text.trim().is_empty() => Some(tree.text),
            Ok(_) => None,
            Err(error) => {
                tracing::warn!(%error, root_id, "could not render the root's brief");
                None
            }
        }
    }
}

/// The single place production code obtains the node-store adapter.
#[cfg(not(test))]
pub fn root_nodes(pool: &SqlitePool) -> Box<dyn RootNodes> {
    Box::new(DaemonRootNodes::new(pool))
}

/// Under test the adapter is [`test_nodes::TestNodes`], so the end-of-turn trigger can be exercised.
#[cfg(test)]
pub fn root_nodes(_pool: &SqlitePool) -> Box<dyn RootNodes> {
    Box::new(test_nodes::TestNodes)
}

/// Test-only stand-in for the node store behind [`root_nodes`], keyed by chat id so tests running in
/// parallel never see one another's roots. A chat not registered has no root, like production today.
#[cfg(test)]
pub(crate) mod test_nodes {
    use super::RootNodes;
    use async_trait::async_trait;
    use std::collections::HashMap;
    use std::sync::{LazyLock, Mutex};

    static ROOTS: LazyLock<Mutex<HashMap<String, String>>> =
        LazyLock::new(|| Mutex::new(HashMap::new()));

    /// Gives `chat_id` an active root, so a large-enough turn of that chat rotates.
    pub(crate) fn give_root(chat_id: &str, root_id: &str) {
        ROOTS
            .lock()
            .unwrap()
            .insert(chat_id.to_owned(), root_id.to_owned());
    }

    pub(super) struct TestNodes;

    #[async_trait]
    impl RootNodes for TestNodes {
        async fn active_root_for_chat(&self, chat_id: &str) -> Option<String> {
            ROOTS.lock().unwrap().get(chat_id).cloned()
        }
        async fn has_waiting_child(&self, _root_id: &str) -> bool {
            false
        }
        async fn brief_text(&self, _root_id: &str) -> Option<String> {
            Some("TEST-BRIEF".to_owned())
        }
    }
}

/// Produces the conversation summary carried in a rehydration prefix.
///
/// Only the deterministic implementation exists. An LLM compactor is deferred; it will implement
/// this same trait.
#[async_trait]
pub trait ConversationSummarizer: Send + Sync {
    async fn summarize(&self, pool: &SqlitePool, chat_id: &str) -> String;
}

/// Renders the chat's recent exchanges as `you: ... / núcleo: ...` lines (the same shape
/// `replayed()` uses), then redacts secrets from the result.
pub struct DeterministicSummarizer;

#[async_trait]
impl ConversationSummarizer for DeterministicSummarizer {
    async fn summarize(&self, pool: &SqlitePool, chat_id: &str) -> String {
        let history = match super::send::recent_exchanges(pool, chat_id).await {
            Ok(history) => history,
            Err(error) => {
                tracing::warn!(%error, chat_id, "could not read the exchanges to summarize");
                Vec::new()
            }
        };
        let mut out = String::new();
        for (asked, answered) in &history {
            out.push_str("you: ");
            out.push_str(asked);
            out.push_str("\nnúcleo: ");
            out.push_str(answered);
            out.push_str("\n\n");
        }
        crate::governance::redact::redact_secrets(out.trim_end())
    }
}

/// `Some(root_id)` iff the chat has a root, its fill is strictly past `soft`, and no child of the
/// root is in synchronous wait.
pub async fn should_rotate(
    nodes: &dyn RootNodes,
    chat_id: &str,
    context_fill: Option<i64>,
    soft: i64,
) -> Option<String> {
    let fill = context_fill?;
    if fill <= soft {
        return None;
    }
    let root_id = nodes.active_root_for_chat(chat_id).await?;
    if nodes.has_waiting_child(&root_id).await {
        return None;
    }
    Some(root_id)
}

/// The text the first turn of the new session is prefixed with.
///
/// Worded so the model carries on as the same conversation: it is not told it forgot anything
/// (a model told that hedges and re-asks what was settled; see `replayed()`). The last request is
/// verbatim and labelled as the one to continue with.
pub fn rehydration_prefix(brief: Option<&str>, summary: &str, last_request: &str) -> String {
    let mut out = String::from(
        "This is the same conversation and the same work, carried forward. Below is where things \
         stand; continue from it as your own.\n\n",
    );
    if let Some(brief) = brief {
        out.push_str("## Work tree\n");
        out.push_str(brief);
        out.push_str("\n\n");
    }
    out.push_str("## Conversation so far\n");
    out.push_str(summary);
    out.push_str("\n\n## Last request (verbatim)\n");
    out.push_str(last_request);
    out.push_str("\n\nThat is where things stand.");
    out
}

/// A rotation waiting for the next turn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pending {
    pub prefix: String,
    pub tainted: bool,
}

/// Rotates the chat if `should_rotate` says so, using the configured threshold.
pub async fn rotate(
    pool: &SqlitePool,
    nodes: &dyn RootNodes,
    summarizer: &dyn ConversationSummarizer,
    chat_id: &str,
    run_id: i64,
    context_fill: Option<i64>,
) -> sqlx::Result<bool> {
    rotate_with_soft(
        pool,
        nodes,
        summarizer,
        chat_id,
        run_id,
        context_fill,
        soft_fill(),
    )
    .await
}

/// [`rotate`] with an explicit threshold.
pub async fn rotate_with_soft(
    pool: &SqlitePool,
    nodes: &dyn RootNodes,
    summarizer: &dyn ConversationSummarizer,
    chat_id: &str,
    run_id: i64,
    context_fill: Option<i64>,
    soft: i64,
) -> sqlx::Result<bool> {
    let Some(root_id) = should_rotate(nodes, chat_id, context_fill, soft).await else {
        return Ok(false);
    };
    let last_request: String = sqlx::query_scalar("SELECT prompt FROM runs WHERE id = ?")
        .bind(run_id)
        .fetch_optional(pool)
        .await?
        .unwrap_or_default();
    let brief = nodes.brief_text(&root_id).await;
    let summary = summarizer.summarize(pool, chat_id).await;
    // The verbatim last request and the brief bypass the summarizer, so redact the whole prefix.
    let prefix = crate::governance::redact::redact_secrets(&rehydration_prefix(
        brief.as_deref(),
        &summary,
        &last_request,
    ));

    // Conservative: any untrusted read since the chat's floor may have reached the brief or the
    // summary.
    let tainted: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM runs
                         WHERE chat_id = ? AND read_untrusted = 1
                           AND id > COALESCE((SELECT cleared_after_run_id FROM chats
                                               WHERE chat_id = ?), 0))",
    )
    .bind(chat_id)
    .bind(chat_id)
    .fetch_one(pool)
    .await?;

    sqlx::query(
        "INSERT INTO root_rotations (chat_id, root_id, from_run_id, prefix, tainted, created_at)
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(chat_id)
    .bind(&root_id)
    .bind(run_id)
    .bind(&prefix)
    .bind(tainted)
    .bind(chrono::Utc::now().to_rfc3339())
    .execute(pool)
    .await?;
    super::forget_session(pool, chat_id).await?;
    Ok(true)
}

/// The newest undelivered rotation for the chat, marked delivered to `run_id`. Older undelivered
/// rows are superseded (marked delivered too) and never returned.
pub async fn take_pending(
    pool: &SqlitePool,
    chat_id: &str,
    run_id: i64,
) -> sqlx::Result<Option<Pending>> {
    let rows: Vec<(i64, String, bool)> = sqlx::query_as(
        "UPDATE root_rotations SET delivered_run_id = ?
          WHERE chat_id = ? AND delivered_run_id IS NULL
          RETURNING id, prefix, tainted",
    )
    .bind(run_id)
    .bind(chat_id)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .max_by_key(|(id, _, _)| *id)
        .map(|(_, prefix, tainted)| Pending { prefix, tainted }))
}

/// The prefix, a separator line, then the new message.
pub fn with_prefix(prefix: &str, prompt: &str) -> String {
    format!("{prefix}\n\n---\nThe new message follows.\n\n{prompt}")
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FakeRootNodes {
        root: Option<String>,
        waiting: bool,
        brief: Option<String>,
    }

    fn fake(root: bool) -> FakeRootNodes {
        FakeRootNodes {
            root: root.then(|| "root-1".to_string()),
            waiting: false,
            brief: Some("BRIEF-TEXT".into()),
        }
    }

    #[async_trait]
    impl RootNodes for FakeRootNodes {
        async fn active_root_for_chat(&self, _: &str) -> Option<String> {
            self.root.clone()
        }
        async fn has_waiting_child(&self, _: &str) -> bool {
            self.waiting
        }
        async fn brief_text(&self, _: &str) -> Option<String> {
            self.brief.clone()
        }
    }

    async fn pool() -> SqlitePool {
        crate::testdb::fresh_pool().await
    }

    async fn run(pool: &SqlitePool, chat: &str, prompt: &str, answer: &str, untrusted: i64) -> i64 {
        sqlx::query(
            "INSERT INTO runs (prompt, status, mode, session_id, chat_id, stdout, read_untrusted,
                               created_at)
             VALUES (?, 'completed', 'assistant', 's', ?, ?, ?, '2026-08-11T10:00:00+00:00')",
        )
        .bind(prompt)
        .bind(chat)
        .bind(answer)
        .bind(untrusted)
        .execute(pool)
        .await
        .unwrap()
        .last_insert_rowid()
    }

    async fn rows(pool: &SqlitePool) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM root_rotations")
            .fetch_one(pool)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn daemon_adapter_finds_the_root_by_chat_and_renders_its_brief() {
        let pool = pool().await;
        let nodes = DaemonRootNodes::new(&pool);
        assert_eq!(nodes.active_root_for_chat("chat-x").await, None);
        let root = open_node(&pool, None, "root", Some("chat-x")).await;
        assert_eq!(
            nodes.active_root_for_chat("chat-x").await,
            Some(root.clone())
        );
        assert_eq!(nodes.active_root_for_chat("other").await, None);
        let brief = nodes.brief_text(&root).await.expect("a brief");
        assert!(!brief.trim().is_empty());
        assert_eq!(nodes.brief_text("missing").await, None);
    }

    #[tokio::test]
    async fn daemon_adapter_counts_only_open_children_as_waiting() {
        let pool = pool().await;
        let nodes = DaemonRootNodes::new(&pool);
        let root = open_node(&pool, None, "root", Some("chat-w")).await;
        assert!(
            !nodes.has_waiting_child(&root).await,
            "a lone root waits on nothing"
        );
        let child = open_node(&pool, Some(&root), "leaf", None).await;
        assert!(
            nodes.has_waiting_child(&root).await,
            "a running child holds rotation"
        );
        sqlx::query("UPDATE work_nodes SET status = 'done' WHERE id = ?")
            .bind(&child)
            .execute(&pool)
            .await
            .unwrap();
        assert!(
            !nodes.has_waiting_child(&root).await,
            "a closed child does not"
        );
    }

    async fn open_node(
        pool: &SqlitePool,
        parent: Option<&str>,
        kind: &str,
        chat: Option<&str>,
    ) -> String {
        let args = crate::work_nodes::OpenArgs {
            parent_id: parent.map(str::to_owned),
            kind: kind.into(),
            title: "t".into(),
            goal: "g".into(),
            chat_id: chat.map(str::to_owned),
            ..Default::default()
        };
        crate::work_nodes::open(pool, None, &crate::work_nodes::NodeLimits::default(), &args)
            .await
            .unwrap()
            .id
    }

    #[tokio::test]
    async fn threshold_is_strict_and_needs_a_fill() {
        let n = fake(true);
        assert_eq!(should_rotate(&n, "c", Some(100), 100).await, None);
        assert_eq!(
            should_rotate(&n, "c", Some(101), 100).await.as_deref(),
            Some("root-1")
        );
        assert_eq!(should_rotate(&n, "c", None, 100).await, None);
    }

    #[tokio::test]
    async fn waiting_child_blocks_rotation() {
        let mut n = fake(true);
        n.waiting = true;
        assert_eq!(should_rotate(&n, "c", Some(500), 100).await, None);
    }

    #[tokio::test]
    async fn no_root_is_a_no_op_and_keeps_the_session() {
        let pool = pool().await;
        let id = run(&pool, "c", "hi", "yo", 0).await;
        crate::assistant::upsert_session(&pool, "c", "sess", "2026-08-11T10:00:00+00:00")
            .await
            .unwrap();
        let done = rotate_with_soft(
            &pool,
            &NoRootNodes,
            &DeterministicSummarizer,
            "c",
            id,
            Some(999),
            1,
        )
        .await
        .unwrap();
        assert!(!done);
        assert_eq!(rows(&pool).await, 0);
        assert_eq!(
            crate::assistant::get_session(&pool, "c")
                .await
                .unwrap()
                .as_deref(),
            Some("sess")
        );
    }

    #[tokio::test]
    async fn rotation_stores_the_prefix_and_forgets_the_session() {
        let pool = pool().await;
        run(&pool, "c", "first question", "first answer", 0).await;
        let id = run(&pool, "c", "the last request", "done", 0).await;
        crate::assistant::upsert_session(&pool, "c", "sess", "2026-08-11T10:00:00+00:00")
            .await
            .unwrap();
        let done = rotate_with_soft(
            &pool,
            &fake(true),
            &DeterministicSummarizer,
            "c",
            id,
            Some(500),
            100,
        )
        .await
        .unwrap();
        assert!(done);
        assert_eq!(
            crate::assistant::get_session(&pool, "c").await.unwrap(),
            None
        );
        let pending = take_pending(&pool, "c", id + 1).await.unwrap().unwrap();
        assert!(pending.prefix.contains("BRIEF-TEXT"));
        assert!(
            pending
                .prefix
                .contains("you: first question\nnúcleo: first answer")
        );
        assert!(
            pending
                .prefix
                .contains("## Last request (verbatim)\nthe last request")
        );
        assert!(!pending.tainted);
    }

    #[tokio::test]
    async fn taint_carries_from_an_untrusted_read() {
        let pool = pool().await;
        let id = run(&pool, "dirty", "q", "a", 1).await;
        rotate_with_soft(
            &pool,
            &fake(true),
            &DeterministicSummarizer,
            "dirty",
            id,
            Some(500),
            100,
        )
        .await
        .unwrap();
        assert!(
            take_pending(&pool, "dirty", 99)
                .await
                .unwrap()
                .unwrap()
                .tainted
        );

        let id = run(&pool, "clean", "q", "a", 0).await;
        rotate_with_soft(
            &pool,
            &fake(true),
            &DeterministicSummarizer,
            "clean",
            id,
            Some(500),
            100,
        )
        .await
        .unwrap();
        assert!(
            !take_pending(&pool, "clean", 99)
                .await
                .unwrap()
                .unwrap()
                .tainted
        );
    }

    #[tokio::test]
    async fn pending_is_delivered_once_and_older_ones_are_superseded() {
        let pool = pool().await;
        let id = run(&pool, "c", "q", "a", 0).await;
        for _ in 0..2 {
            rotate_with_soft(
                &pool,
                &fake(true),
                &DeterministicSummarizer,
                "c",
                id,
                Some(500),
                100,
            )
            .await
            .unwrap();
        }
        assert_eq!(rows(&pool).await, 2);
        assert!(take_pending(&pool, "c", 7).await.unwrap().is_some());
        assert!(take_pending(&pool, "c", 8).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn the_summary_is_redacted() {
        let pool = pool().await;
        let id = run(
            &pool,
            "c",
            "key is sk-abcdefghijklmnopqrstuvwxyz0123456789",
            "noted",
            0,
        )
        .await;
        rotate_with_soft(
            &pool,
            &fake(true),
            &DeterministicSummarizer,
            "c",
            id,
            Some(500),
            100,
        )
        .await
        .unwrap();
        let pending = take_pending(&pool, "c", 9).await.unwrap().unwrap();
        assert!(pending.prefix.contains("you: key is"));
        assert!(
            !pending
                .prefix
                .contains("abcdefghijklmnopqrstuvwxyz0123456789")
        );
    }

    #[test]
    fn prefix_omits_a_missing_brief_and_with_prefix_appends_the_message() {
        let p = rehydration_prefix(None, "S", "L");
        assert!(!p.contains("## Work tree"));
        assert!(p.contains("## Conversation so far\nS"));
        let m = with_prefix(&p, "new message");
        assert!(m.starts_with(&p));
        assert!(m.ends_with("new message"));
    }
}
