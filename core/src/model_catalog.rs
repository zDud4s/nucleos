//! Model discovery. The pure half lives here first: product names for ids, the family and runner an
//! id implies, parsing the vendors' `/v1/models` payloads and the key-less sources (the Codex
//! CLI's own model cache, the public models.dev catalogue), grouping, and the versioned fallback
//! catalogue shown when no source can be reached.
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
pub const CATALOGUE_VERSION: &str = "2026-10-04";

const ALL: &[&str] = &["low", "medium", "high", "xhigh", "max"];
const THREE: &[&str] = &["low", "medium", "high"];
const ULTRA: &[&str] = &["low", "medium", "high", "xhigh", "max", "ultra"];

/// What the picker shows when no source at all is available (no key, no Codex cache, models.dev
/// unreachable), and what fills the ids a source omits. The last resort, not the list.
pub const FALLBACK: &[(&str, &[&str])] = &[
    ("claude-opus-5-5", ALL),
    ("claude-opus-5", ALL),
    ("claude-opus-4-6", ALL),
    ("claude-sonnet-5-5", ALL),
    ("claude-sonnet-5", ALL),
    ("claude-fable-5-1", ALL),
    ("claude-fable-5", ALL),
    ("claude-haiku-4-5", &[]),
    ("gpt-6.1-sol", ULTRA),
    ("gpt-6-astra", ULTRA),
    ("gpt-6-sol", ULTRA),
    ("gpt-6-luna", ALL),
    ("gpt-5.6-sol", ALL),
    ("gpt-5.6-terra", ULTRA),
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
        let version: Vec<&str> = rest
            .iter()
            .copied()
            .filter(|t| is_numeric_token(t))
            .collect();
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
    tokens
        .iter()
        .map(|t| title(t))
        .collect::<Vec<_>>()
        .join(" ")
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

/// The Codex CLI's own model cache (`$CODEX_HOME/models_cache.json`), which the CLI refreshes by
/// itself on subscription, no API key involved:
/// `{fetched_at, models:[{slug, visibility, supported_reasoning_levels:[{effort}], priority}]}`.
/// Only `visibility: "list"` rows the codex runner answers are kept; efforts come from the cache.
///
/// The cache carries no release dates, only `priority` (1 = shown first). It is turned into a
/// `created` stamp that sorts the same way — the cache's `fetched_at` minus the priority — so
/// `group`'s newest-first tie-break inside one version (`gpt-6-astra`, `-sol`, `-luna`) keeps the
/// CLI's own order instead of falling back to the alphabet.
pub fn parse_codex_cache(body: &str) -> Vec<Discovered> {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(body) else {
        return Vec::new();
    };
    let Some(models) = v.get("models").and_then(|m| m.as_array()) else {
        return Vec::new();
    };
    let anchor = v
        .get("fetched_at")
        .and_then(|f| f.as_str())
        .and_then(|f| chrono::DateTime::parse_from_rfc3339(f).ok())
        .map_or(0, |d| d.timestamp());
    models
        .iter()
        .filter_map(|m| {
            let id = m.get("slug")?.as_str()?;
            if m.get("visibility").and_then(|v| v.as_str()) != Some("list")
                || runner_by_id(id) != Some("codex")
            {
                return None;
            }
            let efforts: Vec<String> = m
                .get("supported_reasoning_levels")
                .and_then(|l| l.as_array())
                .map(|levels| {
                    levels
                        .iter()
                        .filter_map(|l| l.get("effort")?.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
            let efforts = (!efforts.is_empty()).then_some(efforts);
            let created = m
                .get("priority")
                .and_then(|p| p.as_i64())
                .map(|p| anchor - p);
            make(id, efforts, created)
        })
        .collect()
}

/// How many distinct versions of each Anthropic family `parse_models_dev` keeps, newest first.
const MODELS_DEV_GENERATIONS: usize = 3;

/// The public, key-less catalogue at models.dev (`{"anthropic":{"models":{id:{id, release_date,
/// status?}}}}`), used for Anthropic when no API key is set. The catalogue lists every model the
/// vendor ever shipped, so it is filtered:
/// - only `claude-*` ids the claude runner answers;
/// - nothing marked `status: "deprecated"`;
/// - a dated snapshot (`claude-opus-4-5-20251101`) is dropped when an undated alias of it
///   (`claude-opus-4-5`, `claude-opus-4-5-latest`) is listed;
/// - old generations: a major version more than one behind the newest Anthropic major anywhere
///   (with 5.x out, 3.x goes and 4.x stays), so a family that stopped shipping fades out too;
/// - per family, only the newest `MODELS_DEV_GENERATIONS` distinct versions — a family that ships
///   a new version pushes its oldest out, with no list to edit.
///
/// Labels stay `display_name(id)` (the vendor-neutral scheme every other row uses), `created` is
/// the `release_date`, and a model the built-in catalogue knows takes its efforts from there.
pub fn parse_models_dev(body: &str) -> Vec<Discovered> {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(body) else {
        return Vec::new();
    };
    let Some(models) = v
        .get("anthropic")
        .and_then(|a| a.get("models"))
        .and_then(|m| m.as_object())
    else {
        return Vec::new();
    };
    let listed: Vec<(&str, &serde_json::Value)> = models
        .iter()
        .filter_map(|(key, m)| {
            let id = m.get("id").and_then(|i| i.as_str()).unwrap_or(key);
            let deprecated = m.get("status").and_then(|s| s.as_str()) == Some("deprecated");
            (id.starts_with("claude-") && runner_by_id(id) == Some("claude") && !deprecated)
                .then_some((id, m))
        })
        .collect();
    let undated = |id: &str| {
        listed
            .iter()
            .any(|(other, _)| !is_dated(other) && normalise(other) == normalise(id))
    };
    let mut found: Vec<Discovered> = listed
        .iter()
        .filter(|(id, _)| !(is_dated(id) && undated(id)))
        .filter_map(|(id, m)| {
            let created = m
                .get("release_date")
                .and_then(|d| d.as_str())
                .and_then(|d| chrono::NaiveDate::parse_from_str(d, "%Y-%m-%d").ok())
                .and_then(|d| d.and_hms_opt(0, 0, 0))
                .map(|d| d.and_utc().timestamp());
            let efforts = FALLBACK
                .iter()
                .find(|(known, _)| known == id)
                .map(|(_, e)| e.iter().map(|l| l.to_string()).collect());
            make(id, efforts, created)
        })
        .collect();
    let major = |d: &Discovered| version_of(&d.id).first().copied();
    if let Some(newest) = found.iter().filter_map(major).max() {
        found.retain(|d| major(d).is_some_and(|m| m + 1 >= newest));
    }
    let mut versions: HashMap<String, Vec<Vec<u64>>> = HashMap::new();
    for d in &found {
        let seen = versions.entry(d.family.clone()).or_default();
        let v = version_of(&d.id);
        if !seen.contains(&v) {
            seen.push(v);
        }
    }
    for seen in versions.values_mut() {
        seen.sort_by(|a, b| b.cmp(a));
        seen.truncate(MODELS_DEV_GENERATIONS);
    }
    found.retain(|d| versions[&d.family].contains(&version_of(&d.id)));
    found.sort_by(|a, b| a.id.cmp(&b.id));
    found
}

/// Numeric version of an id, for ordering. Empty for an alias.
fn version_of(id: &str) -> Vec<u64> {
    let s = normalise(id);
    let tokens: Vec<&str> = s.split('-').filter(|t| !t.is_empty()).collect();
    let numeric = |t: &str| -> Vec<u64> {
        let digits: String = t
            .chars()
            .take_while(|c| c.is_ascii_digit() || *c == '.')
            .collect();
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
            make(
                id,
                Some(efforts.iter().map(|e| e.to_string()).collect()),
                None,
            )
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
    /// The best source any vendor came from: `live` (a vendor API, with a key), `keyless` (the
    /// Codex CLI's cache or models.dev), or `fallback` (the built-in catalogue only).
    pub source: &'static str,
    /// Per vendor (`anthropic`, `openai`): `api`, `codex-cache`, `models.dev` or `fallback`. A
    /// vendor carried over from the previous snapshot keeps the source it was learned from.
    pub sources: std::collections::BTreeMap<&'static str, &'static str>,
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
        if self.ok {
            FRESH_FOR
        } else {
            RETRY_AFTER_FAILURE
        }
    }

    fn is_fresh(&self) -> bool {
        self.at.elapsed() < self.ttl()
    }
}

const VENDORS: [&str; 2] = ["anthropic", "openai"];

fn fallback_snapshot(ok: bool) -> Snapshot {
    Snapshot {
        models: fallback(),
        source: "fallback",
        sources: VENDORS.iter().map(|v| (*v, "fallback")).collect(),
        fetched_at: chrono::Utc::now().to_rfc3339(),
        at: std::time::Instant::now(),
        ok,
    }
}

/// The key-less source of a vendor, as named in `Snapshot::sources`.
fn keyless_source(provider: &str) -> &'static str {
    if provider == "anthropic" {
        "models.dev"
    } else {
        "codex-cache"
    }
}

fn keyless_parse(provider: &str) -> fn(&str) -> Vec<Discovered> {
    if provider == "anthropic" {
        parse_models_dev
    } else {
        parse_codex_cache
    }
}

async fn no_keyless(_: &'static str) -> Option<Result<String, FetchError>> {
    None
}

/// `refresh_all` with no key-less source: only vendors with a key are asked.
pub async fn refresh_with<F, Fut>(keys: &Keys, fetch: F, previous: Option<&Snapshot>) -> Snapshot
where
    F: Fn(&'static str, String) -> Fut,
    Fut: std::future::Future<Output = Result<String, FetchError>>,
{
    refresh_all(keys, fetch, no_keyless, previous).await
}

/// Builds a snapshot vendor by vendor, each from the first source that yields models:
/// 1. the vendor API through `fetch`, when a key is set;
/// 2. the key-less source through `keyless` (`openai`: the Codex CLI's cache file; `anthropic`:
///    models.dev) — `None` means there is no such source here, `Err` that it failed;
/// 3. what `previous` learned of that vendor from a real source;
/// 4. the built-in `FALLBACK`.
///
/// Fallback ids no source listed are appended, so a source that omits one never hides it. A
/// source that failed (as opposed to being absent) makes the snapshot short-lived.
pub async fn refresh_all<F, Fut, K, KFut>(
    keys: &Keys,
    fetch: F,
    keyless: K,
    previous: Option<&Snapshot>,
) -> Snapshot
where
    F: Fn(&'static str, String) -> Fut,
    Fut: std::future::Future<Output = Result<String, FetchError>>,
    K: Fn(&'static str) -> KFut,
    KFut: std::future::Future<Output = Option<Result<String, FetchError>>>,
{
    type Parse = fn(&str) -> Vec<Discovered>;
    let vendors: [(&'static str, &Option<String>, Parse); 2] = [
        ("anthropic", &keys.anthropic, parse_anthropic),
        ("openai", &keys.openai, parse_openai),
    ];
    let mut models: Vec<Discovered> = Vec::new();
    let mut sources = std::collections::BTreeMap::new();
    let mut ok = true;
    for (provider, key, parse) in vendors {
        let mut found = Vec::new();
        let mut source = "fallback";
        if let Some(key) = key.as_ref().filter(|k| !k.is_empty()) {
            match fetch(provider, key.clone()).await {
                Ok(body) => found = parse(&body),
                Err(error) => tracing::warn!(provider, ?error, "model list fetch failed"),
            }
            if found.is_empty() {
                ok = false;
                tracing::warn!(provider, "no model list from the vendor API");
            } else {
                source = "api";
            }
        }
        if found.is_empty() {
            // An absent source (`None`) is not a failure; one that answered nothing usable is.
            let attempted = match keyless(provider).await {
                Some(Ok(body)) => {
                    found = keyless_parse(provider)(&body);
                    true
                }
                Some(Err(error)) => {
                    tracing::warn!(provider, ?error, "key-less model list failed");
                    true
                }
                None => false,
            };
            if !found.is_empty() {
                source = keyless_source(provider);
            } else if attempted {
                ok = false;
            }
        }
        if found.is_empty()
            && let Some(prev) = previous
            && let Some(prev_source) = prev.sources.get(provider).filter(|s| **s != "fallback")
        {
            ok = false;
            found = prev
                .models
                .iter()
                .filter(|d| d.provider == provider)
                .cloned()
                .collect();
            source = prev_source;
        }
        sources.insert(provider, source);
        models.extend(found);
    }
    if models.is_empty() {
        return fallback_snapshot(ok);
    }
    for d in fallback() {
        if !models.iter().any(|l| l.id == d.id) {
            models.push(d);
        }
    }
    let source = if sources.values().any(|s| *s == "api") {
        "live"
    } else if sources.values().any(|s| *s != "fallback") {
        "keyless"
    } else {
        "fallback"
    };
    Snapshot {
        models,
        source,
        sources,
        fetched_at: chrono::Utc::now().to_rfc3339(),
        at: std::time::Instant::now(),
        ok,
    }
}

/// `previous` while it is fresh, a refresh otherwise. The freshness rule `current` applies.
#[cfg(test)]
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

fn client() -> &'static reqwest::Client {
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .build()
            .unwrap_or_default()
    })
}

async fn fetch_live(provider: &'static str, key: String) -> Result<String, FetchError> {
    let request = match provider {
        "anthropic" => client()
            .get("https://api.anthropic.com/v1/models?limit=1000")
            .header("x-api-key", key)
            .header("anthropic-version", "2023-06-01"),
        _ => client()
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

/// The public, key-less catalogue. Asked with no credential of any kind.
const MODELS_DEV_URL: &str = "https://models.dev/api.json";

/// Where the Codex CLI keeps its model cache: `$CODEX_HOME/models_cache.json`, else
/// `<home>/.codex/models_cache.json`. Pure, so the order is testable.
fn codex_cache_path(
    codex_home: Option<String>,
    home: Option<String>,
) -> Option<std::path::PathBuf> {
    let base = match codex_home.filter(|h| !h.trim().is_empty()) {
        Some(h) => std::path::PathBuf::from(h),
        None => std::path::PathBuf::from(home.filter(|h| !h.trim().is_empty())?).join(".codex"),
    };
    Some(base.join("models_cache.json"))
}

async fn fetch_keyless(provider: &'static str) -> Option<Result<String, FetchError>> {
    if provider == "openai" {
        let path = codex_cache_path(
            std::env::var("CODEX_HOME").ok(),
            std::env::var("USERPROFILE")
                .ok()
                .or_else(|| std::env::var("HOME").ok()),
        )?;
        // No cache file means no Codex CLI here: an absent source, not a failure to retry.
        return match tokio::fs::read_to_string(&path).await {
            Ok(body) => Some(Ok(body)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(_) => Some(Err(FetchError::Transport)),
        };
    }
    // Deliberately a bare GET: no key, no auth header, nothing that identifies the owner.
    let response = match client().get(MODELS_DEV_URL).send().await {
        Ok(r) => r,
        Err(_) => return Some(Err(FetchError::Transport)),
    };
    let status = response.status();
    if !status.is_success() {
        return Some(Err(FetchError::Status(status.as_u16())));
    }
    Some(response.text().await.map_err(|_| FetchError::Transport))
}

static KEYLESS: std::sync::OnceLock<bool> = std::sync::OnceLock::new();

/// Turns on the key-less sources (the Codex CLI's cache, models.dev) for the life of the process.
/// `main.rs` calls it; tests never do, so a test daemon touches neither the network nor `~/.codex`.
pub fn enable_keyless() {
    let _ = KEYLESS.set(true);
}

static SNAPSHOT: std::sync::LazyLock<tokio::sync::Mutex<Option<Snapshot>>> =
    std::sync::LazyLock::new(|| tokio::sync::Mutex::new(None));

async fn refresh_process(previous: Option<&Snapshot>) -> Snapshot {
    let keys = KEYS.get().cloned().unwrap_or_default();
    if KEYLESS.get().copied().unwrap_or(false) {
        refresh_all(&keys, fetch_live, fetch_keyless, previous).await
    } else {
        refresh_with(&keys, fetch_live, previous).await
    }
}

/// Set while a background refresh runs, so a burst of requests on a stale snapshot starts one.
static REFRESHING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// The process-wide snapshot. Only the very first call waits on a refresh (and `main.rs` makes
/// that call at start-up); after that a stale snapshot is served as it is while ONE background
/// task refreshes it, so the picker never waits on models.dev or a vendor API. With no keys
/// installed and the key-less sources off it is the fallback and touches no network.
pub async fn current() -> Snapshot {
    let mut guard = SNAPSHOT.lock().await;
    let Some(snap) = guard.clone() else {
        let snap = refresh_process(None).await;
        *guard = Some(snap.clone());
        return snap;
    };
    drop(guard);
    if !snap.is_fresh() && !REFRESHING.swap(true, std::sync::atomic::Ordering::AcqRel) {
        let previous = snap.clone();
        tokio::spawn(async move {
            // The lock is not held across the network, so `cached_or_fallback` keeps answering.
            let fresh = refresh_process(Some(&previous)).await;
            *SNAPSHOT.lock().await = Some(fresh);
            REFRESHING.store(false, std::sync::atomic::Ordering::Release);
        });
    }
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

/// The vendors `latest` answers for, in the order it lists them.
pub const LATEST_VENDORS: [&str; 2] = VENDORS;

/// What the agents' `latest_models` tool answers: per vendor, its models newest first, and where
/// that vendor's list came from. Pure over a snapshot, so no network and no waiting; the caller
/// hands it `current()`, which only ever blocks on the process's very first call.
///
/// `vendor` narrows to one of `LATEST_VENDORS`, case-insensitively; anything else is an error that
/// names the valid ones, so an agent asking for `google` learns what it may ask for instead of
/// receiving an empty list it could read as "no such models exist".
///
/// Newest first is by the vendor's own `created` stamp; a model with none sorts after every
/// stamped one, and ties keep the catalogue's order, so a source that stamps nothing (the fallback)
/// still comes back in the order the catalogue lists it.
pub fn latest(snapshot: &Snapshot, vendor: Option<&str>) -> Result<serde_json::Value, String> {
    let wanted = vendor.map(str::trim).filter(|v| !v.is_empty());
    let wanted = match wanted {
        None => None,
        Some(v) => match LATEST_VENDORS.iter().find(|k| k.eq_ignore_ascii_case(v)) {
            Some(k) => Some(*k),
            None => {
                return Err(format!(
                    "unknown vendor {v:?}; use one of: {}",
                    LATEST_VENDORS.join(", ")
                ));
            }
        },
    };
    let vendors: Vec<serde_json::Value> = LATEST_VENDORS
        .iter()
        .filter(|k| wanted.is_none_or(|w| w == **k))
        .map(|k| {
            let mut models: Vec<&Discovered> = snapshot
                .models
                .iter()
                .filter(|m| m.provider == *k)
                .collect();
            // Stable, so equal stamps keep the catalogue's order.
            models.sort_by_key(|m| std::cmp::Reverse(m.created));
            serde_json::json!({
                "vendor": k,
                "source": snapshot.sources.get(k).copied().unwrap_or("fallback"),
                "models": models.iter().map(|m| serde_json::json!({
                    "id": m.id,
                    "name": if m.label.is_empty() { display_name(&m.id) } else { m.label.clone() },
                    "family": m.family,
                    "efforts": m.efforts,
                    "created": m.created,
                })).collect::<Vec<_>>(),
            })
        })
        .collect();
    Ok(serde_json::json!({
        "vendors": vendors,
        "fetched_at": snapshot.fetched_at,
        "catalogue_version": CATALOGUE_VERSION,
    }))
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

    fn found(id: &str, provider: &'static str, created: Option<i64>) -> Discovered {
        let (_, family) = family_of(id);
        Discovered {
            id: id.to_string(),
            label: display_name(id),
            provider,
            family,
            runner: if provider == "anthropic" {
                "claude"
            } else {
                "codex"
            },
            efforts: vec!["low".to_string(), "high".to_string()],
            created,
        }
    }

    #[test]
    fn latest_lists_each_vendor_newest_first_with_its_source() {
        let mut snap = fallback_snapshot(true);
        snap.models = vec![
            found("claude-opus-4-1", "anthropic", Some(100)),
            found("claude-opus-4-7", "anthropic", Some(300)),
            found("claude-sonnet-4-5", "anthropic", None),
            found("gpt-5", "openai", Some(200)),
        ];
        snap.sources.insert("anthropic", "models.dev");

        let body = latest(&snap, None).unwrap();

        let vendors = body["vendors"].as_array().unwrap();
        assert_eq!(vendors.len(), 2);
        assert_eq!(vendors[0]["vendor"], "anthropic");
        assert_eq!(vendors[0]["source"], "models.dev");
        let ids: Vec<&str> = vendors[0]["models"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| m["id"].as_str().unwrap())
            .collect();
        // Stamped newest first, the unstamped one after every stamped one.
        assert_eq!(
            ids,
            ["claude-opus-4-7", "claude-opus-4-1", "claude-sonnet-4-5"]
        );
        let first = &vendors[0]["models"][0];
        assert_eq!(first["name"], display_name("claude-opus-4-7"));
        assert_eq!(first["family"], "opus");
        assert_eq!(first["efforts"], serde_json::json!(["low", "high"]));
        assert_eq!(vendors[1]["vendor"], "openai");
        assert_eq!(vendors[1]["source"], "fallback");
        assert_eq!(body["fetched_at"], snap.fetched_at);
    }

    #[test]
    fn latest_filters_by_vendor_and_refuses_an_unknown_one() {
        let snap = fallback_snapshot(true);

        let body = latest(&snap, Some(" OpenAI ")).unwrap();
        let vendors = body["vendors"].as_array().unwrap();
        assert_eq!(vendors.len(), 1);
        assert_eq!(vendors[0]["vendor"], "openai");
        assert!(!vendors[0]["models"].as_array().unwrap().is_empty());

        // An empty filter is no filter.
        let all = latest(&snap, Some("")).unwrap();
        assert_eq!(all["vendors"].as_array().unwrap().len(), 2);

        let err = latest(&snap, Some("google")).unwrap_err();
        assert!(err.contains("anthropic") && err.contains("openai"), "{err}");
    }

    #[test]
    fn latest_over_the_fallback_serves_the_one_catalogue_and_nothing_else() {
        let snap = fallback_snapshot(true);
        let body = latest(&snap, None).unwrap();
        let served: usize = body["vendors"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v["models"].as_array().unwrap().len())
            .sum();
        assert_eq!(served, fallback().len());
        assert_eq!(body["catalogue_version"], CATALOGUE_VERSION);
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
        assert_eq!(
            calls.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "refetched"
        );
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
        assert_eq!(
            family_of("claude-opus-4-6"),
            ("anthropic", "opus".to_string())
        );
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
        assert_eq!(
            default_efforts("openai", "gpt"),
            vec!["low", "medium", "high"]
        );
        assert_eq!(
            default_efforts("openai", "o"),
            vec!["low", "medium", "high"]
        );
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
        assert_eq!(
            ids(&groups[0].models),
            vec!["opus", "claude-opus-4-6", "claude-opus-4-1"]
        );
        // Same version: `created` descending.
        assert_eq!(
            ids(&groups[3].models),
            vec!["gpt-5.6-luna", "gpt-5.6-sol", "gpt-5.5"]
        );
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
            "claude-opus-5-5",
            "claude-opus-4-6",
            "claude-sonnet-5-5",
            "claude-fable-5-1",
            "claude-sonnet-5",
            "claude-haiku-4-5",
            "gpt-6.1-sol",
            "gpt-6-astra",
            "gpt-6-sol",
            "gpt-6-luna",
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
        assert_eq!(
            grouped,
            found.len(),
            "every fallback model lands in a group"
        );
        assert!(groups.iter().all(|g| !g.models.is_empty()));
        assert_eq!(groups[0].provider, "anthropic");
        assert_eq!(groups[0].family, "opus");
        assert!(groups.iter().any(|g| g.label == "OpenAI · GPT"));
    }

    /// The shape of the Codex CLI's `models_cache.json`, trimmed to what the parser reads.
    const CODEX_CACHE: &str = r#"{
        "fetched_at": "2026-10-03T22:07:11.224534Z",
        "etag": "W/\"x\"",
        "client_version": "0.159.2",
        "identity": "abc",
        "models": [
            {"slug": "gpt-6.1-sol", "display_name": "GPT-6.1-Sol", "visibility": "list", "priority": 1,
             "supported_reasoning_levels": [{"effort": "low", "description": "a"}, {"effort": "medium"},
                {"effort": "high"}, {"effort": "xhigh"}, {"effort": "max"}, {"effort": "ultra"}]},
            {"slug": "gpt-6-luna", "display_name": "GPT-6-Luna", "visibility": "list", "priority": 4,
             "supported_reasoning_levels": [{"effort": "low"}, {"effort": "high"}]},
            {"slug": "gpt-6-astra", "display_name": "GPT-6-Astra", "visibility": "list", "priority": 2,
             "supported_reasoning_levels": [{"effort": "low"}, {"effort": "ultra"}]},
            {"slug": "gpt-6-sol", "display_name": "GPT-6-Sol", "visibility": "list", "priority": 3,
             "supported_reasoning_levels": []},
            {"slug": "gpt-reserve", "display_name": "GPT-Reserve", "visibility": "hide", "priority": 4},
            {"slug": "codex-auto-review", "display_name": "Codex Auto Review", "visibility": "hide", "priority": 43},
            {"slug": "codex-mini-latest", "display_name": "Codex Mini", "visibility": "list", "priority": 50}
        ]
    }"#;

    /// The shape of models.dev's `api.json`, trimmed: a provider map, `anthropic.models` by id.
    const MODELS_DEV: &str = r#"{
        "openai": {"id": "openai", "models": {"gpt-9": {"id": "gpt-9", "release_date": "2026-09-01"}}},
        "anthropic": {"id": "anthropic", "name": "Anthropic", "models": {
            "claude-opus-5-5": {"id": "claude-opus-5-5", "name": "Claude Opus 5.5", "release_date": "2026-09-22"},
            "claude-opus-5": {"id": "claude-opus-5", "name": "Claude Opus 5", "release_date": "2026-07-24"},
            "claude-opus-4-8": {"id": "claude-opus-4-8", "name": "Claude Opus 4.8", "release_date": "2026-05-28"},
            "claude-opus-4-7": {"id": "claude-opus-4-7", "name": "Claude Opus 4.7", "release_date": "2026-04-14"},
            "claude-opus-4-5": {"id": "claude-opus-4-5", "name": "Claude Opus 4.5 (latest)", "release_date": "2025-11-24"},
            "claude-opus-4-5-20251101": {"id": "claude-opus-4-5-20251101", "name": "Claude Opus 4.5", "release_date": "2025-11-24"},
            "claude-haiku-4-5": {"id": "claude-haiku-4-5", "name": "Claude Haiku 4.5 (latest)", "release_date": "2025-10-15"},
            "claude-haiku-4-5-20251001": {"id": "claude-haiku-4-5-20251001", "name": "Claude Haiku 4.5", "release_date": "2025-10-15"},
            "claude-3-7-sonnet-20250219": {"id": "claude-3-7-sonnet-20250219", "release_date": "2025-02-19"},
            "claude-sonnet-4-5-20250929": {"id": "claude-sonnet-4-5-20250929", "release_date": "2025-09-29"},
            "claude-3-5-haiku-latest": {"id": "claude-3-5-haiku-latest", "release_date": "2024-10-22"},
            "claude-3-5-haiku-20241022": {"id": "claude-3-5-haiku-20241022", "release_date": "2024-10-22"},
            "claude-sonnet-3-5": {"id": "claude-sonnet-3-5", "release_date": "2024-06-20", "status": "deprecated"},
            "claude-fable-5-1": {"id": "claude-fable-5-1", "name": "Claude Fable 5.1", "release_date": "2026-09-01"},
            "not-a-claude": {"id": "not-a-claude", "release_date": "2026-09-01"}
        }}
    }"#;

    fn found_ids(found: &[Discovered]) -> Vec<&str> {
        found.iter().map(|d| d.id.as_str()).collect()
    }

    #[test]
    fn parses_the_codex_cli_cache_keeping_listed_models_and_their_efforts() {
        let found = parse_codex_cache(CODEX_CACHE);
        assert_eq!(
            found_ids(&found),
            ["gpt-6.1-sol", "gpt-6-luna", "gpt-6-astra", "gpt-6-sol"],
            "hidden rows and slugs codex does not route are dropped"
        );
        for d in &found {
            assert_eq!(
                (d.provider, d.family.as_str(), d.runner),
                ("openai", "gpt", "codex")
            );
            assert_eq!(d.label, display_name(&d.id));
        }
        assert_eq!(found[0].label, "GPT-6.1 Sol");
        assert_eq!(found[2].label, "GPT-6 Astra");
        assert_eq!(found[0].efforts, ULTRA);
        assert_eq!(found[1].efforts, ["low", "high"]);
        assert_eq!(
            found[3].efforts,
            default_efforts("openai", "gpt"),
            "an empty level list falls back to the family default"
        );

        // Inside one version the CLI's priority order holds, not the alphabet.
        let created: HashMap<String, i64> = found
            .iter()
            .filter_map(|d| d.created.map(|c| (d.id.clone(), c)))
            .collect();
        let choices = found
            .iter()
            .map(|d| choice(&d.id, &d.label, d.runner, &[]))
            .collect();
        let groups = group(choices, &created);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].label, "OpenAI · GPT");
        assert_eq!(
            ids(&groups[0].models),
            ["gpt-6.1-sol", "gpt-6-astra", "gpt-6-sol", "gpt-6-luna"]
        );

        assert!(parse_codex_cache("not json").is_empty());
        assert!(parse_codex_cache(r#"{"models": 3}"#).is_empty());
    }

    #[test]
    fn parses_models_dev_keeping_current_claude_generations() {
        let found = parse_models_dev(MODELS_DEV);
        assert_eq!(
            found_ids(&found),
            [
                "claude-fable-5-1",
                "claude-haiku-4-5",
                "claude-opus-4-8",
                "claude-opus-5",
                "claude-opus-5-5",
                "claude-sonnet-4-5-20250929",
            ],
            "dated twins of a listed alias, deprecated rows, non-claude ids, other providers, \
             3.x generations and all but the newest three opus versions are dropped; a dated \
             id with no alias stays"
        );
        let fable = found.iter().find(|d| d.id == "claude-fable-5-1").unwrap();
        assert_eq!(
            fable.efforts, ALL,
            "a model the fallback knows keeps its efforts"
        );
        assert_eq!(fable.label, "Fable 5.1");
        assert_eq!(fable.runner, "claude");
        let opus = found.iter().find(|d| d.id == "claude-opus-5-5").unwrap();
        assert_eq!(
            opus.created,
            Some(
                chrono::DateTime::parse_from_rfc3339("2026-09-22T00:00:00Z")
                    .unwrap()
                    .timestamp()
            )
        );

        assert!(parse_models_dev("<html>").is_empty());
        assert!(parse_models_dev(r#"{"anthropic": {}}"#).is_empty());
    }

    #[test]
    fn the_codex_cache_path_prefers_codex_home() {
        let p = |c: Option<&str>, h: Option<&str>| {
            codex_cache_path(c.map(str::to_string), h.map(str::to_string))
        };
        assert_eq!(
            p(Some("/x/codex"), Some("/home/me")),
            Some(std::path::Path::new("/x/codex").join("models_cache.json"))
        );
        assert_eq!(
            p(Some("  "), Some("/home/me")),
            Some(
                std::path::Path::new("/home/me")
                    .join(".codex")
                    .join("models_cache.json")
            )
        );
        assert_eq!(p(None, None), None);
    }

    /// A key-less source that answers from the fixtures and counts its calls.
    fn keyless_fixtures(
        calls: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    ) -> impl Fn(&'static str) -> std::future::Ready<Option<Result<String, FetchError>>> {
        move |provider| {
            calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let body = if provider == "anthropic" {
                MODELS_DEV
            } else {
                CODEX_CACHE
            };
            std::future::ready(Some(Ok(body.to_string())))
        }
    }

    #[tokio::test]
    async fn without_keys_the_keyless_sources_are_used() {
        let fetches = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = fetches.clone();
        let snap = refresh_all(
            &Keys::default(),
            move |_, _| {
                counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                async { Err(FetchError::Transport) }
            },
            keyless_fixtures(Default::default()),
            None,
        )
        .await;
        assert_eq!(fetches.load(std::sync::atomic::Ordering::SeqCst), 0);
        assert_eq!(snap.source, "keyless");
        assert_eq!(snap.sources["anthropic"], "models.dev");
        assert_eq!(snap.sources["openai"], "codex-cache");
        assert!(snap.models.iter().any(|d| d.id == "gpt-6-astra"));
        assert!(snap.models.iter().any(|d| d.id == "claude-opus-4-8"));
        assert!(
            snap.models.iter().any(|d| d.id == "claude-opus-4-6"),
            "fallback ids no source listed stay"
        );
        assert_eq!(snap.ttl(), FRESH_FOR);
        let wire = serde_json::to_value(&snap).unwrap();
        assert_eq!(wire["sources"]["openai"], "codex-cache");
    }

    #[tokio::test]
    async fn a_key_wins_over_the_keyless_source_and_a_failed_key_falls_to_it() {
        let keyless_calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let snap = refresh_all(
            &both_keys(),
            |provider, _| async move {
                if provider == "anthropic" {
                    Ok(r#"{"data":[{"id":"claude-opus-9-9","created_at":"2026-01-01T00:00:00Z"}]}"#
                        .to_string())
                } else {
                    Err(FetchError::Status(401))
                }
            },
            keyless_fixtures(keyless_calls.clone()),
            None,
        )
        .await;
        assert_eq!(snap.source, "live");
        assert_eq!(snap.sources["anthropic"], "api");
        assert_eq!(snap.sources["openai"], "codex-cache");
        assert_eq!(
            keyless_calls.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "models.dev is not asked when the Anthropic key answered"
        );
        assert!(snap.models.iter().any(|d| d.id == "claude-opus-9-9"));
        assert!(
            !snap.models.iter().any(|d| d.id == "claude-opus-4-8"),
            "nothing from models.dev"
        );
        assert!(snap.models.iter().any(|d| d.id == "gpt-6-astra"));
        assert!(
            snap.ttl() < FRESH_FOR,
            "a failed key is retried soon even when the key-less source covered it"
        );
    }

    #[tokio::test]
    async fn a_failed_keyless_source_keeps_the_previous_snapshot_then_the_fallback() {
        let first = refresh_all(
            &Keys::default(),
            |_, _| async { Err(FetchError::Transport) },
            keyless_fixtures(Default::default()),
            None,
        )
        .await;
        let failing = |provider: &'static str| async move {
            if provider == "anthropic" {
                Some(Err(FetchError::Transport))
            } else {
                None // no Codex CLI on this machine
            }
        };
        let again = refresh_all(
            &Keys::default(),
            |_, _| async { Err(FetchError::Transport) },
            failing,
            Some(&first),
        )
        .await;
        assert_eq!(again.sources["anthropic"], "models.dev", "carried over");
        assert_eq!(again.sources["openai"], "codex-cache", "carried over");
        assert!(again.models.iter().any(|d| d.id == "claude-opus-4-8"));
        assert!(again.models.iter().any(|d| d.id == "gpt-6-astra"));
        assert!(again.ttl() < FRESH_FOR);

        let cold = refresh_all(
            &Keys::default(),
            |_, _| async { Err(FetchError::Transport) },
            failing,
            None,
        )
        .await;
        assert_eq!(cold.source, "fallback");
        assert_eq!(cold.models.len(), FALLBACK.len());
        assert!(cold.ttl() < FRESH_FOR, "models.dev failed: retried soon");

        let absent = refresh_all(
            &Keys::default(),
            |_, _| async { Err(FetchError::Transport) },
            |_| async { None },
            None,
        )
        .await;
        assert_eq!(absent.source, "fallback");
        assert_eq!(absent.ttl(), FRESH_FOR, "an absent source is not a failure");
    }
}
