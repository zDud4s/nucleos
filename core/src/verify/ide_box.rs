//! The liveness half of the IDE verify box (spec 2026-10-05 section 4.7; F2c-1).
//!
//! An interactive session in a registered worktree can be given a limited MCP box (`verify`,
//! `verify_status`). The box beats this daemon every `BEAT_EVERY`; a worktree is live while its
//! last beat is younger than `LIVE_FOR`, so a closed session stops being live within that window.
//! Liveness is a fact about this process, so it lives in memory and a restart loses it until the
//! next beat. A beat is recorded only when the project's `ide_verify` switch is on: with it off
//! the route refuses and nothing is recorded, which is what "off means as before" rests on.

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use axum::Json;
use axum::extract::{Extension, State};
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};

use crate::auth::Scope;
use crate::state::AppState;
use crate::verify::{Caller, VerifyError, resolve_worktree};
use crate::{autopilot, git_exec};

/// How often a live box beats.
pub const BEAT_EVERY: Duration = Duration::from_secs(30);

/// How long one beat keeps a worktree live: three missed beats before it expires.
pub const LIVE_FOR: Duration = Duration::from_secs(90);

/// How long a beat may wait on git to resolve the worktree.
const RESOLVE_WITHIN: Duration = Duration::from_secs(10);

/// The last beat of each worktree, keyed by its canonical path.
#[derive(Debug, Default)]
pub struct Liveness {
    /// Worktree key -> (project, when it last beat).
    beats: HashMap<String, (String, Instant)>,
}

impl Liveness {
    /// Records a beat and forgets every worktree whose last beat has already expired.
    pub fn beat(&mut self, key: String, project: String, now: Instant) {
        self.beats
            .retain(|_, (_, at)| now.saturating_duration_since(*at) < LIVE_FOR);
        self.beats.insert(key, (project, now));
    }

    /// Whether a worktree beat less than `LIVE_FOR` before `now`. A beat stamped after `now`
    /// counts as live: the clock never runs backwards, so it is simply not older than the ttl.
    pub fn is_live(&self, key: &str, now: Instant) -> bool {
        self.beats
            .get(key)
            .is_some_and(|(_, at)| now.saturating_duration_since(*at) < LIVE_FOR)
    }
}

/// The process-wide registry the route records into.
static LIVE: LazyLock<Mutex<Liveness>> = LazyLock::new(|| Mutex::new(Liveness::default()));

/// The machine file (under the machine config root) the interactive hook reads to point an IDE
/// session at the verify box. Must equal `HINTS_FILE` in `core/hooks/ask_daemon.py`.
pub const HINTS_FILE: &str = "ide-verify-hints.json";

/// Serialises read-modify-write of the hints file between concurrent beats.
static HINTS_LOCK: Mutex<()> = Mutex::new(());

/// The hints-file key of a canonical worktree path: no verbatim `\\?\` prefix, `/` separators,
/// no trailing `/`.
pub(crate) fn hint_key(canonical: &str) -> String {
    let plain = canonical.strip_prefix(r"\\?\").unwrap_or(canonical);
    plain.replace('\\', "/").trim_end_matches('/').to_owned()
}

/// One worktree's hint: the project, when it stops counting, the worktree map's gated tools (with
/// their allowed subcommands) and its gate entrypoints.
fn hint_entry(
    project: &str,
    map: &crate::tests_map::TestsMap,
    expires_at: u64,
) -> serde_json::Value {
    let tools: serde_json::Map<String, serde_json::Value> = map
        .tests
        .tools
        .iter()
        .map(|(name, tool)| (name.clone(), serde_json::json!(tool.allow)))
        .collect();
    serde_json::json!({
        "project": project,
        "expires_at": expires_at,
        "tools": tools,
        "entrypoints": map.tests.gate_entrypoints,
    })
}

/// Sets (`Some`) or drops (`None`) the entry for `key` in `dir`/`HINTS_FILE`, pruning every entry
/// that expired at or before `now_unix`. The file is replaced through a temp file and a rename and
/// deleted when no entry is left; a malformed file is replaced.
pub(crate) fn write_hint(
    dir: &std::path::Path,
    key: &str,
    entry: Option<serde_json::Value>,
    now_unix: u64,
) -> std::io::Result<()> {
    let _guard = HINTS_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let path = dir.join(HINTS_FILE);
    let mut worktrees: serde_json::Map<String, serde_json::Value> = std::fs::read_to_string(&path)
        .ok()
        .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
        .and_then(|file| match file.get("worktrees") {
            Some(serde_json::Value::Object(found)) => Some(found.clone()),
            _ => None,
        })
        .unwrap_or_default();
    worktrees.retain(|_, value| {
        value
            .get("expires_at")
            .and_then(serde_json::Value::as_u64)
            .is_some_and(|at| at > now_unix)
    });
    match entry {
        Some(entry) => {
            worktrees.insert(key.to_owned(), entry);
        }
        None => {
            worktrees.remove(key);
        }
    }
    if worktrees.is_empty() {
        return match std::fs::remove_file(&path) {
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(error),
            _ => Ok(()),
        };
    }
    std::fs::create_dir_all(dir)?;
    let body = serde_json::json!({ "version": 1, "worktrees": worktrees });
    let tmp = dir.join(format!("{HINTS_FILE}.tmp"));
    std::fs::write(&tmp, body.to_string())?;
    std::fs::rename(&tmp, &path)
}

/// Whether a box beat for this worktree (canonical path) recently enough. The hook reads the
/// hints file instead (it cannot reach this process), so this has no caller yet.
#[allow(dead_code)] // The hook decides from the hints file, not from this registry.
pub(crate) fn is_live(key: &str) -> bool {
    LIVE.lock()
        .map(|live| live.is_live(key, Instant::now()))
        .unwrap_or(false)
}

/// What a recorded beat answers: how long it counts and how often to repeat it.
#[derive(Debug, Serialize)]
pub struct BeatAnswer {
    pub live_for_secs: u64,
    pub beat_every_secs: u64,
}

/// Why a beat was not recorded.
#[derive(Debug)]
pub(crate) enum BeatError {
    /// The project's `ide_verify` switch is off; the message names the project.
    Off(String),
    /// The worktree is not a registered worktree of a known project, or could not be resolved.
    Refused(VerifyError),
}

/// Records a beat for `worktree` at `now`: the worktree must be a registered worktree of a known
/// project, and that project's switch must be on.
pub(crate) async fn record_beat(
    pool: &sqlx::SqlitePool,
    registry: &Mutex<Liveness>,
    hints: Option<&std::path::Path>,
    worktree: &str,
    now: Instant,
) -> Result<BeatAnswer, BeatError> {
    let (root, project) = resolve_worktree(
        pool,
        Caller::Owner,
        Some(worktree),
        Instant::now() + RESOLVE_WITHIN,
    )
    .await
    .map_err(BeatError::Refused)?;
    let enabled = autopilot::ide_verify_enabled(pool, &project)
        .await
        .map_err(|error| {
            BeatError::Refused(VerifyError::Internal(format!(
                "could not read the IDE verify switch of {project}: {error}"
            )))
        })?;
    if !enabled {
        return Err(BeatError::Off(format!(
            "IDE verify is switched off for project {project}; nothing recorded"
        )));
    }
    let key = git_exec::canonical(&root)
        .await
        .map_err(|error| BeatError::Refused(VerifyError::Unprocessable(error)))?;
    // A poisoned lock only means another beat panicked mid-insert; the map is still usable.
    registry
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .beat(key.clone(), project.clone(), now);
    if let Some(dir) = hints {
        let unix_now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |since| since.as_secs());
        let dir = dir.to_path_buf();
        let hint_key = hint_key(&key);
        let written = tokio::task::spawn_blocking(move || {
            let entry = match crate::tests_map::load(&root) {
                crate::tests_map::MapState::Valid(map) => {
                    Some(hint_entry(&project, &map, unix_now + LIVE_FOR.as_secs()))
                }
                _ => None,
            };
            write_hint(&dir, &hint_key, entry, unix_now)
        })
        .await;
        match written {
            Ok(Ok(())) => {}
            Ok(Err(error)) => tracing::warn!(%error, "could not write the IDE verify hint"),
            Err(error) => tracing::warn!(%error, "the IDE verify hint task failed"),
        }
    }
    Ok(BeatAnswer {
        live_for_secs: LIVE_FOR.as_secs(),
        beat_every_secs: BEAT_EVERY.as_secs(),
    })
}

/// The body of `POST /verify/box/beat`.
#[derive(Debug, Deserialize)]
pub struct BeatArgs {
    pub worktree: String,
}

/// `POST /verify/box/beat`: a box announces that its session is still there. Control or admin key
/// only; a run key must not be able to keep a worktree alive.
pub async fn post_box_beat(
    State(state): State<AppState>,
    Extension(scope): Extension<Scope>,
    Json(args): Json<BeatArgs>,
) -> Result<Json<BeatAnswer>, (StatusCode, String)> {
    if Caller::from_scope(&scope) != Some(Caller::Owner) {
        return Err((
            StatusCode::FORBIDDEN,
            "only the owner's key can keep a verify box alive".to_owned(),
        ));
    }
    let hints = state.machine_config_root.as_deref();
    match record_beat(&state.pool, &LIVE, hints, &args.worktree, Instant::now()).await {
        Ok(answer) => Ok(Json(answer)),
        Err(BeatError::Off(message)) => Err((StatusCode::CONFLICT, message)),
        Err(BeatError::Refused(error)) => Err((error.status(), error.message().to_owned())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::SqlitePool;
    use std::path::Path;
    use std::sync::Mutex;
    use std::time::{Duration, Instant};

    /// Runs git in `dir`, asserts it succeeded, and returns its trimmed stdout.
    fn git_in(dir: &Path, args: &[&str]) -> String {
        let output = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args([
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "-c",
                "core.autocrlf=false",
            ])
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap().trim().to_owned()
    }

    /// A one-commit git repository.
    fn repo() -> tempfile::TempDir {
        let repo = tempfile::tempdir().unwrap();
        std::fs::write(repo.path().join("a.txt"), "a\n").unwrap();
        git_in(repo.path(), &["init", "-q", "-b", "main"]);
        git_in(repo.path(), &["add", "-A"]);
        git_in(repo.path(), &["commit", "-q", "-m", "one"]);
        repo
    }

    /// A pool with `repo` rostered as project `alpha`, its IDE verify switch as given.
    async fn pool_with(repo: &Path, ide_verify: bool) -> SqlitePool {
        let pool = crate::testdb::fresh_pool().await;
        sqlx::query(
            "INSERT INTO autopilot_state (project_id, mode, project_root, ide_verify) \
             VALUES ('alpha', 'active', ?, ?)",
        )
        .bind(repo.to_string_lossy().into_owned())
        .bind(i64::from(ide_verify))
        .execute(&pool)
        .await
        .unwrap();
        pool
    }

    async fn key_of(repo: &Path) -> String {
        crate::git_exec::canonical(repo).await.unwrap()
    }

    #[test]
    fn a_box_is_live_until_its_beat_is_older_than_the_ttl() {
        assert_eq!(BEAT_EVERY, Duration::from_secs(30));
        assert_eq!(LIVE_FOR, Duration::from_secs(90));

        let at = Instant::now();
        let mut live = Liveness::default();
        assert!(!live.is_live("/w/a", at), "nothing was recorded yet");

        live.beat("/w/a".to_owned(), "alpha".to_owned(), at);
        assert!(live.is_live("/w/a", at));
        assert!(live.is_live("/w/a", at + LIVE_FOR - Duration::from_secs(1)));
        assert!(
            !live.is_live("/w/a", at + LIVE_FOR),
            "a beat exactly as old as the ttl is already expired"
        );
        assert!(!live.is_live("/w/a", at + LIVE_FOR + Duration::from_secs(1)));
        assert!(!live.is_live("/w/other", at), "liveness is per worktree");

        // A later beat renews it, and an earlier-than-now clock never panics.
        live.beat(
            "/w/a".to_owned(),
            "alpha".to_owned(),
            at + Duration::from_secs(80),
        );
        assert!(live.is_live("/w/a", at + Duration::from_secs(150)));
        assert!(live.is_live("/w/a", at));
    }

    #[tokio::test]
    async fn a_beat_with_the_switch_off_records_nothing() {
        let repo = repo();
        let pool = pool_with(repo.path(), false).await;
        let registry = Mutex::new(Liveness::default());
        let now = Instant::now();

        let answer = record_beat(&pool, &registry, None, &repo.path().to_string_lossy(), now).await;

        assert!(
            matches!(answer, Err(BeatError::Off(_))),
            "a beat with the switch off must be refused as off"
        );
        let key = key_of(repo.path()).await;
        assert!(!registry.lock().unwrap().is_live(&key, now));
        assert!(
            !registry
                .lock()
                .unwrap()
                .is_live(&key, now + Duration::from_secs(1))
        );
    }

    #[tokio::test]
    async fn a_beat_with_the_switch_on_makes_the_worktree_live() {
        let repo = repo();
        let pool = pool_with(repo.path(), true).await;
        let registry = Mutex::new(Liveness::default());
        let now = Instant::now();

        let answer = record_beat(&pool, &registry, None, &repo.path().to_string_lossy(), now).await;

        let Ok(answer) = answer else {
            panic!("a beat for a registered worktree with the switch on was refused");
        };
        assert_eq!(answer.live_for_secs, 90);
        assert_eq!(answer.beat_every_secs, 30);
        let key = key_of(repo.path()).await;
        assert!(registry.lock().unwrap().is_live(&key, now));
        assert!(
            !registry.lock().unwrap().is_live(&key, now + LIVE_FOR),
            "the registry honours the ttl"
        );
    }

    #[tokio::test]
    async fn a_beat_for_a_path_outside_any_project_is_refused() {
        let rostered = repo();
        let pool = pool_with(rostered.path(), true).await;
        let registry = Mutex::new(Liveness::default());
        let now = Instant::now();

        // A real git repository the roster has never heard of, and a plain directory.
        let stranger = repo();
        let plain = tempfile::tempdir().unwrap();
        for path in [stranger.path(), plain.path()] {
            let answer = record_beat(&pool, &registry, None, &path.to_string_lossy(), now).await;
            assert!(
                matches!(answer, Err(BeatError::Refused(_))),
                "{} is outside every project and must be refused",
                path.display()
            );
            let key = crate::git_exec::canonical(path).await.unwrap();
            assert!(!registry.lock().unwrap().is_live(&key, now));
        }
        let key = key_of(rostered.path()).await;
        assert!(
            !registry.lock().unwrap().is_live(&key, now),
            "a refused beat must not make an unrelated worktree live"
        );
    }

    /// The map F2c-3 mirrors into the hints file: one gated tool with an allowed subcommand, one
    /// bare tool, one gate entrypoint.
    const HINT_MAP: &str = "version: 1\ntests:\n  groups:\n    core:\n      paths: [core/]\n      command: bash scripts/gates.sh core\n  gate_entrypoints: [scripts/gates.sh]\n  tools:\n    cargo: { allow: [fmt] }\n    tsc: {}\n";

    fn unix_now() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
    }

    fn read_hints(dir: &Path) -> serde_json::Value {
        let text = std::fs::read_to_string(dir.join(HINTS_FILE)).expect("the hints file exists");
        serde_json::from_str(&text).expect("the hints file is JSON")
    }

    #[tokio::test]
    async fn a_beat_with_the_switch_off_writes_no_hint() {
        let repo = repo();
        std::fs::write(repo.path().join("nucleos.tests.yaml"), HINT_MAP).unwrap();
        let pool = pool_with(repo.path(), false).await;
        let registry = Mutex::new(Liveness::default());
        let hints = tempfile::tempdir().unwrap();

        let answer = record_beat(
            &pool,
            &registry,
            Some(hints.path()),
            &repo.path().to_string_lossy(),
            Instant::now(),
        )
        .await;

        assert!(matches!(answer, Err(BeatError::Off(_))));
        assert!(
            !hints.path().join(HINTS_FILE).exists(),
            "a switched-off project must leave no hint behind"
        );
    }

    #[tokio::test]
    async fn a_beat_with_the_switch_on_writes_a_hint_for_its_worktree() {
        let repo = repo();
        std::fs::write(repo.path().join("nucleos.tests.yaml"), HINT_MAP).unwrap();
        let pool = pool_with(repo.path(), true).await;
        let registry = Mutex::new(Liveness::default());
        let hints = tempfile::tempdir().unwrap();

        let before = unix_now();
        let answer = record_beat(
            &pool,
            &registry,
            Some(hints.path()),
            &repo.path().to_string_lossy(),
            Instant::now(),
        )
        .await;
        let after = unix_now();

        assert!(answer.is_ok(), "the beat was refused");
        assert_eq!(HINTS_FILE, "ide-verify-hints.json");
        let file = read_hints(hints.path());
        assert_eq!(file["version"], 1);
        let key = hint_key(&key_of(repo.path()).await);
        let entry = &file["worktrees"][key.as_str()];
        assert_eq!(entry["project"], "alpha");
        assert_eq!(entry["tools"]["cargo"], serde_json::json!(["fmt"]));
        assert_eq!(entry["tools"]["tsc"], serde_json::json!([]));
        assert_eq!(
            entry["entrypoints"],
            serde_json::json!(["scripts/gates.sh"])
        );
        let expires = entry["expires_at"]
            .as_u64()
            .expect("expires_at is a number");
        assert!(
            (before + LIVE_FOR.as_secs() - 1..=after + LIVE_FOR.as_secs() + 1).contains(&expires),
            "expires_at {expires} is not the beat plus the live window"
        );
    }

    #[tokio::test]
    async fn a_beat_without_a_valid_map_drops_its_hint_and_prunes_expired_ones() {
        let repo = repo();
        let pool = pool_with(repo.path(), true).await;
        let registry = Mutex::new(Liveness::default());
        let hints = tempfile::tempdir().unwrap();
        let key = hint_key(&key_of(repo.path()).await);
        let now = unix_now();
        let live = serde_json::json!({
            "project": "beta", "expires_at": now + 1000, "tools": {}, "entrypoints": []
        });
        let stale = |at: u64| {
            serde_json::json!({
                "project": "alpha", "expires_at": at, "tools": {"cargo": []}, "entrypoints": []
            })
        };
        let seed = serde_json::json!({
            "version": 1,
            "worktrees": { "/live/other": live, "/gone/expired": stale(1), key.as_str(): stale(now + 1000) }
        });
        std::fs::write(hints.path().join(HINTS_FILE), seed.to_string()).unwrap();

        // No map in the worktree at all: its own entry goes, the expired one is pruned, the
        // foreign live one stays.
        let answer = record_beat(
            &pool,
            &registry,
            Some(hints.path()),
            &repo.path().to_string_lossy(),
            Instant::now(),
        )
        .await;
        assert!(answer.is_ok(), "a missing map must not fail the beat");
        let file = read_hints(hints.path());
        let worktrees = file["worktrees"].as_object().unwrap();
        assert!(!worktrees.contains_key(key.as_str()), "{file}");
        assert!(!worktrees.contains_key("/gone/expired"), "{file}");
        assert!(worktrees.contains_key("/live/other"), "{file}");

        // An invalid map drops the entry the same way, and an empty file is deleted.
        std::fs::write(
            repo.path().join("nucleos.tests.yaml"),
            "version: [not a map",
        )
        .unwrap();
        let alone =
            serde_json::json!({ "version": 1, "worktrees": { key.as_str(): stale(now + 1000) } });
        std::fs::write(hints.path().join(HINTS_FILE), alone.to_string()).unwrap();
        let answer = record_beat(
            &pool,
            &registry,
            Some(hints.path()),
            &repo.path().to_string_lossy(),
            Instant::now(),
        )
        .await;
        assert!(answer.is_ok());
        assert!(
            !hints.path().join(HINTS_FILE).exists(),
            "a hints file with no entry left must be deleted"
        );
    }

    #[test]
    fn hint_key_is_a_plain_forward_slash_path() {
        assert_eq!(hint_key(r"\\?\C:\Users\x\repo"), "C:/Users/x/repo");
        assert_eq!(hint_key(r"C:\w\r\"), "C:/w/r");
        assert_eq!(hint_key("/tmp/a/b/"), "/tmp/a/b");
        assert_eq!(hint_key("/tmp/a/b"), "/tmp/a/b");
    }
}
