//! The shared files area (`/files/*`).

use super::*;

pub(super) fn routes() -> Router<AppState> {
    Router::new()
        .route("/files", get(get_files).delete(delete_file))
        .route("/files/folder", post(post_files_folder))
        .route("/files/download", get(get_file_download))
        .route("/files/search", get(get_files_search))
        .route("/files/move", post(post_files_move))
        // The way back from `DELETE /files`, which moves into a trash rather than destroying. Both
        // are in no scope table in `auth.rs`, which leaves them to Admin and the control token like
        // the routes that change the folder: the list is the doorway to a restore, and a restore
        // is a write into the root.
        .route("/files/trash", get(get_files_trash))
        .route("/files/restore", post(post_files_restore))
        // Whole-body limit rather than the 2 MB default, for the same reason `/voice/capture` has
        // one: the request this route exists for is bigger than the default and would be rejected
        // identically every time. The ceiling is a memory ceiling too — see `files::MAX_UPLOAD_BYTES`.
        .route(
            "/files/upload",
            post(post_file_upload).layer(DefaultBodyLimit::max(crate::files::MAX_UPLOAD_BYTES)),
        )
}

/// The trash beside the root, or the same 503 when startup could not make it. A delete with no
/// trash refuses rather than removing for good — see `AppState::files_trash`.
pub(super) fn files_trash(state: &AppState) -> Result<&std::path::Path, StatusCode> {
    state
        .files_trash
        .as_deref()
        .ok_or(StatusCode::SERVICE_UNAVAILABLE)
}

#[derive(Deserialize)]
pub(super) struct FolderQuery {
    /// Relative to the root. Absent means the root itself.
    #[serde(default)]
    pub(super) path: String,
}

pub(super) async fn get_files(
    State(state): State<AppState>,
    Query(query): Query<FolderQuery>,
) -> Result<Json<Vec<crate::files::Entry>>, StatusCode> {
    let root = files_root(&state)?;
    crate::files::list(root, &query.path)
        .map(Json)
        .map_err(folder_status)
}

#[derive(Deserialize)]
pub(super) struct CreateFolderRequest {
    pub(super) path: String,
}

pub(super) async fn post_files_folder(
    State(state): State<AppState>,
    Json(body): Json<CreateFolderRequest>,
) -> Result<StatusCode, StatusCode> {
    let root = files_root(&state)?;
    crate::files::create_folder(root, &body.path)
        .map(|()| StatusCode::CREATED)
        .map_err(folder_status)
}

#[derive(Deserialize)]
pub(super) struct SearchQuery {
    /// Where the walk starts. Absent means the root itself.
    #[serde(default)]
    pub(super) path: String,
    pub(super) q: String,
}

/// Finds entries by name, in one folder and everything under it.
///
/// A folder this size is searched by walking it — there is no index, and building one would be a
/// second copy of the truth to keep honest. The ceilings live in `files::search`, and the answer
/// says when one of them cut in rather than passing a partial result off as the whole.
pub(super) async fn get_files_search(
    State(state): State<AppState>,
    Query(query): Query<SearchQuery>,
) -> Result<Json<crate::files::Found>, StatusCode> {
    let root = files_root(&state)?.to_path_buf();

    // Off the async runtime: a deep tree is a long blocking walk, and holding a runtime thread for
    // it would stall every other request on that thread.
    let found =
        tokio::task::spawn_blocking(move || crate::files::search(&root, &query.path, &query.q))
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    found.map(Json).map_err(folder_status)
}

/// Hands back one file's bytes, streamed rather than gathered.
///
/// `attachment`, never `inline`, and `application/octet-stream` whatever the extension says — the
/// same rule the attachment route follows, and for a stronger reason: half of what is in this
/// folder arrived as mail from a stranger, and a webview asked to render it in place would be
/// executing a file this system exists to keep at arm's length.
pub(super) async fn get_file_download(
    State(state): State<AppState>,
    Query(query): Query<FolderQuery>,
) -> Result<axum::response::Response, StatusCode> {
    let root = files_root(&state)?;
    let target = crate::files::resolve_file(root, &query.path).map_err(folder_status)?;

    // The name comes off the resolved path, not the query string: `resolve_within` has already
    // decided the last component is a name a filesystem carries, and `content_disposition` makes it
    // safe for a header again.
    let filename = target
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("file")
        .to_string();

    let file = tokio::fs::File::open(&target)
        .await
        // Gone between resolving and opening: rare, and it is the same answer as never having been
        // there.
        .map_err(|_| StatusCode::NOT_FOUND)?;
    let length = file.metadata().await.ok().map(|metadata| metadata.len());

    let mut headers = axum::http::HeaderMap::new();
    headers.insert(
        axum::http::header::CONTENT_TYPE,
        axum::http::HeaderValue::from_static("application/octet-stream"),
    );
    headers.insert(
        axum::http::header::CONTENT_DISPOSITION,
        axum::http::HeaderValue::from_str(&content_disposition(&filename))
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?,
    );
    if let Some(length) = length {
        headers.insert(axum::http::header::CONTENT_LENGTH, length.into());
    }

    let stream = tokio_util::io::ReaderStream::new(file);
    Ok((headers, axum::body::Body::from_stream(stream)).into_response())
}

#[derive(Deserialize)]
pub(super) struct UploadQuery {
    /// Which folder under the root. Absent means the root itself.
    #[serde(default)]
    pub(super) folder: String,
    /// What the browser called the file. Made safe on the way to disk, like a sender's name is.
    pub(super) filename: String,
}

/// The other way bytes enter this folder: a person picking a file of their own.
///
/// It goes through the same `write_file` as filing an attachment — same name rule, same numbered
/// collisions — because the folder's guarantee is about the folder, not about who is writing. The
/// stored name is reported back for the same reason it is there: it can differ from what was sent.
pub(super) async fn post_file_upload(
    State(state): State<AppState>,
    Query(query): Query<UploadQuery>,
    bytes: axum::body::Bytes,
) -> Result<Json<SavedFile>, StatusCode> {
    let root = files_root(&state)?.to_path_buf();

    // Blocking file I/O off the async runtime, and uncancellable for the same reason as filing: a
    // client that disconnects mid-write must not leave half a file under a name that says it is
    // whole.
    let saved = uncancellable(async move {
        tokio::task::spawn_blocking(move || {
            crate::files::write_file(&root, &query.folder, &query.filename, &bytes).map(|stored| {
                SavedFile {
                    filename: stored,
                    folder: query.folder,
                }
            })
        })
        .await
    })
    .await?
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    saved.map(Json).map_err(folder_status)
}

#[derive(Deserialize)]
pub(super) struct MoveRequest {
    pub(super) from: String,
    pub(super) to: String,
}

/// Renames or moves one entry. Both ends are resolved against the root, so neither can name a
/// destination outside it.
pub(super) async fn post_files_move(
    State(state): State<AppState>,
    Json(body): Json<MoveRequest>,
) -> Result<StatusCode, StatusCode> {
    let root = files_root(&state)?.to_path_buf();

    let moved = uncancellable(async move {
        tokio::task::spawn_blocking(move || crate::files::move_entry(&root, &body.from, &body.to))
            .await
    })
    .await?
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    moved
        .map(|()| StatusCode::NO_CONTENT)
        .map_err(folder_status)
}

#[derive(Deserialize)]
pub(super) struct DeleteQuery {
    pub(super) path: String,
    /// Required to remove a folder that still has something in it — see `files::delete`.
    #[serde(default)]
    pub(super) recursive: bool,
}

/// Moves one entry into the trash and answers with what was moved — the `id` is what an undo, or a
/// restore from the "Recently deleted" list later, sends back. 200 with a body rather than the 204
/// it was while this removed for good: the caller now has something to keep.
pub(super) async fn delete_file(
    State(state): State<AppState>,
    Query(query): Query<DeleteQuery>,
) -> Result<Json<crate::files::Trashed>, StatusCode> {
    let root = files_root(&state)?.to_path_buf();
    let trash = files_trash(&state)?.to_path_buf();

    let deleted = uncancellable(async move {
        tokio::task::spawn_blocking(move || {
            crate::files::delete(&root, &trash, &query.path, query.recursive)
        })
        .await
    })
    .await?
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    deleted.map(Json).map_err(folder_status)
}

/// What can still be brought back, newest first. Blocking work off the runtime like search: it
/// reads one small record per entry, and a trash holds up to thirty days of deletes.
pub(super) async fn get_files_trash(
    State(state): State<AppState>,
) -> Result<Json<Vec<crate::files::Trashed>>, StatusCode> {
    let trash = files_trash(&state)?.to_path_buf();
    tokio::task::spawn_blocking(move || crate::files::list_trash(&trash))
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .map(Json)
        .map_err(folder_status)
}

#[derive(Deserialize)]
pub(super) struct RestoreRequest {
    /// An id from `DELETE /files` or `GET /files/trash`, and nothing else — `files::restore`
    /// refuses anything not shaped like one before it touches a path.
    pub(super) id: String,
}

#[derive(serde::Serialize)]
pub(super) struct Restored {
    /// Where it is again, relative to the root: the recorded path, which the shell can open.
    pub(super) path: String,
}

/// Puts one trashed entry back where it was. A taken name is 409 and a folder that is no longer
/// there is 404 — both leave the entry in the trash, so the caller can make room and ask again.
/// Uncancellable like a move, for the same reason: a restore half-observed by a dropped connection
/// has still happened.
pub(super) async fn post_files_restore(
    State(state): State<AppState>,
    Json(body): Json<RestoreRequest>,
) -> Result<Json<Restored>, StatusCode> {
    let root = files_root(&state)?.to_path_buf();
    let trash = files_trash(&state)?.to_path_buf();

    let restored = uncancellable(async move {
        tokio::task::spawn_blocking(move || crate::files::restore(&root, &trash, &body.id)).await
    })
    .await?
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    restored
        .map(|path| Json(Restored { path }))
        .map_err(folder_status)
}
