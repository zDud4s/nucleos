//! The package a chat travels in, and the import that unpacks it.
//!
//! Built from an allow-list: the struct below IS the list, and nothing outside it can reach the wire.
//! No vault content, control token, device session, API key or settings document is a field, so none
//! can be packaged by accident. The session transcript is copied as it is and never rewritten.
//!
//! The origin builds it with [`build`] while the chat is frozen; the destination unpacks it with
//! [`import`], which a repeat of the same `transfer_id` leaves unchanged.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use sqlx::SqlitePool;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};

use super::transfer::{self, Refusal};
use super::transfer_code::{self, Code};

/// The chat's own fields that travel.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ChatFields {
    pub title: Option<String>,
    pub brain: String,
    pub created_at: String,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub plan_only: bool,
}

/// The CLI session transcript, byte for byte.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SessionFile {
    pub session_id: String,
    /// Standard base64 of the `.jsonl` as it is on disk.
    pub data: String,
}

/// A message waiting in the queue.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct QueuedMessage {
    pub text: String,
    pub model_text: Option<String>,
    pub origin: Option<String>,
    pub images: Option<String>,
}

/// A tool call that was parked on a question when the move began.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ParkedQuestion {
    pub tool: String,
    pub detail: Option<String>,
}

/// Everything that crosses to the other machine. Exactly these fields; see the module comment.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Package {
    pub transfer_id: String,
    pub chat_id: String,
    pub chat: ChatFields,
    pub session: Option<SessionFile>,
    pub code: Option<Code>,
    pub queue: Vec<QueuedMessage>,
    pub parked: Vec<ParkedQuestion>,
    /// A turn was cut or a question was parked: the destination continues the work.
    pub interrupted: bool,
}

/// Why packaging or importing stopped.
#[derive(Debug)]
pub enum Failure {
    Refused(Refusal),
    /// Something about the code, the session or the package itself; the words are for the person.
    Problem(String),
}

impl From<Refusal> for Failure {
    fn from(refusal: Refusal) -> Self {
        Self::Refused(refusal)
    }
}

impl From<sqlx::Error> for Failure {
    fn from(error: sqlx::Error) -> Self {
        Self::Refused(Refusal::Store(error))
    }
}

/// What `begin` saw that a later step can no longer see: the questions parked, and whether it cut a turn.
#[derive(Default, Clone)]
struct Remembered {
    asks: Vec<ParkedQuestion>,
    cut: bool,
}

static REMEMBERED: LazyLock<Mutex<HashMap<String, Remembered>>> = LazyLock::new(Default::default);

fn remembered() -> std::sync::MutexGuard<'static, HashMap<String, Remembered>> {
    REMEMBERED
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Called by `begin` before it cuts anything: keeps the open questions (a cut forgets them) and
/// whether a turn was in flight, for the package to carry. `asks` is what the hook store holds for
/// the chat right now; the caller reads it, since the store lives in the crate above this one.
pub fn remember(chat_id: &str, cut: bool, asks: Vec<ParkedQuestion>) {
    let mut seen = remembered();
    let entry = seen.entry(chat_id.to_owned()).or_default();
    for ask in asks {
        if !entry
            .asks
            .iter()
            .any(|a| a.tool == ask.tool && a.detail == ask.detail)
        {
            entry.asks.push(ask);
        }
    }
    entry.cut |= cut;
}

/// Called by `abort`: a move that did not happen leaves nothing behind.
pub fn forget(chat_id: &str) {
    remembered().remove(chat_id);
}

/// The CLI keys a project's transcripts by its directory with every non-alphanumeric character
/// turned into `-`.
pub fn encode_cwd(path: &str) -> String {
    path.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

fn plain_id(id: &str) -> bool {
    !id.is_empty() && id != "." && id != ".." && !id.contains(['/', '\\', ':', '\0'])
}

type ChatRow = (
    Option<String>,
    String,
    String,
    Option<String>,
    Option<String>,
    i64,
    Option<String>,
    Option<String>,
);

/// Builds the package of a frozen chat. Confirms the freeze alive and refuses if it is not standing.
/// `store` is the CLI's transcript store (`sessions::default_root`). `live_asks` is what the hook
/// store holds for the chat now; it joins the ones `remember` kept before a cut.
pub async fn build(
    pool: &SqlitePool,
    chat_id: &str,
    transfer_id: &str,
    store: &Path,
    live_asks: Vec<ParkedQuestion>,
) -> Result<Package, Failure> {
    if !transfer::touch_at(pool, chat_id, transfer_id, transfer::unix_now()).await? {
        return Err(Refusal::NotFrozenForThis.into());
    }
    let row: Option<ChatRow> = sqlx::query_as(
        "SELECT title, brain, created_at, model, effort, plan_only, cwd, ide_session_id
           FROM chats WHERE chat_id = ?",
    )
    .bind(chat_id)
    .fetch_optional(pool)
    .await?;
    let (title, brain, created_at, model, effort, plan_only, cwd, ide_session) =
        row.ok_or(Refusal::NoSuchChat)?;
    let stored: Option<Option<String>> =
        sqlx::query_scalar("SELECT session_id FROM assistant_sessions WHERE chat_id = ?")
            .bind(chat_id)
            .fetch_optional(pool)
            .await?;
    let session_id: Option<String> = stored.flatten().or(ide_session);
    let session = match session_id {
        Some(id) => match crate::sessions::transcript_path(store, &id) {
            Some(path) => {
                let bytes = std::fs::read(&path)
                    .map_err(|e| Failure::Problem(format!("could not read the session: {e}")))?;
                Some(SessionFile {
                    session_id: id,
                    data: B64.encode(bytes),
                })
            }
            None => None,
        },
        None => None,
    };
    let code = match cwd.as_deref().filter(|c| !c.trim().is_empty()) {
        Some(cwd) => {
            let probe = transfer_code::probe(pool, Path::new(cwd))
                .await
                .map_err(Failure::Problem)?;
            Some(
                transfer_code::capture(probe)
                    .await
                    .map_err(Failure::Problem)?,
            )
        }
        None => None,
    };
    let queue = sqlx::query_as::<_, (String, Option<String>, Option<String>, Option<String>)>(
        "SELECT text, model_text, origin, images FROM chat_queue WHERE chat_id = ? ORDER BY id",
    )
    .bind(chat_id)
    .fetch_all(pool)
    .await?
    .into_iter()
    .map(|(text, model_text, origin, images)| QueuedMessage {
        text,
        model_text,
        origin,
        images,
    })
    .collect();
    let (mut parked, cut) = {
        let seen = remembered().get(chat_id).cloned().unwrap_or_default();
        (seen.asks, seen.cut)
    };
    for ask in live_asks {
        if !parked
            .iter()
            .any(|a| a.tool == ask.tool && a.detail == ask.detail)
        {
            parked.push(ask);
        }
    }
    let interrupted = cut || !parked.is_empty();
    Ok(Package {
        transfer_id: transfer_id.to_owned(),
        chat_id: chat_id.to_owned(),
        chat: ChatFields {
            title,
            brain,
            created_at,
            model,
            effort,
            plan_only: plan_only != 0,
        },
        session,
        code,
        queue,
        parked,
        interrupted,
    })
}

/// What the destination did.
#[derive(Debug, PartialEq, Eq)]
pub enum Imported {
    /// The chat is here now; `queued` messages wait, the first of which starts the turn.
    Done { cwd: Option<PathBuf>, queued: usize },
    /// This `transfer_id` was imported before; nothing changed.
    Already,
}

/// The words a parked question comes back as.
fn reask_text(parked: &[ParkedQuestion]) -> String {
    let list: Vec<String> = parked
        .iter()
        .map(|p| match &p.detail {
            Some(detail) => format!("{} ({detail})", p.tool),
            None => p.tool.clone(),
        })
        .collect();
    format!(
        "{} Before the chat moved machines you were waiting for the owner's answer on: {}. \
         Ask for it again before doing it.",
        super::auto_resume::AUTO_RESUME_TEXT,
        list.join("; ")
    )
}

/// Unpacks a package on this machine. Checks everything that can be refused first (the project is
/// here, the chat is not alive here, the pieces are intact), then builds the tree, copies the
/// transcript into the encoded folder of the new directory, writes the chat and its queue, and
/// records the `transfer_id`. A chat that moved away from here and comes back is revived with the
/// newer content.
pub async fn import(
    pool: &SqlitePool,
    package: &Package,
    store: &Path,
) -> Result<Imported, Failure> {
    if package.transfer_id.trim().is_empty() || package.chat_id.trim().is_empty() {
        return Err(Failure::Problem(
            "the package names no transfer or chat".to_string(),
        ));
    }
    if transfer::was_imported(pool, &package.transfer_id).await? {
        return Ok(Imported::Already);
    }
    let here: Option<Option<String>> =
        sqlx::query_scalar("SELECT moved_to FROM chats WHERE chat_id = ?")
            .bind(&package.chat_id)
            .fetch_optional(pool)
            .await?;
    if matches!(here, Some(None)) {
        return Err(Refusal::AlreadyHere.into());
    }
    if package
        .session
        .as_ref()
        .is_some_and(|s| !plain_id(&s.session_id))
    {
        return Err(Failure::Problem(
            "the package carries an invalid session id".to_string(),
        ));
    }
    let transcript = match &package.session {
        Some(session) => Some(
            B64.decode(&session.data)
                .map_err(|e| Failure::Problem(format!("the session is damaged: {e}")))?,
        ),
        None => None,
    };
    let tree = match &package.code {
        Some(code) => {
            let root = transfer_code::destination_root(pool, &code.project_id)
                .await
                .map_err(Failure::Problem)?;
            Some(
                transfer_code::materialize(code, &root, &package.chat_id, &package.transfer_id)
                    .await
                    .map_err(Failure::Problem)?,
            )
        }
        None => None,
    };
    if let (Some(session), Some(bytes)) = (&package.session, &transcript) {
        let folder = tree.as_ref().map_or_else(
            || store.join("default"),
            |tree| store.join(encode_cwd(&tree.to_string_lossy())),
        );
        std::fs::create_dir_all(&folder)
            .and_then(|()| {
                std::fs::write(folder.join(format!("{}.jsonl", session.session_id)), bytes)
            })
            .map_err(|e| Failure::Problem(format!("could not place the session: {e}")))?;
    }
    let cwd_text = tree.as_ref().map(|t| t.to_string_lossy().into_owned());
    let brain = match package.chat.brain.as_str() {
        brain @ ("cloud" | "local" | "openrouter") => brain,
        _ => "cloud",
    };
    let session_id = package.session.as_ref().map(|s| s.session_id.clone());
    let mut tx = pool.begin().await?;
    let saved = sqlx::query(
        "INSERT INTO chats (chat_id, title, brain, created_at, cwd, ide_session_id, model, effort, plan_only)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT(chat_id) DO UPDATE SET
             title = excluded.title, brain = excluded.brain, cwd = excluded.cwd,
             ide_session_id = excluded.ide_session_id, model = excluded.model,
             effort = excluded.effort, plan_only = excluded.plan_only,
             moved_to = NULL, frozen_for = NULL, frozen_at = NULL, archived_at = NULL
         WHERE chats.moved_to IS NOT NULL",
    )
    .bind(&package.chat_id)
    .bind(&package.chat.title)
    .bind(brain)
    .bind(&package.chat.created_at)
    .bind(&cwd_text)
    .bind(&session_id)
    .bind(&package.chat.model)
    .bind(&package.chat.effort)
    .bind(i64::from(package.chat.plan_only))
    .execute(&mut *tx)
    .await?
    .rows_affected();
    if saved == 0 {
        return Err(Refusal::AlreadyHere.into());
    }
    match &session_id {
        Some(id) => {
            sqlx::query(
                "INSERT INTO assistant_sessions (chat_id, session_id, updated_at) VALUES (?, ?, ?)
                 ON CONFLICT(chat_id) DO UPDATE SET session_id = excluded.session_id,
                                                    updated_at = excluded.updated_at",
            )
            .bind(&package.chat_id)
            .bind(id)
            .bind(chrono::Utc::now().to_rfc3339())
            .execute(&mut *tx)
            .await?;
        }
        None => {
            sqlx::query("DELETE FROM assistant_sessions WHERE chat_id = ?")
                .bind(&package.chat_id)
                .execute(&mut *tx)
                .await?;
        }
    }
    sqlx::query("DELETE FROM chat_queue WHERE chat_id = ?")
        .bind(&package.chat_id)
        .execute(&mut *tx)
        .await?;
    let mut waiting: Vec<(String, Option<String>, String, String)> = package
        .queue
        .iter()
        .map(|m| {
            (
                m.text.clone(),
                m.model_text.clone(),
                m.origin.clone().unwrap_or_else(|| "shell".to_string()),
                m.images.clone().unwrap_or_else(|| "[]".to_string()),
            )
        })
        .collect();
    if !package.parked.is_empty() {
        waiting.push((
            reask_text(&package.parked),
            None,
            "shell".to_string(),
            "[]".to_string(),
        ));
    } else if package.interrupted && waiting.is_empty() {
        waiting.push((
            super::auto_resume::AUTO_RESUME_TEXT.to_string(),
            None,
            "shell".to_string(),
            "[]".to_string(),
        ));
    }
    for (text, model_text, origin, images) in &waiting {
        sqlx::query(
            "INSERT INTO chat_queue (chat_id, text, origin, images, model_text, created_at)
             VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(&package.chat_id)
        .bind(text)
        .bind(origin)
        .bind(images)
        .bind(model_text)
        .bind(chrono::Utc::now().to_rfc3339())
        .execute(&mut *tx)
        .await?;
    }
    sqlx::query("INSERT OR IGNORE INTO chat_transfers_seen (transfer_id, chat_id, seen_at) VALUES (?, ?, ?)")
        .bind(&package.transfer_id)
        .bind(&package.chat_id)
        .bind(transfer::unix_now())
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(Imported::Done {
        cwd: tree,
        queued: waiting.len(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::execution::git_exec::testkit::{git_ok, init_contained_repo, space_free_tempdir};
    use std::ffi::OsStr;

    async fn a_chat(pool: &SqlitePool, chat_id: &str, cwd: Option<&str>) {
        sqlx::query("INSERT INTO chats (chat_id, title, brain, created_at, cwd) VALUES (?, 'work', 'cloud', ?, ?)")
            .bind(chat_id)
            .bind("2026-01-01T00:00:00Z")
            .bind(cwd)
            .execute(pool)
            .await
            .unwrap();
    }

    async fn roster(pool: &SqlitePool, project_id: &str, root: &Path) {
        sqlx::query(
            "INSERT INTO autopilot_state (project_id, mode, project_root) VALUES (?, 'active', ?)",
        )
        .bind(project_id)
        .bind(root.to_string_lossy().into_owned())
        .execute(pool)
        .await
        .unwrap();
    }

    fn freeze(pool: &SqlitePool, chat: &str, id: &str) -> impl std::future::Future<Output = ()> {
        let (pool, chat, id) = (pool.clone(), chat.to_owned(), id.to_owned());
        async move {
            transfer::freeze_at(&pool, &chat, &id, transfer::unix_now())
                .await
                .unwrap()
        }
    }

    #[tokio::test]
    async fn uncommitted_and_new_files_arrive_and_ignored_files_are_listed_as_left_behind() {
        let (_keep, repo) = init_contained_repo("nucleos-move-code-");
        std::fs::write(repo.join(".gitignore"), "*.log\n").unwrap();
        assert!(git_ok(&repo, &[OsStr::new("add"), OsStr::new("-A")]));
        assert!(git_ok(
            &repo,
            &[
                OsStr::new("commit"),
                OsStr::new("-m"),
                OsStr::new("ignore logs")
            ]
        ));
        std::fs::write(repo.join("seed.txt"), "changed but not committed\n").unwrap();
        std::fs::write(repo.join("new.txt"), "brand new\n").unwrap();
        std::fs::write(repo.join("build.log"), "SECRET-LOG").unwrap();
        let dest_parent = space_free_tempdir("nucleos-move-dest-");
        let dest_repo = dest_parent.path().join("dest");
        std::fs::create_dir_all(&dest_repo).unwrap();
        assert!(git_ok(&dest_repo, &[OsStr::new("init")]));
        let store_a = space_free_tempdir("nucleos-move-store-a-");
        let store_b = space_free_tempdir("nucleos-move-store-b-");
        let transcript = b"{\"cwd\":\"x\"}\n{\"line\":2}\n";
        let encoded = store_a.path().join(encode_cwd(&repo.to_string_lossy()));
        std::fs::create_dir_all(&encoded).unwrap();
        std::fs::write(encoded.join("sess-1.jsonl"), transcript).unwrap();

        let origin = crate::testdb::fresh_pool().await;
        let destination = crate::testdb::fresh_pool().await;
        roster(&origin, "alpha", &repo).await;
        roster(&destination, "alpha", &dest_repo).await;
        a_chat(&origin, "c1", Some(&repo.to_string_lossy())).await;
        sqlx::query("INSERT INTO assistant_sessions (chat_id, session_id, updated_at) VALUES ('c1', 'sess-1', 'now')")
            .execute(&origin)
            .await
            .unwrap();

        let preview = transfer::preview(&origin, "c1", false, &[]).await.unwrap();
        assert!(preview.target_ok, "{preview:?}");
        assert!(
            preview.left_behind.iter().any(|p| p == "build.log"),
            "{preview:?}"
        );

        freeze(&origin, "c1", "t-1").await;
        let package = build(&origin, "c1", "t-1", store_a.path(), vec![])
            .await
            .unwrap();
        let code = package.code.as_ref().unwrap();
        assert!(code.left_behind.iter().any(|p| p == "build.log"));
        assert_eq!(
            code.files
                .iter()
                .map(|f| f.path.as_str())
                .collect::<Vec<_>>(),
            vec!["new.txt"]
        );

        let done = import(&destination, &package, store_b.path())
            .await
            .unwrap();
        let tree = match done {
            Imported::Done {
                cwd: Some(tree), ..
            } => tree,
            other => panic!("{other:?}"),
        };
        assert_eq!(
            std::fs::read_to_string(tree.join("seed.txt"))
                .unwrap()
                .replace("\r\n", "\n"),
            "changed but not committed\n"
        );
        assert_eq!(
            std::fs::read_to_string(tree.join("new.txt")).unwrap(),
            "brand new\n"
        );
        assert!(
            !tree.join("build.log").exists(),
            "an ignored file must stay behind"
        );
        let landed = store_b
            .path()
            .join(encode_cwd(&tree.to_string_lossy()))
            .join("sess-1.jsonl");
        assert_eq!(
            std::fs::read(landed).unwrap(),
            transcript,
            "the transcript is copied as it is"
        );
        let cwd: Option<String> = sqlx::query_scalar("SELECT cwd FROM chats WHERE chat_id = 'c1'")
            .fetch_one(&destination)
            .await
            .unwrap();
        assert_eq!(cwd.as_deref(), Some(tree.to_string_lossy().as_ref()));
        let _ = git_ok(
            &dest_repo,
            &[
                OsStr::new("worktree"),
                OsStr::new("remove"),
                OsStr::new("--force"),
                tree.as_os_str(),
            ],
        );
    }

    #[tokio::test]
    async fn repeating_an_import_with_the_same_transfer_id_changes_nothing() {
        let origin = crate::testdb::fresh_pool().await;
        let destination = crate::testdb::fresh_pool().await;
        let store = space_free_tempdir("nucleos-move-store-");
        a_chat(&origin, "c1", None).await;
        crate::chats::enqueue(&origin, "c1", "waiting", "shell", "[]")
            .await
            .unwrap();
        freeze(&origin, "c1", "t-1").await;
        let package = build(&origin, "c1", "t-1", store.path(), vec![])
            .await
            .unwrap();

        let first = import(&destination, &package, store.path()).await.unwrap();
        // Something happens at the destination between the two attempts.
        sqlx::query("UPDATE chats SET title = 'edited here' WHERE chat_id = 'c1'")
            .execute(&destination)
            .await
            .unwrap();
        crate::chats::take_queued(&destination, "c1").await.unwrap();
        let second = import(&destination, &package, store.path()).await.unwrap();

        assert_eq!(
            first,
            Imported::Done {
                cwd: None,
                queued: 1
            }
        );
        assert_eq!(second, Imported::Already);
        let title: Option<String> =
            sqlx::query_scalar("SELECT title FROM chats WHERE chat_id = 'c1'")
                .fetch_one(&destination)
                .await
                .unwrap();
        assert_eq!(title.as_deref(), Some("edited here"));
        assert!(
            crate::chats::queued(&destination, "c1")
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn a_chat_without_a_folder_carries_its_transcript() {
        let origin = crate::testdb::fresh_pool().await;
        let destination = crate::testdb::fresh_pool().await;
        let store_a = space_free_tempdir("nucleos-move-store-a-");
        let store_b = space_free_tempdir("nucleos-move-store-b-");
        a_chat(&origin, "c1", None).await;
        let source_folder = store_a.path().join("default");
        std::fs::create_dir_all(&source_folder).unwrap();
        std::fs::write(source_folder.join("sess-1.jsonl"), b"transcript").unwrap();
        sqlx::query(
            "INSERT INTO assistant_sessions (chat_id, session_id, updated_at)
             VALUES ('c1', 'sess-1', 'now')",
        )
        .execute(&origin)
        .await
        .unwrap();
        freeze(&origin, "c1", "t-1").await;

        let package = build(&origin, "c1", "t-1", store_a.path(), vec![])
            .await
            .unwrap();
        import(&destination, &package, store_b.path())
            .await
            .unwrap();

        assert_eq!(
            std::fs::read(store_b.path().join("default").join("sess-1.jsonl")).unwrap(),
            b"transcript"
        );
    }

    #[tokio::test]
    async fn an_import_cannot_overwrite_a_live_destination_chat() {
        let origin = crate::testdb::fresh_pool().await;
        let destination = crate::testdb::fresh_pool().await;
        let store = space_free_tempdir("nucleos-move-store-");
        a_chat(&origin, "c1", None).await;
        a_chat(&destination, "c1", None).await;
        freeze(&origin, "c1", "t-1").await;
        let package = build(&origin, "c1", "t-1", store.path(), vec![])
            .await
            .unwrap();

        let imported = import(&destination, &package, store.path()).await;

        assert!(matches!(
            imported,
            Err(Failure::Refused(Refusal::AlreadyHere))
        ));
    }

    #[tokio::test]
    async fn a_moved_chat_can_move_back() {
        let a = crate::testdb::fresh_pool().await;
        let b = crate::testdb::fresh_pool().await;
        let store = space_free_tempdir("nucleos-move-store-");
        a_chat(&a, "c1", None).await;
        freeze(&a, "c1", "t-1").await;
        let there = build(&a, "c1", "t-1", store.path(), vec![]).await.unwrap();
        import(&b, &there, store.path()).await.unwrap();
        transfer::finish_at(&a, "c1", "t-1", "laptop", transfer::unix_now())
            .await
            .unwrap();
        assert!(matches!(
            transfer::gate(&a, "c1").await,
            transfer::Gate::Moved(_)
        ));
        // Work continues at B, then the chat comes back.
        sqlx::query("UPDATE chats SET title = 'newer title' WHERE chat_id = 'c1'")
            .execute(&b)
            .await
            .unwrap();
        crate::chats::enqueue(&b, "c1", "typed at B", "shell", "[]")
            .await
            .unwrap();
        freeze(&b, "c1", "t-2").await;
        let back = build(&b, "c1", "t-2", store.path(), vec![]).await.unwrap();

        let done = import(&a, &back, store.path()).await.unwrap();

        assert_eq!(
            done,
            Imported::Done {
                cwd: None,
                queued: 1
            }
        );
        assert_eq!(transfer::gate(&a, "c1").await, transfer::Gate::Open);
        let title: Option<String> =
            sqlx::query_scalar("SELECT title FROM chats WHERE chat_id = 'c1'")
                .fetch_one(&a)
                .await
                .unwrap();
        assert_eq!(title.as_deref(), Some("newer title"));
        // A chat alive here is not a chat that moved away.
        let again = Package {
            transfer_id: "t-3".into(),
            ..back.clone()
        };
        assert!(matches!(
            import(&a, &again, store.path()).await,
            Err(Failure::Refused(Refusal::AlreadyHere))
        ));
    }

    #[tokio::test]
    async fn a_missing_project_is_refused_before_anything_freezes() {
        let pool = crate::testdb::fresh_pool().await;
        let not_a_repo = space_free_tempdir("nucleos-move-norepo-");
        a_chat(&pool, "c1", Some(&not_a_repo.path().to_string_lossy())).await;
        a_chat(&pool, "c2", Some("C:/this/folder/does/not/exist")).await;

        let preview = transfer::preview(&pool, "c1", false, &[]).await.unwrap();
        let gone = transfer::preview(&pool, "c2", false, &[]).await.unwrap();

        assert!(
            !preview.target_ok && preview.target_problem.is_some(),
            "{preview:?}"
        );
        assert!(!gone.target_ok, "{gone:?}");
        for chat in ["c1", "c2"] {
            assert_eq!(
                transfer::gate(&pool, chat).await,
                transfer::Gate::Open,
                "preview must not freeze"
            );
        }
        // The destination refuses a package whose project it does not have, and builds nothing.
        let destination = crate::testdb::fresh_pool().await;
        let package = Package {
            transfer_id: "t-1".into(),
            chat_id: "c9".into(),
            chat: ChatFields {
                title: None,
                brain: "cloud".into(),
                created_at: "2026-01-01T00:00:00Z".into(),
                model: None,
                effort: None,
                plan_only: false,
            },
            session: None,
            code: Some(Code {
                project_id: "nowhere".into(),
                commit: "a".repeat(40),
                branch: None,
                bundle: String::new(),
                diff: String::new(),
                files: vec![],
                left_behind: vec![],
            }),
            queue: vec![],
            parked: vec![],
            interrupted: false,
        };
        let refused = import(&destination, &package, Path::new("unused")).await;
        assert!(
            matches!(refused, Err(Failure::Problem(ref m)) if m.contains("nowhere")),
            "{refused:?}"
        );
        // Migration 0061 seeds the app's own `shell` chat in every database, so count the package's chat.
        let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM chats WHERE chat_id = 'c9'")
            .fetch_one(&destination)
            .await
            .unwrap();
        assert_eq!(rows, 0);
        assert!(!transfer::was_imported(&destination, "t-1").await.unwrap());
    }

    #[tokio::test]
    async fn the_package_never_contains_stored_secrets() {
        let (_keep, repo) = init_contained_repo("nucleos-move-secrets-");
        std::fs::write(repo.join(".gitignore"), ".env\n").unwrap();
        assert!(git_ok(&repo, &[OsStr::new("add"), OsStr::new("-A")]));
        assert!(git_ok(
            &repo,
            &[
                OsStr::new("commit"),
                OsStr::new("-m"),
                OsStr::new("ignore env")
            ]
        ));
        std::fs::write(repo.join(".env"), "TOKEN=SECRET-ENV-VALUE").unwrap();
        let store = space_free_tempdir("nucleos-move-store-");
        let pool = crate::testdb::fresh_pool().await;
        roster(&pool, "alpha", &repo).await;
        a_chat(&pool, "c1", Some(&repo.to_string_lossy())).await;
        // Everything in the chat row outside the allow-list, and the stores around it.
        sqlx::query("UPDATE chats SET system_prompt = 'SECRET-PROMPT', denied_tools = 'SECRET-DENIED', extra_dirs = 'SECRET-DIRS', agents = 'SECRET-AGENTS' WHERE chat_id = 'c1'")
            .execute(&pool)
            .await
            .unwrap();
        freeze(&pool, "c1", "t-1").await;

        let package = build(&pool, "c1", "t-1", store.path(), vec![])
            .await
            .unwrap();
        let wire = serde_json::to_value(&package).unwrap();

        let mut keys: Vec<&str> = wire
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            [
                "chat",
                "chat_id",
                "code",
                "interrupted",
                "parked",
                "queue",
                "session",
                "transfer_id"
            ]
        );
        let text = wire.to_string();
        for secret in [
            "SECRET-PROMPT",
            "SECRET-DENIED",
            "SECRET-DIRS",
            "SECRET-AGENTS",
            "SECRET-ENV-VALUE",
        ] {
            assert!(!text.contains(secret), "{secret} reached the package");
        }
        assert!(
            package
                .code
                .unwrap()
                .left_behind
                .iter()
                .any(|p| p == ".env")
        );
    }

    #[tokio::test]
    async fn a_parked_question_is_asked_again_on_the_destination() {
        let origin = crate::testdb::fresh_pool().await;
        let destination = crate::testdb::fresh_pool().await;
        let store = space_free_tempdir("nucleos-move-store-");
        a_chat(&origin, "c-parked", None).await;
        // The hook store lives in the crate above; here the parked question is a plain value.
        let asked = || {
            vec![ParkedQuestion {
                tool: "Bash".into(),
                detail: Some("rm -rf build".into()),
            }]
        };

        let preview = transfer::preview(&origin, "c-parked", true, &asked())
            .await
            .unwrap();
        assert!(
            preview
                .will_stop
                .iter()
                .any(|w| w.contains("Bash") && w.contains("asked again")),
            "{preview:?}"
        );

        // `begin` remembers the question before a cut forgets it.
        // The cut then forgets it live, so `build` is given none.
        remember("c-parked", true, asked());
        freeze(&origin, "c-parked", "t-1").await;
        let package = build(&origin, "c-parked", "t-1", store.path(), vec![])
            .await
            .unwrap();
        forget("c-parked");

        assert_eq!(package.parked.len(), 1);
        import(&destination, &package, store.path()).await.unwrap();
        let waiting = crate::chats::queued(&destination, "c-parked")
            .await
            .unwrap();
        assert_eq!(waiting.len(), 1, "the turn continues with one message");
        assert!(
            waiting[0]
                .text
                .starts_with(super::super::auto_resume::AUTO_RESUME_TEXT),
            "{}",
            waiting[0].text
        );
        assert!(
            waiting[0].text.contains("Bash") && waiting[0].text.contains("rm -rf build"),
            "{}",
            waiting[0].text
        );
    }
}
