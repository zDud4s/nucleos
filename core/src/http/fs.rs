//! Owner-only browsing of folders on the machine that runs the daemon (`/fs/dirs`).

use super::*;

const MAX_DIRS: usize = 2_000;
const MAX_ENTRIES: usize = 100_000;

pub(super) fn routes() -> Router<AppState> {
    Router::new()
        // Lists directories on the tower for the remote path picker.
        .route("/fs/dirs", get(get_dirs))
}

#[derive(Deserialize)]
pub(super) struct DirsQuery {
    /// An empty path is the machine root, not the daemon's working directory.
    #[serde(default)]
    path: String,
}

#[derive(Serialize, Debug, PartialEq, Eq)]
pub(super) struct DirsResponse {
    path: String,
    parent: Option<String>,
    dirs: Vec<String>,
    truncated: bool,
}

#[derive(Serialize)]
pub(super) struct DirsError {
    error: &'static str,
}

/// Lists folders without reading their contents. The handler calls this on the blocking pool because
/// `read_dir` and each metadata lookup can wait on a slow or disconnected filesystem.
fn list_dirs(requested: &str) -> Result<DirsResponse, std::io::Error> {
    let requested = requested.trim();
    if requested.is_empty() {
        #[cfg(windows)]
        return Ok(root_response_from_drives(existing_drives()));

        #[cfg(not(windows))]
        return list_directory(std::path::PathBuf::from("/"));
    }

    list_directory(std::path::PathBuf::from(requested))
}

fn list_directory(path: std::path::PathBuf) -> Result<DirsResponse, std::io::Error> {
    list_directory_with_caps(path, MAX_DIRS, MAX_ENTRIES)
}

fn list_directory_with_caps(
    path: std::path::PathBuf,
    directory_cap: usize,
    entry_cap: usize,
) -> Result<DirsResponse, std::io::Error> {
    let reader = std::fs::read_dir(&path)?;
    let mut dirs = Vec::new();
    let mut truncated = false;
    let mut entries_seen = 0;

    for entry in reader {
        if entries_seen == entry_cap {
            truncated = true;
            break;
        }
        entries_seen += 1;
        if let Ok(entry) = entry {
            // A renamed or inaccessible entry is ordinary while a directory is being browsed. It
            // is omitted instead of turning the whole listing into an error.
            if let Ok(file_type) = entry.file_type() {
                let is_directory = if file_type.is_dir() {
                    true
                } else if needs_metadata(&file_type) {
                    std::fs::metadata(entry.path()).is_ok_and(|metadata| metadata.is_dir())
                } else {
                    false
                };
                if is_directory {
                    if dirs.len() == directory_cap {
                        truncated = true;
                        break;
                    }
                    dirs.push(entry.file_name().to_string_lossy().into_owned());
                }
            }
        }
        if entries_seen == entry_cap {
            truncated = true;
            break;
        }
    }

    dirs.sort_by_cached_key(|name| name.to_lowercase());
    Ok(DirsResponse {
        path: path_string(&path),
        parent: parent_of(&path),
        dirs,
        truncated,
    })
}

fn needs_metadata(file_type: &std::fs::FileType) -> bool {
    file_type.is_symlink()
}

fn path_string(path: &std::path::Path) -> String {
    path.to_string_lossy().into_owned()
}

fn parent_of(path: &std::path::Path) -> Option<String> {
    #[cfg(windows)]
    if is_drive_root(path) {
        return Some(String::new());
    }

    path.parent().map(path_string)
}

#[cfg(windows)]
fn is_drive_root(path: &std::path::Path) -> bool {
    let mut components = path.components();
    matches!(components.next(), Some(std::path::Component::Prefix(_)))
        && matches!(components.next(), Some(std::path::Component::RootDir))
        && components.next().is_none()
}

/// The pure shaping half of the Windows root, kept separate so its contract is tested on every OS.
#[cfg(any(windows, test))]
fn root_response_from_drives(drives: impl IntoIterator<Item = String>) -> DirsResponse {
    let mut dirs: Vec<_> = drives.into_iter().collect();
    dirs.sort_by_cached_key(|name| name.to_lowercase());
    DirsResponse {
        path: String::new(),
        parent: None,
        dirs,
        truncated: false,
    }
}

#[cfg(windows)]
fn existing_drives() -> Vec<String> {
    (b'A'..=b'Z')
        .map(|letter| format!("{}:\\", char::from(letter)))
        .filter(|root| std::path::Path::new(root).is_dir())
        .collect()
}

/// Lists directories on the tower for the remote path picker.
pub(super) async fn get_dirs(
    Query(query): Query<DirsQuery>,
) -> Result<Json<DirsResponse>, (StatusCode, Json<DirsError>)> {
    let listed = tokio::task::spawn_blocking(move || list_dirs(&query.path))
        .await
        .map_err(|_| {
            directory_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "The tower could not list this folder.",
            )
        })?;

    listed.map(Json).map_err(|error| match error.kind() {
        std::io::ErrorKind::NotFound => directory_error(
            StatusCode::NOT_FOUND,
            "The requested folder does not exist.",
        ),
        std::io::ErrorKind::NotADirectory => directory_error(
            StatusCode::BAD_REQUEST,
            "The requested path is not a folder.",
        ),
        std::io::ErrorKind::PermissionDenied => directory_error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "The requested folder cannot be read.",
        ),
        _ => directory_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "The requested folder could not be read.",
        ),
    })
}

fn directory_error(status: StatusCode, error: &'static str) -> (StatusCode, Json<DirsError>) {
    (status, Json(DirsError { error }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fs_dirs_lists_only_directories() {
        let temporary = tempfile::tempdir().unwrap();
        std::fs::create_dir(temporary.path().join("alpha")).unwrap();
        std::fs::create_dir(temporary.path().join("Beta")).unwrap();
        std::fs::write(temporary.path().join("not-a-folder"), "private").unwrap();

        let listed = list_dirs(temporary.path().to_string_lossy().as_ref()).unwrap();

        assert_eq!(listed.dirs, ["alpha", "Beta"]);
        assert!(!listed.truncated);
    }

    #[test]
    fn fs_dirs_stops_at_the_cap() {
        let temporary = tempfile::tempdir().unwrap();
        std::fs::create_dir(temporary.path().join("alpha")).unwrap();
        std::fs::create_dir(temporary.path().join("beta")).unwrap();

        let listed = list_directory_with_caps(temporary.path().to_path_buf(), 1, 100).unwrap();

        assert_eq!(listed.dirs, ["alpha"]);
        assert!(listed.truncated);
    }

    #[test]
    fn fs_dirs_lists_windows_drives_at_the_root() {
        let listed = root_response_from_drives(["D:\\".to_owned(), "C:\\".to_owned()]);

        assert_eq!(listed.path, "");
        assert_eq!(listed.parent, None);
        assert_eq!(listed.dirs, ["C:\\", "D:\\"]);
        assert!(!listed.truncated);
    }
}
