//! Model discovery. The pure half lives here first: product names for ids, the family and runner an
//! id implies, parsing the vendors' `/v1/models` payloads, grouping, and the versioned fallback
//! catalogue shown when no key is set or the vendor cannot be reached.
//!
//! Naming is fixed in ONE place, on the daemon (`display_name`), because the daemon is where vendor
//! ids arrive; every client then shows the same label for the same id.

use crate::config::{AssistantChoice, EFFORT_LEVELS};
use serde::Serialize;
use std::collections::HashMap;

/// One model learned from a vendor (or the fallback catalogue).
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Discovered {
    pub id: String,
    pub label: String,
    /// `anthropic` or `openai`.
    pub provider: &'static str,
    pub family: String,
    /// `claude` or `codex`: the agent CLI that answers this id.
    pub runner: &'static str,
    pub efforts: Vec<String>,
    /// Unix seconds, when the vendor said.
    pub created: Option<i64>,
}

/// One labelled group of the picker: a provider's family, newest first.
#[derive(Debug, Clone, Serialize)]
pub struct ModelGroup {
    pub provider: String,
    pub family: String,
    /// `Claude · Opus`, `OpenAI · GPT`.
    pub label: String,
    pub models: Vec<AssistantChoice>,
}

const ALIASES: [&str; 4] = ["opus", "sonnet", "haiku", "fable"];

/// Bumped whenever `FALLBACK` is refreshed by hand.
pub const CATALOGUE_VERSION: &str = "2026-10-02";

const ALL: &[&str] = &["low", "medium", "high", "xhigh", "max"];
const THREE: &[&str] = &["low", "medium", "high"];

/// What the picker shows when no vendor list is available. Ids already known to this repo.
pub const FALLBACK: &[(&str, &[&str])] = &[
    ("claude-opus-4-6", ALL),
    ("claude-sonnet-5-5", ALL),
    ("claude-sonnet-5", ALL),
    ("claude-haiku-4-5", &[]),
    ("gpt-5.6-sol", ALL),
    ("gpt-5.6-terra", &["low", "medium", "high", "xhigh", "max", "ultra"]),
    ("gpt-5.6-luna", ALL),
    ("gpt-5.5", &["low", "medium", "high", "xhigh"]),
];

fn normalise(id: &str) -> String {
    let mut s = id.trim().to_lowercase();
    if let Some(rest) = s.strip_suffix("-latest") {
        s = rest.to_string();
    }
    if let Some((head, tail)) = s.rsplit_once('-')
        && tail.len() == 8
        && tail.bytes().all(|b| b.is_ascii_digit())
    {
        s = head.to_string();
    }
    s
}

fn is_dated(id: &str) -> bool {
    let id = id.trim().to_lowercase();
    id.rsplit_once('-')
        .is_some_and(|(_, t)| t.len() == 8 && t.bytes().all(|b| b.is_ascii_digit()))
}

fn title(token: &str) -> String {
    let mut chars = token.chars();
    match chars.next() {
        Some(c) => c.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

fn starts_with_digit(s: &str) -> bool {
    s.chars().next().is_some_and(|c| c.is_ascii_digit())
}

fn is_numeric_token(t: &str) -> bool {
    !t.is_empty() && t.chars().all(|c| c.is_ascii_digit() || c == '.')
}

fn is_o_series(token: &str) -> bool {
    let mut c = token.chars();
    c.next() == Some('o') && c.next().is_some_and(|d| d.is_ascii_digit())
}

/// The name a person reads for a vendor id. Pure; see the table in the tests.
pub fn display_name(id: &str) -> String {
    let s = normalise(id);
    let tokens: Vec<&str> = s.split('-').filter(|t| !t.is_empty()).collect();
    if tokens.is_empty() {
        return String::new();
    }
    if tokens[0] == "claude" && tokens.len() > 1 {
        let rest = &tokens[1..];
        let family = rest.iter().find(|t| !starts_with_digit(t));
        let version: Vec<&str> = rest.iter().copied().filter(|t| is_numeric_token(t)).collect();
        let extra: Vec<String> = rest
            .iter()
            .filter(|t| !is_numeric_token(t))
            .skip(1)
            .map(|t| title(t))
            .collect();
        let mut out = family.map(|f| title(f)).unwrap_or_default();
        if !version.is_empty() {
            out = format!("{out} {}", version.join(".")).trim().to_string();
        }
        for e in extra {
            out = format!("{out} {e}");
        }
        return out;
    }
    if tokens.len() == 1 && ALIASES.contains(&tokens[0]) {
        return title(tokens[0]);
    }
    if tokens[0] == "gpt" && tokens.len() > 1 && starts_with_digit(tokens[1]) {
        let mut out = format!("GPT-{}", tokens[1]);
        for t in &tokens[2..] {
            out = format!("{out} {}", title(t));
        }
        return out;
    }
    if is_o_series(tokens[0]) {
        let mut out = tokens[0].to_string();
        for t in &tokens[1..] {
            out = format!("{out} {}", title(t));
        }
        return out;
    }
    tokens.iter().map(|t| title(t)).collect::<Vec<_>>().join(" ")
}

/// `(provider, family)` an id belongs to.
pub fn family_of(id: &str) -> (&'static str, String) {
    let s = normalise(id);
    let tokens: Vec<&str> = s.split('-').filter(|t| !t.is_empty()).collect();
    match tokens.first().copied() {
        Some("claude") => {
            let fam = tokens[1..].iter().find(|t| !starts_with_digit(t)).copied();
            ("anthropic", fam.unwrap_or("claude").to_string())
        }
        Some(a) if tokens.len() == 1 && ALIASES.contains(&a) => ("anthropic", a.to_string()),
        Some("gpt") if tokens.get(1).is_some_and(|t| starts_with_digit(t)) => {
            ("openai", "gpt".to_string())
        }
        Some(t) if is_o_series(t) => ("openai", "o".to_string()),
        Some(t) => ("other", t.to_string()),
        None => ("other", String::new()),
    }
}

/// The agent CLI that answers an id, or `None` when the id says nothing about it.
pub fn runner_by_id(id: &str) -> Option<&'static str> {
    let s = id.trim().to_lowercase();
    if s.starts_with("claude-") || ALIASES.contains(&s.as_str()) {
        return Some("claude");
    }
    if s.strip_prefix("gpt-").is_some_and(starts_with_digit) || is_o_series(&s) {
        return Some("codex");
    }
    None
}

/// Effort levels a family takes when no config entry says otherwise.
pub fn default_efforts(provider: &str, family: &str) -> Vec<String> {
    let levels: &[&str] = match (provider, family) {
        ("anthropic", "opus" | "sonnet") => &EFFORT_LEVELS,
        ("anthropic", "haiku") => &[],
        ("anthropic", "fable") => THREE,
        ("openai", _) => THREE,
        _ => &[],
    };
    levels.iter().map(|l| l.to_string()).collect()
}

fn make(id: &str, efforts: Option<Vec<String>>, created: Option<i64>) -> Option<Discovered> {
    let runner = runner_by_id(id)?;
    let (provider, family) = family_of(id);
    let efforts = efforts.unwrap_or_else(|| default_efforts(provider, &family));
    Some(Discovered {
        id: id.to_string(),
        label: display_name(id),
        provider,
        family,
        runner,
        efforts,
        created,
    })
}

/// `{"data":[{"id","created_at"}]}`; only `claude-*` ids are kept. Garbage yields an empty list.
pub fn parse_anthropic(body: &str) -> Vec<Discovered> {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(body) else {
        return Vec::new();
    };
    let Some(data) = v.get("data").and_then(|d| d.as_array()) else {
        return Vec::new();
    };
    data.iter()
        .filter_map(|m| {
            let id = m.get("id")?.as_str()?;
            if !id.starts_with("claude-") {
                return None;
            }
            let created = m
                .get("created_at")
                .and_then(|c| c.as_str())
                .and_then(|c| chrono::DateTime::parse_from_rfc3339(c).ok())
                .map(|d| d.timestamp());
            make(id, None, created)
        })
        .collect()
}

/// `{"data":[{"id","created"}]}`; only chat-capable `gpt-<n>` / `o<n>` ids are kept.
pub fn parse_openai(body: &str) -> Vec<Discovered> {
    const NOT_CHAT: [&str; 8] = [
        "audio",
        "realtime",
        "tts",
        "transcribe",
        "image",
        "search",
        "embedding",
        "instruct",
    ];
    let Ok(v) = serde_json::from_str::<serde_json::Value>(body) else {
        return Vec::new();
    };
    let Some(data) = v.get("data").and_then(|d| d.as_array()) else {
        return Vec::new();
    };
    data.iter()
        .filter_map(|m| {
            let id = m.get("id")?.as_str()?;
            if runner_by_id(id) != Some("codex") || NOT_CHAT.iter().any(|w| id.contains(w)) {
                return None;
            }
            make(id, None, m.get("created").and_then(|c| c.as_i64()))
        })
        .collect()
}

/// Numeric version of an id, for ordering. Empty for an alias.
fn version_of(id: &str) -> Vec<u64> {
    let s = normalise(id);
    let tokens: Vec<&str> = s.split('-').filter(|t| !t.is_empty()).collect();
    let numeric = |t: &str| -> Vec<u64> {
        let digits: String = t.chars().take_while(|c| c.is_ascii_digit() || *c == '.').collect();
        digits.split('.').filter_map(|p| p.parse().ok()).collect()
    };
    match tokens.first().copied() {
        Some("claude") => tokens[1..]
            .iter()
            .filter(|t| is_numeric_token(t))
            .flat_map(|t| numeric(t))
            .collect(),
        Some("gpt") => tokens.get(1).map(|t| numeric(t)).unwrap_or_default(),
        Some(t) if is_o_series(t) => numeric(&t[1..]),
        _ => Vec::new(),
    }
}

fn provider_rank(p: &str) -> u8 {
    match p {
        "anthropic" => 0,
        "openai" => 1,
        _ => 2,
    }
}

fn family_rank(f: &str) -> u8 {
    ALIASES
        .iter()
        .position(|a| *a == f)
        .map_or(ALIASES.len() as u8, |i| i as u8)
}

fn group_label(provider: &str, family: &str) -> String {
    let p = match provider {
        "anthropic" => "Claude".to_string(),
        "openai" => "OpenAI".to_string(),
        other => title(other),
    };
    let f = match family {
        "gpt" => "GPT".to_string(),
        "o" => "o-series".to_string(),
        other => title(other),
    };
    format!("{p} · {f}")
}

/// Cloud choices as labelled groups: provider then family, newest first inside each. Pure.
pub fn group(choices: Vec<AssistantChoice>, created: &HashMap<String, i64>) -> Vec<ModelGroup> {
    let mut buckets: HashMap<(String, String), Vec<AssistantChoice>> = HashMap::new();
    for c in choices.into_iter().filter(|c| c.brain == "cloud") {
        let (provider, family) = family_of(&c.id);
        buckets
            .entry((provider.to_string(), family))
            .or_default()
            .push(c);
    }
    let mut groups: Vec<ModelGroup> = buckets
        .into_iter()
        .map(|((provider, family), mut models)| {
            models.sort_by(|a, b| {
                let (va, vb) = (version_of(&a.id), version_of(&b.id));
                // Aliases (no version) first, then the version descending, then `created`.
                va.is_empty()
                    .cmp(&vb.is_empty())
                    .reverse()
                    .then_with(|| vb.cmp(&va))
                    .then_with(|| created.get(&b.id).cmp(&created.get(&a.id)))
                    .then_with(|| a.id.cmp(&b.id))
            });
            let mut kept: Vec<AssistantChoice> = Vec::new();
            for m in models {
                match kept.iter().position(|k| k.label == m.label) {
                    Some(i) if is_dated(&kept[i].id) && !is_dated(&m.id) => kept[i] = m,
                    Some(_) => {}
                    None => kept.push(m),
                }
            }
            ModelGroup {
                label: group_label(&provider, &family),
                provider,
                family,
                models: kept,
            }
        })
        .collect();
    groups.sort_by(|a, b| {
        provider_rank(&a.provider)
            .cmp(&provider_rank(&b.provider))
            .then_with(|| family_rank(&a.family).cmp(&family_rank(&b.family)))
            .then_with(|| a.family.cmp(&b.family))
    });
    groups
}

/// The built-in catalogue as `Discovered` rows.
pub fn fallback() -> Vec<Discovered> {
    FALLBACK
        .iter()
        .filter_map(|(id, efforts)| {
            make(id, Some(efforts.iter().map(|e| e.to_string()).collect()), None)
        })
        .collect()
}

/// The vendor keys, handed in once by `main.rs`. Never read from the keyring here.
#[derive(Clone, Default)]
pub struct Keys {
    pub anthropic: Option<String>,
    pub openai: Option<String>,
}

// By hand, so a stray `{:?}` can never print a key.
impl std::fmt::Debug for Keys {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Keys")
            .field("anthropic", &self.anthropic.is_some())
            .field("openai", &self.openai.is_some())
            .finish()
    }
}

static KEYS: std::sync::OnceLock<Keys> = std::sync::OnceLock::new();

/// Installs the keys for the life of the process. A second call is ignored.
pub fn install_keys(keys: Keys) {
    let _ = KEYS.set(keys);
}

/// Why a vendor fetch failed. Carries NO message text: a transport error can echo a URL or header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FetchError {
    Status(u16),
    Transport,
}

/// What the last refresh learned.
#[derive(Debug, Clone, Serialize)]
pub struct Snapshot {
    pub models: Vec<Discovered>,
    /// `live` or `fallback`.
    pub source: &'static str,
    /// RFC 3339.
    pub fetched_at: String,
    #[serde(skip)]
    at: std::time::Instant,
    #[serde(skip)]
    ok: bool,
}

const FRESH_FOR: std::time::Duration = std::time::Duration::from_secs(24 * 60 * 60);
const RETRY_AFTER_FAILURE: std::time::Duration = std::time::Duration::from_secs(15 * 60);

impl Snapshot {
    /// How long this snapshot is trusted: a day when every fetch worked, minutes when one did not.
    pub fn ttl(&self) -> std::time::Duration {
        if self.ok { FRESH_FOR } else { RETRY_AFTER_FAILURE }
    }

    fn is_fresh(&self) -> bool {
        self.at.elapsed() < self.ttl()
    }
}

fn fallback_snapshot(ok: bool) -> Snapshot {
    Snapshot {
        models: fallback(),
        source: "fallback",
        fetched_at: chrono::Utc::now().to_rfc3339(),
        at: std::time::Instant::now(),
        ok,
    }
}

/// Fetches both vendors (those with a key) through `fetch` and builds a snapshot. With no keys it
/// never calls `fetch`. A failed vendor keeps what `previous` knew of it, else the fallback.
pub async fn refresh_with<F, Fut>(keys: &Keys, fetch: F, previous: Option<&Snapshot>) -> Snapshot
where
    F: Fn(&'static str, String) -> Fut,
    Fut: std::future::Future<Output = Result<String, FetchError>>,
{
    type Parse = fn(&str) -> Vec<Discovered>;
    let vendors: [(&'static str, &Option<String>, Parse); 2] = [
        ("anthropic", &keys.anthropic, parse_anthropic),
        ("openai", &keys.openai, parse_openai),
    ];
    let mut live: Vec<Discovered> = Vec::new();
    let mut asked = false;
    let mut ok = true;
    for (provider, key, parse) in vendors {
        let Some(key) = key.as_ref().filter(|k| !k.is_empty()) else {
            continue;
        };
        asked = true;
        let found = match fetch(provider, key.clone()).await {
            Ok(body) => {
                let found = parse(&body);
                if found.is_empty() {
                    tracing::warn!(provider, "model list came back empty");
                }
                found
            }
            Err(error) => {
                tracing::warn!(provider, ?error, "model list fetch failed");
                Vec::new()
            }
        };
        if found.is_empty() {
            ok = false;
            if let Some(prev) = previous.filter(|p| p.source == "live") {
                live.extend(prev.models.iter().filter(|d| d.provider == provider).cloned());
            }
        } else {
            live.extend(found);
        }
    }
    if !asked {
        return fallback_snapshot(true);
    }
    if live.is_empty() {
        return fallback_snapshot(false);
    }
    for d in fallback() {
        if !live.iter().any(|l| l.id == d.id) {
            live.push(d);
        }
    }
    Snapshot {
        models: live,
        source: "live",
        fetched_at: chrono::Utc::now().to_rfc3339(),
        at: std::time::Instant::now(),
        ok,
    }
}

/// `previous` while it is fresh, a refresh otherwise.
pub async fn ensure_with<F, Fut>(keys: &Keys, fetch: F, previous: Option<&Snapshot>) -> Snapshot
where
    F: Fn(&'static str, String) -> Fut,
    Fut: std::future::Future<Output = Result<String, FetchError>>,
{
    match previous {
        Some(p) if p.is_fresh() => p.clone(),
        _ => refresh_with(keys, fetch, previous).await,
    }
}

static CLIENT: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();

async fn fetch_live(provider: &'static str, key: String) -> Result<String, FetchError> {
    let client = CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(5))
            .build()
            .unwrap_or_default()
    });
    let request = match provider {
        "anthropic" => client
            .get("https://api.anthropic.com/v1/models?limit=1000")
            .header("x-api-key", key)
            .header("anthropic-version", "2023-06-01"),
        _ => client
            .get("https://api.openai.com/v1/models")
            .header("Authorization", format!("Bearer {key}")),
    };
    let response = request.send().await.map_err(|_| FetchError::Transport)?;
    let status = response.status();
    if !status.is_success() {
        return Err(FetchError::Status(status.as_u16()));
    }
    response.text().await.map_err(|_| FetchError::Transport)
}

static SNAPSHOT: std::sync::LazyLock<tokio::sync::Mutex<Option<Snapshot>>> =
    std::sync::LazyLock::new(|| tokio::sync::Mutex::new(None));

/// The process-wide snapshot, refreshed when stale. With no keys installed it is the fallback and
/// touches no network.
pub async fn current() -> Snapshot {
    let keys = KEYS.get().cloned().unwrap_or_default();
    let mut guard = SNAPSHOT.lock().await;
    let snap = ensure_with(&keys, fetch_live, guard.as_ref()).await;
    *guard = Some(snap.clone());
    snap
}

/// What is known right now, never touching the network or waiting on a refresh.
pub fn cached_or_fallback() -> Vec<Discovered> {
    match SNAPSHOT.try_lock() {
        Ok(guard) => guard
            .as_ref()
            .map(|s| s.models.clone())
            .unwrap_or_else(fallback),
        Err(_) => fallback(),
    }
}

/// Whether a model answered by `runner` may be offered: the daemon's own menu follows the active
/// runner; a rooted chat may use either CLI; an unrooted one the active runner, and never Codex.
pub fn admits(active_runner: &str, rooted: Option<bool>, runner: &str) -> bool {
    match rooted {
        None => runner == active_runner,
        Some(true) => true,
        Some(false) => runner == active_runner && runner != "codex",
    }
}

/// A discovered model as a picker row.
pub fn as_choice(d: &Discovered) -> AssistantChoice {
    AssistantChoice {
        id: d.id.clone(),
        label: d.label.clone(),
        brain: "cloud".to_string(),
        efforts: d.efforts.clone(),
        runner: Some(d.runner.to_string()),
        tools: None,
        installed: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{AssistantChoice, EFFORT_LEVELS};
    use std::collections::HashMap;

    fn choice(id: &str, label: &str, runner: &str, efforts: &[&str]) -> AssistantChoice {
        AssistantChoice {
            id: id.to_string(),
            label: label.to_string(),
            brain: "cloud".to_string(),
            efforts: efforts.iter().map(|e| e.to_string()).collect(),
            runner: Some(runner.to_string()),
            tools: None,
            installed: None,
        }
    }

    fn ids(models: &[AssistantChoice]) -> Vec<&str> {
        models.iter().map(|m| m.id.as_str()).collect()
    }

    const FAKE_KEY: &str = "sk-test-secret-key-0123456789";

    fn both_keys() -> Keys {
        Keys {
            anthropic: Some(FAKE_KEY.to_string()),
            openai: Some(FAKE_KEY.to_string()),
        }
    }

    #[tokio::test]
    async fn refresh_without_keys_serves_the_fallback_without_fetching() {
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = calls.clone();
        let snap = refresh_with(
            &Keys::default(),
            move |_, _| {
                counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                async { Err(FetchError::Transport) }
            },
            None,
        )
        .await;
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);
        assert_eq!(snap.source, "fallback");
        assert_eq!(snap.models.len(), FALLBACK.len());
        assert!(!snap.fetched_at.is_empty());
    }

    #[tokio::test]
    async fn a_failed_fetch_falls_back_and_never_carries_the_key() {
        let snap = refresh_with(
            &both_keys(),
            |_, _| async { Err(FetchError::Status(401)) },
            None,
        )
        .await;
        assert_eq!(snap.source, "fallback");
        assert_eq!(snap.models.len(), FALLBACK.len());
        let wire = serde_json::to_string(&snap).unwrap();
        assert!(!wire.contains(FAKE_KEY), "the key leaked into the snapshot");
        assert!(!format!("{snap:?}").contains(FAKE_KEY));
        assert!(!format!("{:?}", both_keys()).contains(FAKE_KEY));
        assert!(
            snap.ttl() < std::time::Duration::from_secs(3600),
            "a failure is retried after minutes, not a day"
        );

        // A later failure keeps what an earlier success learned.
        let ok = refresh_with(
            &both_keys(),
            |provider, _| async move {
                if provider == "anthropic" {
                    Ok(r#"{"data":[{"id":"claude-opus-9-9","created_at":"2026-01-01T00:00:00Z"}]}"#
                        .to_string())
                } else {
                    Err(FetchError::Transport)
                }
            },
            None,
        )
        .await;
        assert_eq!(ok.source, "live");
        assert!(ok.models.iter().any(|d| d.id == "claude-opus-9-9"));
        assert!(
            ok.models.iter().any(|d| d.id == "gpt-5.5"),
            "fallback ids not seen live stay"
        );
        let again = refresh_with(
            &both_keys(),
            |_, _| async { Err(FetchError::Transport) },
            Some(&ok),
        )
        .await;
        assert!(again.models.iter().any(|d| d.id == "claude-opus-9-9"));
    }

    #[tokio::test]
    async fn a_fresh_snapshot_is_reused_within_a_day() {
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let fetch = {
            let calls = calls.clone();
            move |_: &'static str, _: String| {
                calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                async {
                    Ok(r#"{"data":[{"id":"claude-opus-9-9","created_at":"2026-01-01T00:00:00Z"}]}"#
                        .to_string())
                }
            }
        };
        let keys = Keys {
            anthropic: Some(FAKE_KEY.to_string()),
            openai: None,
        };
        let first = ensure_with(&keys, &fetch, None).await;
        assert_eq!(first.source, "live");
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
        let second = ensure_with(&keys, &fetch, Some(&first)).await;
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1, "refetched");
        assert_eq!(second.fetched_at, first.fetched_at);
    }

    #[test]
    fn admits_follows_the_runner_and_root_rules() {
        // The daemon's own menu (no chat): only what the active runner can answer.
        assert!(admits("claude", None, "claude"));
        assert!(!admits("claude", None, "codex"));
        assert!(admits("codex", None, "codex"));
        // A rooted chat may use either CLI.
        assert!(admits("claude", Some(true), "codex"));
        assert!(admits("claude", Some(true), "claude"));
        // An unrooted chat: the active runner, and never Codex.
        assert!(admits("claude", Some(false), "claude"));
        assert!(!admits("claude", Some(false), "codex"));
        assert!(!admits("codex", Some(false), "codex"));
    }

    #[test]
    fn a_discovered_model_becomes_a_cloud_choice() {
        let d = fallback().into_iter().find(|d| d.id == "gpt-5.5").unwrap();
        let c = as_choice(&d);
        assert_eq!(c.brain, "cloud");
        assert_eq!(c.runner.as_deref(), Some("codex"));
        assert_eq!(c.label, "GPT-5.5");
    }

    #[test]
    fn display_names_are_product_names_never_ids() {
        let table = [
            ("claude-sonnet-5-5", "Sonnet 5.5"),
            ("claude-sonnet-5.5", "Sonnet 5.5"),
            ("claude-opus-4-1-20250805", "Opus 4.1"),
            ("claude-3-5-sonnet-20241022", "Sonnet 3.5"),
            ("claude-haiku-4-5", "Haiku 4.5"),
            ("sonnet", "Sonnet"),
            ("gpt-5.6-sol", "GPT-5.6 Sol"),
            ("gpt-5.5", "GPT-5.5"),
            ("gpt-4o-mini", "GPT-4o Mini"),
            ("o4-mini", "o4 Mini"),
        ];
        for (id, want) in table {
            assert_eq!(display_name(id), want, "display name of {id}");
        }
        // Case and surrounding whitespace do not matter, and `-latest` is dropped.
        assert_eq!(display_name("  Claude-Opus-4-6  "), "Opus 4.6");
        assert_eq!(display_name("claude-sonnet-5-5-latest"), "Sonnet 5.5");
        // The bare aliases are title-cased.
        assert_eq!(display_name("opus"), "Opus");
        assert_eq!(display_name("haiku"), "Haiku");
        assert_eq!(display_name("fable"), "Fable");
        // Anything else: tokens title-cased, numbers kept, joined by spaces.
        assert_eq!(display_name("mistral-large-2"), "Mistral Large 2");
    }

    #[test]
    fn family_and_runner_are_derived_from_the_id() {
        assert_eq!(family_of("claude-opus-4-6"), ("anthropic", "opus".to_string()));
        assert_eq!(
            family_of("claude-3-5-sonnet-20241022"),
            ("anthropic", "sonnet".to_string())
        );
        assert_eq!(family_of("haiku"), ("anthropic", "haiku".to_string()));
        assert_eq!(family_of("opus"), ("anthropic", "opus".to_string()));
        assert_eq!(family_of("fable"), ("anthropic", "fable".to_string()));
        assert_eq!(family_of("gpt-5.5"), ("openai", "gpt".to_string()));
        assert_eq!(family_of("gpt-4o-mini"), ("openai", "gpt".to_string()));
        assert_eq!(family_of("o4-mini"), ("openai", "o".to_string()));
        assert_eq!(family_of("o3"), ("openai", "o".to_string()));

        for id in ["claude-sonnet-5-5", "opus", "sonnet", "haiku", "fable"] {
            assert_eq!(runner_by_id(id), Some("claude"), "runner of {id}");
        }
        for id in ["gpt-5.6-sol", "gpt-4o-mini", "o4-mini", "o3"] {
            assert_eq!(runner_by_id(id), Some("codex"), "runner of {id}");
        }
        for id in ["llama3", "qwen3:8b", "omni-model", ""] {
            assert_eq!(runner_by_id(id), None, "runner of {id:?}");
        }

        let all: Vec<String> = EFFORT_LEVELS.iter().map(|e| e.to_string()).collect();
        assert_eq!(default_efforts("anthropic", "opus"), all);
        assert_eq!(default_efforts("anthropic", "sonnet"), all);
        assert!(default_efforts("anthropic", "haiku").is_empty());
        assert_eq!(
            default_efforts("anthropic", "fable"),
            vec!["low", "medium", "high"]
        );
        assert_eq!(default_efforts("openai", "gpt"), vec!["low", "medium", "high"]);
        assert_eq!(default_efforts("openai", "o"), vec!["low", "medium", "high"]);
    }

    #[test]
    fn parses_anthropic_models_payload() {
        let body = r#"{"data":[
            {"id":"claude-opus-4-1-20250805","created_at":"2025-08-05T00:00:00Z","display_name":"x"},
            {"id":"claude-haiku-4-5","created_at":"2025-10-01T00:00:00Z"},
            {"id":"not-a-claude-model","created_at":"2025-10-01T00:00:00Z"}
        ],"has_more":false}"#;
        let found = parse_anthropic(body);
        assert_eq!(found.len(), 2, "only claude-* ids are kept: {found:?}");

        let opus = &found[0];
        assert_eq!(opus.id, "claude-opus-4-1-20250805");
        assert_eq!(opus.label, "Opus 4.1");
        assert_eq!(opus.provider, "anthropic");
        assert_eq!(opus.family, "opus");
        assert_eq!(opus.runner, "claude");
        assert_eq!(opus.created, Some(1_754_352_000));
        let all: Vec<String> = EFFORT_LEVELS.iter().map(|e| e.to_string()).collect();
        assert_eq!(opus.efforts, all);

        let haiku = &found[1];
        assert_eq!(haiku.id, "claude-haiku-4-5");
        assert_eq!(haiku.label, "Haiku 4.5");
        assert!(haiku.efforts.is_empty(), "haiku has no dial");

        // Garbage is an empty list, never a panic.
        assert!(parse_anthropic("not json").is_empty());
        assert!(parse_anthropic(r#"{"data":"nope"}"#).is_empty());
        assert!(parse_anthropic("{}").is_empty());
    }

    #[test]
    fn parses_openai_payload_keeping_only_chat_models() {
        let body = r#"{"object":"list","data":[
            {"id":"gpt-5.5","created":1700000000,"object":"model"},
            {"id":"gpt-4o-mini","created":1690000000},
            {"id":"o4-mini","created":1710000000},
            {"id":"o3","created":1720000000},
            {"id":"gpt-4o-audio-preview","created":1},
            {"id":"gpt-4o-realtime-preview","created":1},
            {"id":"gpt-4o-mini-tts","created":1},
            {"id":"gpt-4o-transcribe","created":1},
            {"id":"gpt-image-1","created":1},
            {"id":"gpt-4o-search-preview","created":1},
            {"id":"gpt-3.5-turbo-instruct","created":1},
            {"id":"text-embedding-3-small","created":1},
            {"id":"whisper-1","created":1},
            {"id":"dall-e-3","created":1},
            {"id":"chatgpt-4o-latest","created":1}
        ]}"#;
        let found = parse_openai(body);
        let got: Vec<&str> = found.iter().map(|d| d.id.as_str()).collect();
        assert_eq!(got, vec!["gpt-5.5", "gpt-4o-mini", "o4-mini", "o3"]);

        let gpt = &found[0];
        assert_eq!(gpt.label, "GPT-5.5");
        assert_eq!(gpt.provider, "openai");
        assert_eq!(gpt.family, "gpt");
        assert_eq!(gpt.runner, "codex");
        assert_eq!(gpt.created, Some(1_700_000_000));
        assert_eq!(gpt.efforts, vec!["low", "medium", "high"]);

        let o = &found[2];
        assert_eq!(o.label, "o4 Mini");
        assert_eq!(o.family, "o");
        assert_eq!(o.runner, "codex");

        assert!(parse_openai("not json").is_empty());
        assert!(parse_openai("{}").is_empty());
    }

    #[test]
    fn groups_are_per_provider_and_family_newest_first() {
        let choices = vec![
            choice("gpt-5.5", "GPT-5.5", "codex", &["low", "medium", "high"]),
            choice("claude-haiku-4-5", "Haiku 4.5", "claude", &[]),
            choice("claude-opus-4-1-20250805", "Opus 4.1", "claude", &["low"]),
            choice("claude-opus-4-6", "Opus 4.6", "claude", &["low"]),
            choice("opus", "Opus", "claude", &["low"]),
            choice("claude-opus-4-1", "Opus 4.1", "claude", &["low"]),
            choice("claude-sonnet-5-5", "Sonnet 5.5", "claude", &["low"]),
            choice("gpt-5.6-sol", "GPT-5.6 Sol", "codex", &["low"]),
            choice("o4-mini", "o4 Mini", "codex", &["low"]),
            choice("gpt-5.6-luna", "GPT-5.6 Luna", "codex", &["low"]),
            // A local choice never reaches the cloud groups.
            AssistantChoice {
                id: "qwen3:8b".to_string(),
                label: "Qwen3 8B".to_string(),
                brain: "local".to_string(),
                efforts: vec![],
                runner: None,
                tools: None,
                installed: Some(true),
            },
        ];
        let mut created = HashMap::new();
        created.insert("gpt-5.6-sol".to_string(), 100);
        created.insert("gpt-5.6-luna".to_string(), 200);

        let groups = group(choices, &created);

        let order: Vec<(&str, &str)> = groups
            .iter()
            .map(|g| (g.provider.as_str(), g.family.as_str()))
            .collect();
        assert_eq!(
            order,
            vec![
                ("anthropic", "opus"),
                ("anthropic", "sonnet"),
                ("anthropic", "haiku"),
                ("openai", "gpt"),
                ("openai", "o"),
            ],
            "anthropic before openai, opus/sonnet/haiku first, nothing local"
        );

        assert_eq!(groups[0].label, "Claude · Opus");
        assert_eq!(groups[3].label, "OpenAI · GPT");

        // Alias first, then version descending; the dated duplicate of "Opus 4.1" folds into the
        // undated id.
        assert_eq!(ids(&groups[0].models), vec!["opus", "claude-opus-4-6", "claude-opus-4-1"]);
        // Same version: `created` descending.
        assert_eq!(ids(&groups[3].models), vec!["gpt-5.6-luna", "gpt-5.6-sol", "gpt-5.5"]);
        assert!(
            groups.iter().all(|g| !g.models.is_empty()),
            "no empty group is ever returned"
        );
        assert!(
            groups
                .iter()
                .flat_map(|g| g.models.iter())
                .all(|m| m.brain == "cloud"),
            "only cloud choices are grouped"
        );
    }

    #[test]
    fn fallback_catalogue_is_versioned_and_groups_cleanly() {
        assert!(!CATALOGUE_VERSION.is_empty());
        assert_eq!(CATALOGUE_VERSION.len(), 10, "a YYYY-MM-DD date");
        assert!(CATALOGUE_VERSION.starts_with("20"));

        let found = fallback();
        assert_eq!(found.len(), FALLBACK.len());
        for want in [
            "claude-opus-4-6",
            "claude-sonnet-5-5",
            "claude-sonnet-5",
            "claude-haiku-4-5",
            "gpt-5.6-sol",
            "gpt-5.6-terra",
            "gpt-5.6-luna",
            "gpt-5.5",
        ] {
            assert!(found.iter().any(|d| d.id == want), "fallback lacks {want}");
        }

        let mut seen = std::collections::HashSet::new();
        for d in &found {
            assert!(seen.insert(d.id.clone()), "duplicate fallback id {}", d.id);
            assert_eq!(
                Some(d.runner),
                runner_by_id(&d.id),
                "the runner of {} follows from its id",
                d.id
            );
            assert_eq!(d.label, display_name(&d.id), "label of {}", d.id);
        }

        let choices: Vec<AssistantChoice> = found
            .iter()
            .map(|d| choice(&d.id, &d.label, d.runner, &[]))
            .collect();
        let groups = group(choices, &HashMap::new());
        let grouped: usize = groups.iter().map(|g| g.models.len()).sum();
        assert_eq!(grouped, found.len(), "every fallback model lands in a group");
        assert!(groups.iter().all(|g| !g.models.is_empty()));
        assert_eq!(groups[0].provider, "anthropic");
        assert_eq!(groups[0].family, "opus");
        assert!(groups.iter().any(|g| g.label == "OpenAI · GPT"));
    }
}
