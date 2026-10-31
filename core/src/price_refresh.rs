//! The daemon keeps the prices of the `models` settings document fresh itself, instead of waiting
//! for somebody to remember `scripts/refresh-models.py`: an estimated cost read off prices nobody
//! has refreshed in months is a wrong number shown with a straight face.
//!
//! [`parse`] holds the pure part: two fetched bodies in, the typed prices out, with no network, no
//! database and no clock, so what it reads can be pinned by a test. This module is the rest:
//! fetching, writing the document, and deciding when.
//!
//! Three things start a refresh:
//! - the loop ([`run_refresh_loop`]), shortly after startup and then daily (hourly after a failed
//!   attempt), when `prices_as_of` is missing or older than [`STALE_AFTER_DAYS`];
//! - the devtime page meeting a model it has no price for ([`note_unpriced`]), at most once per
//!   [`AUTO_RETRY_HOURS`];
//! - the "refresh prices" button (`POST /analytics/devtime/prices/refresh`, [`refresh_now`]), which
//!   always runs.
//!
//! The first two obey `auto_refresh_prices: false`; the button does not. Every refresh is two GETs
//! of public pages and sends nothing about the user.

pub mod parse;

use std::collections::BTreeSet;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use chrono::{DateTime, NaiveDate, Utc};
use serde::Serialize;
use sqlx::SqlitePool;

use crate::config::{ModelsConfig, PriceTable};
use crate::settings::{self, Scope};

/// The document the prices live in.
const MODELS_DOCUMENT: &str = "models";

/// Prices older than this many days are refreshed by the loop. A week, because the vendors change
/// prices with a model launch, not daily, and a daily refresh would be two fetches a day for prices
/// that almost never move.
pub const STALE_AFTER_DAYS: i64 = 7;

/// The least time between two automatic refreshes. Without it, a model that never gets a price (a
/// vendor page that is down, an id no vendor lists) would refetch on every page view.
pub const AUTO_RETRY_HOURS: i64 = 24;

/// How long after startup the loop first looks: long enough that a refresh never competes with the
/// daemon coming up, short enough that stale prices are fixed in the first minute.
const STARTUP_DELAY: Duration = Duration::from_secs(30);

/// How often the loop looks after that. Staleness is measured in days, so a daily look is enough.
const CHECK_EVERY: Duration = Duration::from_secs(24 * 3600);

/// How soon the loop looks again after a refresh it started failed. Sooner than [`CHECK_EVERY`],
/// so a vendor page down at startup (or a laptop that woke without network) does not leave stale
/// prices for another day; not so soon that a page that changed shape is fetched every minute.
const RETRY_AFTER_FAILURE: Duration = Duration::from_secs(3600);

/// Both pages are small; a vendor that takes longer than this is down, and the refresh says so.
const FETCH_TIMEOUT: Duration = Duration::from_secs(30);

/// The most of one body the refresh reads. Both pages are well under a megabyte; the cap is there
/// so a misbehaving server (or something answering in its place) cannot make the daemon buffer an
/// unbounded body, and is generous enough that the catalogue growing never trips it.
const MAX_BODY_BYTES: usize = 16 * 1024 * 1024;

/// Sent with both GETs so the vendors can tell this traffic apart. Nothing else identifies anyone.
const USER_AGENT: &str = "nucleos-price-refresh";

/// How one refresh went, as `GET /analytics/devtime` (`tokens.price_refresh`) and
/// `POST /analytics/devtime/prices/refresh` report it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PriceRefreshStatus {
    /// When the refresh ran, RFC 3339 in UTC.
    pub at: String,
    /// The prices were written, or were already exactly what the vendors publish. `false` means
    /// the document was left as it was and `problems` says why.
    pub ok: bool,
    /// The date the document's prices are now as of: the refresh's own date when `ok`, otherwise
    /// the document's existing `prices_as_of` (`None` when it had none or could not be read).
    pub prices_as_of: Option<String>,
    /// Everything worth a person's eye: the parse's warnings and problems (a model written without
    /// a price, a source it could not read) and any failure to write. Empty on a clean run.
    pub problems: Vec<String>,
}

impl PriceRefreshStatus {
    fn new(
        now: DateTime<Utc>,
        ok: bool,
        prices_as_of: Option<String>,
        problems: Vec<String>,
    ) -> Self {
        PriceRefreshStatus {
            at: now.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            ok,
            prices_as_of,
            problems,
        }
    }
}

/// Where the two bodies are fetched from. Injectable so tests point it at a loopback stub.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PriceSources {
    pub pricing_url: String,
    pub openrouter_url: String,
}

impl Default for PriceSources {
    fn default() -> Self {
        PriceSources {
            pricing_url: parse::PRICING_DOC.to_string(),
            openrouter_url: parse::OPENROUTER_MODELS.to_string(),
        }
    }
}

/// One GET; any failure (transport, non-2xx, unreadable body, a body over [`MAX_BODY_BYTES`]) is
/// `None`, which [`parse::refresh_from_bodies`] turns into "could not read …" and a document left
/// untouched.
async fn fetch(client: &reqwest::Client, url: &str) -> Option<String> {
    fetch_at_most(client, url, MAX_BODY_BYTES).await
}

/// [`fetch`] with the body cap as a parameter, so a test can trip it without serving megabytes.
///
/// A declared length over the cap is refused before any of the body is read; a body without one
/// (chunked) is read chunk by chunk and dropped as soon as it passes the cap. Decoded as `text()`
/// decodes without the `charset` feature: UTF-8, malformed sequences replaced.
async fn fetch_at_most(client: &reqwest::Client, url: &str, max_bytes: usize) -> Option<String> {
    let mut response = match client
        .get(url)
        .header(reqwest::header::USER_AGENT, USER_AGENT)
        .send()
        .await
    {
        Ok(response) => response,
        Err(error) => {
            tracing::warn!(%error, url, "price refresh: fetch failed");
            return None;
        }
    };
    let status = response.status();
    if !status.is_success() {
        tracing::warn!(%status, url, "price refresh: fetch answered with an error status");
        return None;
    }
    let max_length = u64::try_from(max_bytes).unwrap_or(u64::MAX);
    if let Some(length) = response.content_length()
        && length > max_length
    {
        tracing::warn!(
            length,
            max_bytes,
            url,
            "price refresh: the body is too large"
        );
        return None;
    }
    let mut body: Vec<u8> = Vec::new();
    loop {
        match response.chunk().await {
            Ok(Some(chunk)) => {
                if body.len() + chunk.len() > max_bytes {
                    tracing::warn!(max_bytes, url, "price refresh: the body is too large");
                    return None;
                }
                body.extend_from_slice(&chunk);
            }
            Ok(None) => break,
            Err(error) => {
                tracing::warn!(%error, url, "price refresh: could not read the body");
                return None;
            }
        }
    }
    Some(String::from_utf8_lossy(&body).into_owned())
}

/// The `models` document as it is now, or the problem line that says why there is none to refresh.
fn models_document() -> Result<Arc<ModelsConfig>, String> {
    let snapshot = settings::global().snapshot();
    if let Some(unreadable) = snapshot.unreadable_for(&Scope::Machine, MODELS_DOCUMENT) {
        return Err(format!(
            "the models document is unreadable ({}) — prices unchanged",
            unreadable.reason
        ));
    }
    snapshot
        .get_opt::<ModelsConfig>(&Scope::Machine)
        .ok_or_else(|| "no models document — prices unchanged".to_string())
}

/// Refreshes `prices_as_of` and `model_prices` of the `models` document from `sources`, dated
/// `now`'s UTC day.
///
/// A missing document is not created: `claude_model` and `codex_model` are the owner's choice, and a
/// document holding only prices would invent them. That is checked before any fetch, so a machine
/// without the document costs no network. An unreadable one is left for the owner to repair.
///
/// The change goes through [`settings::update`], which applies it to the document as it is at the
/// write and not as it was before the fetch: a fetch can take up to [`FETCH_TIMEOUT`], and an edit
/// made meanwhile is kept. Prices identical to the stored ones write nothing, so a refresh that
/// finds no news adds no version to the document's history.
pub async fn refresh_document(
    pool: &SqlitePool,
    client: &reqwest::Client,
    sources: &PriceSources,
    now: DateTime<Utc>,
) -> PriceRefreshStatus {
    let before = match models_document() {
        Ok(config) => config,
        Err(problem) => return PriceRefreshStatus::new(now, false, None, vec![problem]),
    };
    let (pricing, catalogue) = tokio::join!(
        fetch(client, &sources.pricing_url),
        fetch(client, &sources.openrouter_url)
    );
    let today = now.date_naive().to_string();
    let (refreshed, mut problems) =
        parse::refresh_from_bodies(pricing.as_deref(), catalogue.as_deref(), &today);
    let Some(refreshed) = refreshed else {
        return PriceRefreshStatus::new(now, false, before.prices_as_of.clone(), problems);
    };

    let current = match models_document() {
        Ok(config) => config,
        Err(problem) => {
            problems.push(problem);
            return PriceRefreshStatus::new(now, false, None, problems);
        }
    };
    if current.prices_as_of.as_deref() == Some(refreshed.prices_as_of.as_str())
        && current.model_prices.entries == refreshed.model_prices
    {
        return PriceRefreshStatus::new(now, true, Some(today), problems);
    }
    let written = settings::update::<ModelsConfig, _>(
        pool,
        &Scope::Machine,
        "price-refresh",
        move |config| {
            config.prices_as_of = Some(refreshed.prices_as_of);
            config.model_prices = PriceTable {
                entries: refreshed.model_prices,
                dropped: Vec::new(),
            };
        },
    )
    .await;
    if let Err(error) = written {
        problems.push(format!(
            "could not write the models document ({error}) — prices unchanged"
        ));
        return PriceRefreshStatus::new(now, false, current.prices_as_of.clone(), problems);
    }
    PriceRefreshStatus::new(now, true, Some(today), problems)
}

/// Missing or unreadable is stale (there is no age to trust), and so is anything more than
/// [`STALE_AFTER_DAYS`] before `today`. A date in the future is not: a clock that moved is not a
/// reason to refetch.
fn is_stale(prices_as_of: Option<&str>, today: NaiveDate) -> bool {
    let parsed =
        prices_as_of.and_then(|text| NaiveDate::parse_from_str(text.trim(), "%Y-%m-%d").ok());
    match parsed {
        Some(as_of) => (today - as_of).num_days() > STALE_AFTER_DAYS,
        None => true,
    }
}

/// The loop's decision for one tick: auto refresh on, and the prices stale.
fn tick_wants_refresh(config: &ModelsConfig, today: NaiveDate) -> bool {
    config.auto_refresh_prices && is_stale(config.prices_as_of.as_deref(), today)
}

/// Whether the devtime page's unpriced models should start a background refresh.
///
/// Only for an id not in `known`, the ids that were unpriced when a triggered refresh then ran
/// clean. And only when no automatic refresh started in the last [`AUTO_RETRY_HOURS`], so a vendor
/// page that is down is not hit by every page view.
///
/// So an id whose refresh FAILED is not known and is retried once [`AUTO_RETRY_HOURS`] have passed,
/// while an id still unpriced after a SUCCESSFUL refresh is never retried by this trigger. That is
/// a deliberate trade-off: an alias such as `sonnet`, or an id no vendor lists, never gets a price,
/// and retrying it would restart a refresh every [`AUTO_RETRY_HOURS`] forever; a price a vendor
/// publishes for it later still arrives with the loop's refresh once the prices are
/// [`STALE_AFTER_DAYS`] old.
///
/// `known` only grows (see [`RefreshState::known_unpriced`]): the page's views (global or one
/// project, 7, 30 or 90 days) each have their own unpriced list, and a set replaced by each trigger
/// would let two views take turns retriggering one another every day.
fn should_trigger(
    unpriced: &BTreeSet<String>,
    auto_enabled: bool,
    known: &BTreeSet<String>,
    last_auto_attempt: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
) -> bool {
    auto_enabled
        && unpriced.iter().any(|id| !known.contains(id))
        && last_auto_attempt.is_none_or(|at| now - at >= chrono::Duration::hours(AUTO_RETRY_HOURS))
}

/// What the refresher remembers between refreshes. In memory only: a restarted daemon starts with
/// no status and may refresh once more, which costs two GETs.
#[derive(Debug, Default)]
struct RefreshState {
    last: Option<PriceRefreshStatus>,
    last_auto_attempt: Option<DateTime<Utc>>,
    /// Every id passed to a trigger whose refresh then ran clean, accumulated (never replaced; see
    /// [`should_trigger`]). Added after the refresh rather than when the trigger fires, so a failed
    /// refresh leaves its ids to be retried. Not recomputed from the refreshed prices: an id the
    /// refresh priced drops out of the page's list by itself, and an id it could not price (an
    /// alias) stays here and does not trigger again. A refresh started by the loop or the button
    /// leaves it alone, since it knows no unpriced list.
    known_unpriced: BTreeSet<String>,
}

/// The price refresh of the `models` document: its pool, its sources, its client, and its state.
pub struct Refresher {
    pool: SqlitePool,
    sources: PriceSources,
    client: reqwest::Client,
    /// Serialises refreshes, so two never fetch and write at once. The button waits on it; an
    /// automatic refresh that finds it held skips, since the refresh running is doing its job.
    running: tokio::sync::Mutex<()>,
    state: std::sync::Mutex<RefreshState>,
}

impl Refresher {
    pub fn new(pool: SqlitePool, sources: PriceSources, client: reqwest::Client) -> Self {
        Refresher {
            pool,
            sources,
            client,
            running: tokio::sync::Mutex::new(()),
            state: std::sync::Mutex::new(RefreshState::default()),
        }
    }

    /// The daemon's refresher: the real vendor URLs, and a client with [`FETCH_TIMEOUT`].
    pub fn for_daemon(pool: SqlitePool) -> Self {
        // `expect` for the reason `ConfiguredAssistants::new` gives: the causes are TLS backend
        // and proxy environment problems, i.e. startup misconfiguration.
        let client = reqwest::Client::builder()
            .timeout(FETCH_TIMEOUT)
            .build()
            .expect("HTTP client for the price refresh (check TLS and proxy environment)");
        Refresher::new(pool, PriceSources::default(), client)
    }

    /// A poisoned lock only means a panic elsewhere while it was held; the state is plain data and
    /// still the best answer there is.
    fn lock_state(&self) -> std::sync::MutexGuard<'_, RefreshState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub fn last(&self) -> Option<PriceRefreshStatus> {
        self.lock_state().last.clone()
    }

    /// One refresh, the running lock already held by the caller; logs and records its status.
    async fn run(&self, now: DateTime<Utc>) -> PriceRefreshStatus {
        let status = refresh_document(&self.pool, &self.client, &self.sources, now).await;
        if status.ok {
            tracing::info!(
                prices_as_of = ?status.prices_as_of,
                problems = status.problems.len(),
                "price refresh: done"
            );
        } else {
            tracing::warn!(problems = ?status.problems, "price refresh: prices unchanged");
        }
        self.lock_state().last = Some(status.clone());
        status
    }

    /// The button: always runs, whatever `auto_refresh_prices` says and however recent the last
    /// attempt, after waiting for a refresh already running to finish.
    pub async fn refresh_now(&self, now: DateTime<Utc>) -> PriceRefreshStatus {
        let _running = self.running.lock().await;
        self.run(now).await
    }

    /// An automatic refresh: skipped (`None`) when one is already running.
    async fn refresh_auto(&self, now: DateTime<Utc>) -> Option<PriceRefreshStatus> {
        let _running = self.running.try_lock().ok()?;
        Some(self.run(now).await)
    }

    /// One tick of the loop: refreshes when [`tick_wants_refresh`] says so, else `None`.
    pub async fn auto_tick(
        &self,
        config: &ModelsConfig,
        now: DateTime<Utc>,
    ) -> Option<PriceRefreshStatus> {
        if !tick_wants_refresh(config, now.date_naive()) {
            return None;
        }
        self.lock_state().last_auto_attempt = Some(now);
        self.refresh_auto(now).await
    }

    /// The devtime page's unpriced models: starts a background refresh when [`should_trigger`]
    /// says so, and returns its handle (`None` when it did not start one). The page never waits
    /// for it: it shows this view now, and the next one reads the new prices, a refresh publishing
    /// them to the settings snapshot each request reads. Tests await the handle.
    pub fn note_unpriced(
        self: &Arc<Self>,
        unpriced: &[String],
        auto_enabled: bool,
        now: DateTime<Utc>,
    ) -> Option<tokio::task::JoinHandle<()>> {
        let seen: BTreeSet<String> = unpriced.iter().cloned().collect();
        {
            let mut state = self.lock_state();
            if !should_trigger(
                &seen,
                auto_enabled,
                &state.known_unpriced,
                state.last_auto_attempt,
                now,
            ) {
                return None;
            }
            // Recorded before the spawn, so a request arriving meanwhile does not start a second.
            state.last_auto_attempt = Some(now);
        }
        let refresher = Arc::clone(self);
        Some(tokio::spawn(async move {
            // Known only after a clean run: a failed one (or one skipped because another refresh
            // held the lock) leaves the ids to trigger again after the hold.
            if let Some(status) = refresher.refresh_auto(now).await
                && status.ok
            {
                refresher.lock_state().known_unpriced.extend(seen);
            }
        }))
    }
}

/// The daemon's refresher. A global for the reason `verify::install` gives: `AppState` is built
/// literally in dozens of places, and only two handlers and one loop need this.
static REFRESHER: OnceLock<Arc<Refresher>> = OnceLock::new();

/// Called once from `main`. A second call changes nothing.
pub fn install(refresher: Arc<Refresher>) {
    if REFRESHER.set(refresher).is_err() {
        tracing::warn!("price refresher installed twice; keeping the first");
    }
}

/// `None` until `main` installs one, so never under tests.
pub fn installed() -> Option<Arc<Refresher>> {
    REFRESHER.get().cloned()
}

/// The last refresh since the daemon started, or `None` when none has run.
pub fn last_status() -> Option<PriceRefreshStatus> {
    installed().and_then(|refresher| refresher.last())
}

/// The button. Without a refresher there is nothing to refresh, which is a status, not an error.
pub async fn refresh_now(now: DateTime<Utc>) -> PriceRefreshStatus {
    match installed() {
        Some(refresher) => refresher.refresh_now(now).await,
        None => PriceRefreshStatus::new(
            now,
            false,
            None,
            vec!["the daemon has no price refresher".to_string()],
        ),
    }
}

/// See [`Refresher::note_unpriced`]; a no-op without a refresher.
pub fn note_unpriced(unpriced: &[String], auto_enabled: bool, now: DateTime<Utc>) {
    if let Some(refresher) = installed() {
        // Detached: the page does not wait for the refresh, and the handle is only for tests.
        let _ = refresher.note_unpriced(unpriced, auto_enabled, now);
    }
}

/// How long the loop sleeps after a tick: [`RETRY_AFTER_FAILURE`] when the tick ran a refresh and
/// it failed, [`CHECK_EVERY`] otherwise (a refresh that worked, prices still fresh, auto refresh
/// off, or a refresh skipped because another one was running and will report for itself).
fn sleep_after_tick(tick: Option<&PriceRefreshStatus>) -> Duration {
    match tick {
        Some(status) if !status.ok => RETRY_AFTER_FAILURE,
        _ => CHECK_EVERY,
    }
}

/// The background refresh: a first look after [`STARTUP_DELAY`], then one every [`CHECK_EVERY`]
/// (or [`RETRY_AFTER_FAILURE`] after a failed one, [`sleep_after_tick`]), each reading the `models`
/// document as it is then, so `auto_refresh_prices: false` takes effect without a restart.
pub async fn run_refresh_loop(refresher: Arc<Refresher>) {
    tokio::time::sleep(STARTUP_DELAY).await;
    loop {
        let config = crate::config::models_config_now();
        let tick = refresher.auto_tick(&config, Utc::now()).await;
        tokio::time::sleep(sleep_after_tick(tick.as_ref())).await;
    }
}

#[cfg(test)]
mod tests {
    use super::parse::tests::{GOOD, PRICING_MD};
    use super::*;
    use crate::project::rules::testing::Home;
    use chrono::TimeZone;

    fn day(text: &str) -> NaiveDate {
        NaiveDate::parse_from_str(text, "%Y-%m-%d").unwrap()
    }

    fn at(text: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(text)
            .unwrap()
            .with_timezone(&Utc)
    }

    fn ids(list: &[&str]) -> BTreeSet<String> {
        list.iter().map(|id| id.to_string()).collect()
    }

    /// A `models` document with an unrelated block and old prices, at version 1. The `Home` keeps
    /// the settings handle the test wrote it to.
    async fn priced_document() -> (Home, SqlitePool) {
        let home = Home::new();
        let pool = crate::testdb::fresh_pool().await;
        settings::put(
            &pool,
            &Scope::Machine,
            MODELS_DOCUMENT,
            &serde_json::json!({
                "claude_model": "opus",
                "codex_model": "gpt-a",
                "assistant_choices": [{ "id": "opus", "label": "Opus", "brain": "cloud" }],
                "prices_as_of": "2026-01-01",
                "model_prices": { "old": { "input": 1.0 } },
            }),
            "test",
            None,
        )
        .await
        .expect("models is a registered document");
        (home, pool)
    }

    fn stored_version() -> Option<i64> {
        settings::global()
            .snapshot()
            .version(&Scope::Machine, MODELS_DOCUMENT)
    }

    /// A loopback stand-in for both vendors, `/pricing.md` and `/models`, each answering with the
    /// given status and body.
    async fn stub_vendors(
        pricing: (u16, &'static str),
        catalogue: (u16, &'static str),
    ) -> PriceSources {
        let reply = |(status, body): (u16, &'static str)| {
            move || async move { (axum::http::StatusCode::from_u16(status).unwrap(), body) }
        };
        let app = axum::Router::new()
            .route("/pricing.md", axum::routing::get(reply(pricing)))
            .route("/models", axum::routing::get(reply(catalogue)));
        let base = serve(app).await;
        PriceSources {
            pricing_url: format!("{base}/pricing.md"),
            openrouter_url: format!("{base}/models"),
        }
    }

    /// Serves `app` on a loopback port for the rest of the test; returns its `http://` base.
    async fn serve(app: axum::Router) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        format!("http://{address}")
    }

    /// Sources nothing listens at, so a test that should not reach the network cannot reach a real
    /// vendor either: a fetch there fails at once with "could not read".
    fn dead_sources() -> PriceSources {
        PriceSources {
            pricing_url: "http://127.0.0.1:9/pricing.md".to_string(),
            openrouter_url: "http://127.0.0.1:9/models".to_string(),
        }
    }

    // --- staleness -------------------------------------------------------------------

    #[test]
    fn missing_prices_are_stale() {
        assert!(is_stale(None, day("2026-10-19")));
    }

    #[test]
    fn an_unparseable_date_is_stale() {
        assert!(is_stale(Some("last tuesday"), day("2026-10-19")));
    }

    #[test]
    fn prices_seven_days_old_are_not_stale_and_eight_days_old_are() {
        assert!(!is_stale(Some("2026-10-12"), day("2026-10-19")));
        assert!(is_stale(Some("2026-10-11"), day("2026-10-19")));
    }

    #[test]
    fn the_loop_refreshes_stale_prices_only_while_auto_refresh_is_on() {
        let today = day("2026-10-19");
        let mut config = ModelsConfig::default();
        assert!(
            config.auto_refresh_prices,
            "on unless the document says otherwise"
        );
        assert!(tick_wants_refresh(&config, today));
        config.prices_as_of = Some("2026-10-18".to_string());
        assert!(!tick_wants_refresh(&config, today));
        config.prices_as_of = None;
        config.auto_refresh_prices = false;
        assert!(!tick_wants_refresh(&config, today));
    }

    #[test]
    fn auto_refresh_prices_false_in_the_file_is_read_and_absent_means_true() {
        let on = crate::config::parse_models_config("claude_model: a\ncodex_model: b\n").unwrap();
        assert!(on.auto_refresh_prices);
        let off = crate::config::parse_models_config(
            "claude_model: a\ncodex_model: b\nauto_refresh_prices: false\n",
        )
        .unwrap();
        assert!(!off.auto_refresh_prices);
    }

    // --- the unpriced-model trigger --------------------------------------------------

    #[test]
    fn a_new_unpriced_model_triggers_a_refresh() {
        let now = at("2026-10-19T12:00:00Z");
        assert!(should_trigger(
            &ids(&["claude-new"]),
            true,
            &ids(&[]),
            None,
            now
        ));
        assert!(should_trigger(
            &ids(&["sonnet", "claude-new"]),
            true,
            &ids(&["sonnet"]),
            None,
            now
        ));
    }

    #[test]
    fn the_same_unpriced_models_as_last_time_do_not_trigger() {
        let now = at("2026-10-19T12:00:00Z");
        assert!(!should_trigger(
            &ids(&["sonnet"]),
            true,
            &ids(&["sonnet"]),
            None,
            now
        ));
        assert!(!should_trigger(&ids(&[]), true, &ids(&[]), None, now));
    }

    #[test]
    fn auto_refresh_off_never_triggers() {
        let now = at("2026-10-19T12:00:00Z");
        assert!(!should_trigger(
            &ids(&["claude-new"]),
            false,
            &ids(&[]),
            None,
            now
        ));
    }

    #[test]
    fn an_automatic_attempt_two_hours_ago_holds_the_trigger_and_one_25_hours_ago_does_not() {
        let now = at("2026-10-19T12:00:00Z");
        let unpriced = ids(&["claude-new"]);
        let two_hours_ago = Some(at("2026-10-19T10:00:00Z"));
        let a_day_and_an_hour_ago = Some(at("2026-10-18T11:00:00Z"));
        assert!(!should_trigger(
            &unpriced,
            true,
            &ids(&[]),
            two_hours_ago,
            now
        ));
        assert!(should_trigger(
            &unpriced,
            true,
            &ids(&[]),
            a_day_and_an_hour_ago,
            now
        ));
    }

    fn unpriced(list: &[&str]) -> Vec<String> {
        list.iter().map(|id| id.to_string()).collect()
    }

    #[tokio::test]
    async fn a_failed_triggered_refresh_leaves_its_ids_to_retry_after_the_hold() {
        // No document: the spawned refresh fails before any fetch (and the sources are dead anyway).
        let _home = Home::new();
        let refresher = Arc::new(Refresher::new(
            crate::testdb::fresh_pool().await,
            dead_sources(),
            reqwest::Client::new(),
        ));
        let now = Utc.with_ymd_and_hms(2026, 10, 19, 12, 0, 0).unwrap();
        let sonnet = unpriced(&["sonnet"]);

        refresher
            .note_unpriced(&sonnet, true, now)
            .expect("a new unpriced id triggers")
            .await
            .unwrap();

        assert_eq!(refresher.last().map(|status| status.ok), Some(false));
        let two_hours_later = now + chrono::Duration::hours(2);
        assert!(
            refresher
                .note_unpriced(&sonnet, true, two_hours_later)
                .is_none(),
            "held for AUTO_RETRY_HOURS after the attempt"
        );
        let a_day_later = now + chrono::Duration::hours(AUTO_RETRY_HOURS + 1);
        refresher
            .note_unpriced(&sonnet, true, a_day_later)
            .expect("a failed refresh did not make the id known")
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn an_id_still_unpriced_after_a_clean_refresh_does_not_trigger_again() {
        let (_home, pool) = priced_document().await;
        let sources = stub_vendors((200, PRICING_MD), (200, GOOD)).await;
        let refresher = Arc::new(Refresher::new(pool, sources, reqwest::Client::new()));
        let now = Utc.with_ymd_and_hms(2026, 10, 19, 12, 0, 0).unwrap();
        let a_day_later = now + chrono::Duration::hours(AUTO_RETRY_HOURS + 1);

        refresher
            .note_unpriced(&unpriced(&["sonnet"]), true, now)
            .expect("a new unpriced id triggers")
            .await
            .unwrap();

        assert_eq!(refresher.last().map(|status| status.ok), Some(true));
        assert!(
            refresher
                .note_unpriced(&unpriced(&["sonnet"]), true, a_day_later)
                .is_none()
        );
        refresher
            .note_unpriced(&unpriced(&["sonnet", "claude-new"]), true, a_day_later)
            .expect("an id never seen before still triggers")
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn the_known_ids_grow_so_two_views_do_not_take_turns_retriggering() {
        let (_home, pool) = priced_document().await;
        let sources = stub_vendors((200, PRICING_MD), (200, GOOD)).await;
        let refresher = Arc::new(Refresher::new(pool, sources, reqwest::Client::new()));
        let start = Utc.with_ymd_and_hms(2026, 10, 19, 12, 0, 0).unwrap();
        let nth_day = |n: i64| start + chrono::Duration::hours(n * (AUTO_RETRY_HOURS + 1));
        // The global view's unpriced list, then one project's 7-day view with a different one.
        let global = unpriced(&["alias-a"]);
        let project = unpriced(&["alias-b"]);

        refresher
            .note_unpriced(&global, true, nth_day(0))
            .unwrap()
            .await
            .unwrap();
        refresher
            .note_unpriced(&project, true, nth_day(1))
            .unwrap()
            .await
            .unwrap();

        assert!(refresher.note_unpriced(&global, true, nth_day(2)).is_none());
        assert!(
            refresher
                .note_unpriced(&project, true, nth_day(3))
                .is_none()
        );
    }

    // --- the document ----------------------------------------------------------------

    #[tokio::test]
    async fn a_refresh_writes_the_prices_and_keeps_the_rest_of_the_document() {
        let (_home, pool) = priced_document().await;
        let sources = stub_vendors((200, PRICING_MD), (200, GOOD)).await;

        let status = refresh_document(
            &pool,
            &reqwest::Client::new(),
            &sources,
            at("2026-10-19T08:30:00Z"),
        )
        .await;

        assert!(status.ok, "{status:?}");
        assert_eq!(status.prices_as_of.as_deref(), Some("2026-10-19"));
        assert_eq!(status.at, "2026-10-19T08:30:00Z");
        assert!(status.problems.is_empty(), "{:?}", status.problems);
        let config = settings::get::<ModelsConfig>(&Scope::Machine);
        assert_eq!(config.prices_as_of.as_deref(), Some("2026-10-19"));
        assert!(config.price_of("claude-opus-4-7").is_some(), "{config:?}");
        assert!(config.price_of("old").is_none(), "{config:?}");
        assert_eq!(config.claude_model, "opus");
        assert_eq!(config.assistant_choices.len(), 1);
    }

    /// As the importer leaves the machine: `nucleos-models.yaml` is gone (renamed to
    /// `nucleos-models.yaml.imported`) and the `models` document is the configuration. The prices are
    /// read back through `price_of`, from the live reader and from a fresh load of the table (what a
    /// restarted daemon sees), so a body no reader can price does not pass.
    #[tokio::test]
    async fn a_refresh_writes_the_new_prices_into_the_models_document_and_creates_no_file() {
        use crate::settings::{self, Scope, Settings};

        let home = crate::project::rules::testing::Home::new();
        let pool = crate::testdb::fresh_pool().await;
        settings::put(
            &pool,
            &Scope::Machine,
            "models",
            &serde_json::json!({
                "claude_model": "opus",
                "codex_model": "gpt-a",
                "assistant_choices": [{ "id": "opus", "label": "Opus", "brain": "cloud" }],
                "prices_as_of": "2026-01-01",
                "model_prices": { "old": { "input": 1.0 } },
            }),
            "test",
            None,
        )
        .await
        .expect("models is a registered document");
        let path = home.path().join(crate::config::MODELS_CONFIG_FILE);
        let sources = stub_vendors((200, PRICING_MD), (200, GOOD)).await;

        let status = refresh_document(
            &pool,
            &reqwest::Client::new(),
            &sources,
            at("2026-10-19T08:30:00Z"),
        )
        .await;

        assert!(status.ok, "{status:?}");
        assert_eq!(status.prices_as_of.as_deref(), Some("2026-10-19"));
        assert_eq!(
            settings::global()
                .snapshot()
                .version(&Scope::Machine, "models"),
            Some(2),
            "the refresh is one more version of the document"
        );
        let live = crate::config::models_config_now();
        assert!(live.price_of("claude-opus-4-7").is_some(), "{live:?}");
        let next_start = Settings::new();
        next_start.load(&pool).await.unwrap();
        let stored = next_start.snapshot().get::<ModelsConfig>(&Scope::Machine);
        assert_eq!(stored.prices_as_of.as_deref(), Some("2026-10-19"));
        assert!(stored.price_of("claude-opus-4-7").is_some(), "{stored:?}");
        assert!(stored.price_of("old").is_none(), "{stored:?}");
        assert_eq!(
            stored.claude_model, "opus",
            "the rest of the document stays"
        );
        assert_eq!(stored.assistant_choices.len(), 1);
        assert!(!path.exists(), "the refresh must not bring the file back");
    }

    #[tokio::test]
    async fn a_source_answering_500_leaves_the_document_untouched_and_says_why() {
        let (_home, pool) = priced_document().await;
        let sources = stub_vendors((200, PRICING_MD), (500, "down")).await;

        let status = refresh_document(
            &pool,
            &reqwest::Client::new(),
            &sources,
            at("2026-10-19T08:30:00Z"),
        )
        .await;

        assert!(!status.ok);
        assert_eq!(status.prices_as_of.as_deref(), Some("2026-01-01"));
        assert!(!status.problems.is_empty());
        assert_eq!(stored_version(), Some(1));
        let config = settings::get::<ModelsConfig>(&Scope::Machine);
        assert!(config.price_of("old").is_some(), "{config:?}");
    }

    #[tokio::test]
    async fn a_second_refresh_with_the_same_prices_adds_no_version() {
        let (_home, pool) = priced_document().await;
        let sources = stub_vendors((200, PRICING_MD), (200, GOOD)).await;
        let client = reqwest::Client::new();
        let now = at("2026-10-19T08:30:00Z");
        assert!(refresh_document(&pool, &client, &sources, now).await.ok);
        assert_eq!(stored_version(), Some(2));

        let again = refresh_document(&pool, &client, &sources, now).await;

        assert!(again.ok, "{again:?}");
        assert_eq!(stored_version(), Some(2));
    }

    #[tokio::test]
    async fn a_missing_models_document_is_not_created_and_nothing_is_fetched() {
        let _home = Home::new();
        let pool = crate::testdb::fresh_pool().await;
        // Nothing listens there: a fetch would add a "could not read" line to the problems.
        let sources = dead_sources();

        let status = refresh_document(
            &pool,
            &reqwest::Client::new(),
            &sources,
            at("2026-10-19T08:30:00Z"),
        )
        .await;

        assert!(!status.ok);
        assert_eq!(status.problems.len(), 1, "{:?}", status.problems);
        assert!(status.problems[0].contains("no models document"));
        assert_eq!(stored_version(), None);
    }

    /// A row a human or an older build left undecodable is the owner's to repair; a refresh that
    /// replaced it with defaults plus prices would lose what is in it.
    #[tokio::test]
    async fn an_unreadable_models_document_is_left_alone_and_said_so() {
        let _home = Home::new();
        let pool = crate::testdb::fresh_pool().await;
        let body = r#"{"claude_model": 5}"#;
        sqlx::query(
            "INSERT INTO settings (scope, name, body_json, version, updated_at, updated_by)              VALUES (?, ?, ?, 1, '2026-10-11T00:00:00.000Z', 'test')",
        )
        .bind(Scope::Machine.to_string())
        .bind(MODELS_DOCUMENT)
        .bind(body)
        .execute(&pool)
        .await
        .unwrap();
        settings::load(&pool).await.unwrap();

        let status = refresh_document(
            &pool,
            &reqwest::Client::new(),
            &dead_sources(),
            at("2026-10-19T08:30:00Z"),
        )
        .await;

        assert!(!status.ok);
        assert_eq!(status.prices_as_of, None);
        assert_eq!(status.problems.len(), 1, "{:?}", status.problems);
        assert!(status.problems[0].contains("unreadable"));
        let stored: String = sqlx::query_scalar("SELECT body_json FROM settings WHERE name = ?")
            .bind(MODELS_DOCUMENT)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(stored, body);
    }

    #[tokio::test]
    async fn an_edit_made_while_the_refresh_fetches_is_kept() {
        let (_home, pool) = priced_document().await;
        // The catalogue answers only after the document has changed under the refresh, which is
        // where a System-page write lands during a slow fetch.
        let app = axum::Router::new()
            .route("/pricing.md", axum::routing::get(|| async { PRICING_MD }))
            .route(
                "/models",
                axum::routing::get({
                    let pool = pool.clone();
                    move || {
                        let pool = pool.clone();
                        async move {
                            settings::update::<ModelsConfig, _>(
                                &pool,
                                &Scope::Machine,
                                "test",
                                |config| config.codex_model = "gpt-b".to_string(),
                            )
                            .await
                            .unwrap();
                            GOOD
                        }
                    }
                }),
            );
        let base = serve(app).await;
        let sources = PriceSources {
            pricing_url: format!("{base}/pricing.md"),
            openrouter_url: format!("{base}/models"),
        };

        let status = refresh_document(
            &pool,
            &reqwest::Client::new(),
            &sources,
            at("2026-10-19T08:30:00Z"),
        )
        .await;

        assert!(status.ok, "{status:?}");
        let config = settings::get::<ModelsConfig>(&Scope::Machine);
        assert_eq!(config.codex_model, "gpt-b");
        assert!(config.price_of("claude-opus-4-7").is_some(), "{config:?}");
    }

    #[tokio::test]
    async fn a_body_over_the_cap_is_not_read_and_the_refresh_leaves_the_document() {
        let (_home, pool) = priced_document().await;
        // A declared length over the cap is refused on the headers alone.
        let oversized = vec![b' '; MAX_BODY_BYTES + 1];
        let app = axum::Router::new()
            .route("/pricing.md", axum::routing::get(|| async { PRICING_MD }))
            .route(
                "/models",
                axum::routing::get(move || {
                    let body = oversized.clone();
                    async move { body }
                }),
            );
        let base = serve(app).await;
        let sources = PriceSources {
            pricing_url: format!("{base}/pricing.md"),
            openrouter_url: format!("{base}/models"),
        };

        let status = refresh_document(
            &pool,
            &reqwest::Client::new(),
            &sources,
            at("2026-10-19T08:30:00Z"),
        )
        .await;

        assert!(!status.ok, "{status:?}");
        assert!(
            status
                .problems
                .iter()
                .any(|problem| problem.contains("could not read")),
            "{:?}",
            status.problems
        );
        assert_eq!(stored_version(), Some(1));
    }

    #[tokio::test]
    async fn a_chunked_body_is_dropped_once_it_passes_the_cap() {
        // No declared length: the body has to be counted as it arrives.
        let chunked = || async {
            axum::body::Body::from_stream(tokio_util::io::ReaderStream::new(std::io::Cursor::new(
                vec![b'x'; 4096],
            )))
        };
        let base = serve(axum::Router::new().route("/big", axum::routing::get(chunked))).await;
        let url = format!("{base}/big");
        let client = reqwest::Client::new();

        assert_eq!(fetch_at_most(&client, &url, 1024).await, None);
        assert_eq!(
            fetch_at_most(&client, &url, 8192)
                .await
                .map(|body| body.len()),
            Some(4096)
        );
    }

    #[test]
    fn the_loop_comes_back_in_an_hour_after_a_failed_refresh_and_in_a_day_otherwise() {
        let now = Utc.with_ymd_and_hms(2026, 10, 19, 8, 30, 0).unwrap();
        let failed = PriceRefreshStatus::new(now, false, None, vec!["down".to_string()]);
        let worked = PriceRefreshStatus::new(now, true, Some("2026-10-19".to_string()), vec![]);
        assert_eq!(sleep_after_tick(Some(&failed)), RETRY_AFTER_FAILURE);
        assert_eq!(sleep_after_tick(Some(&worked)), CHECK_EVERY);
        assert_eq!(sleep_after_tick(None), CHECK_EVERY);
        assert!(RETRY_AFTER_FAILURE < CHECK_EVERY);
    }

    #[tokio::test]
    async fn the_button_records_its_status_as_the_last_one() {
        let (_home, pool) = priced_document().await;
        let sources = stub_vendors((200, PRICING_MD), (200, GOOD)).await;
        let refresher = Refresher::new(pool, sources, reqwest::Client::new());
        assert_eq!(refresher.last(), None);

        let status = refresher.refresh_now(at("2026-10-19T08:30:00Z")).await;

        assert!(status.ok, "{status:?}");
        assert_eq!(refresher.last(), Some(status));
    }

    #[tokio::test]
    async fn the_loop_tick_does_nothing_while_auto_refresh_is_off() {
        let pool = crate::testdb::fresh_pool().await;
        let refresher = Refresher::new(pool, dead_sources(), reqwest::Client::new());
        let config = ModelsConfig {
            auto_refresh_prices: false,
            ..ModelsConfig::default()
        };
        let tick = refresher
            .auto_tick(&config, at("2026-10-19T08:30:00Z"))
            .await;
        assert_eq!(tick, None);
        assert_eq!(refresher.last(), None);
    }

    #[test]
    fn the_status_serialises_as_the_shell_reads_it() {
        let status = PriceRefreshStatus::new(
            Utc.with_ymd_and_hms(2026, 10, 19, 8, 30, 0).unwrap(),
            false,
            None,
            vec!["could not read x".to_string()],
        );
        assert_eq!(
            serde_json::to_value(&status).unwrap(),
            serde_json::json!({
                "at": "2026-10-19T08:30:00Z",
                "ok": false,
                "prices_as_of": null,
                "problems": ["could not read x"],
            })
        );
    }

    /// The parser is a port of a script and is otherwise tested against fixtures alone; this reads
    /// the pages it was written for, through the daemon's own `fetch` (the body cap and the
    /// `User-Agent` included), so a vendor changing a page's shape fails here and not in a
    /// production log.
    ///
    /// A line is a problem unless it is one of the two notes a healthy run prints: a cross-check
    /// disagreement with OpenRouter ("keeping anthropic's") or an `openai/*` model priced by time of
    /// day. Those two are printed for a human, not asserted on, because the vendors can differ by
    /// a cent on a given day and that is not this parser's fault.
    #[tokio::test]
    #[ignore = "needs the internet: reads the real Anthropic pricing page and OpenRouter catalogue; run by hand when the vendor pages change: cargo test -p nucleos-app --lib the_real_pricing_pages -- --ignored --nocapture"]
    async fn the_real_pricing_pages_are_read_without_a_problem() {
        let client = reqwest::Client::builder()
            .timeout(FETCH_TIMEOUT)
            .build()
            .unwrap();
        let (pricing, catalogue) = tokio::join!(
            fetch(&client, parse::PRICING_DOC),
            fetch(&client, parse::OPENROUTER_MODELS)
        );
        assert!(
            pricing.is_some() && catalogue.is_some(),
            "a source was not fetched (pricing page: {}, OpenRouter catalogue: {}) — see the \
             warning above, or check the connection",
            pricing.is_some(),
            catalogue.is_some()
        );

        let (refreshed, lines) = parse::refresh_from_bodies(
            pricing.as_deref(),
            catalogue.as_deref(),
            &Utc::now().date_naive().to_string(),
        );
        for line in &lines {
            eprintln!("{line}");
        }
        let refreshed = refreshed.expect(
            "a source did not parse, so a page changed shape — the lines printed above say which",
        );
        let problems: Vec<&String> = lines
            .iter()
            .filter(|line| {
                !line.contains("keeping anthropic's") && !line.contains("override by time of day")
            })
            .collect();
        assert!(
            problems.is_empty(),
            "problems reading the pages: {problems:#?}"
        );

        // The prices must also come back whole from the JSON the document stores them as, with the
        // shapes the cost accounting depends on.
        let stored: PriceTable =
            serde_json::from_value(serde_json::to_value(&refreshed.model_prices).unwrap())
                .expect("the prices read back from the stored body");
        assert_eq!(stored.dropped, Vec::<String>::new());
        let claude: Vec<_> = stored
            .iter()
            .filter(|(id, _)| id.starts_with("claude-"))
            .collect();
        assert!(
            claude
                .iter()
                .any(|(_, price)| price.input.is_some() && price.output.is_some()),
            "no claude-* model with an input and an output price: {claude:#?}"
        );
        assert!(
            claude.iter().any(|(_, price)| price.fast.is_some()),
            "no claude-* model with a fast rate — the fast mode table was not read"
        );
        assert!(
            claude.iter().any(|(_, price)| price.geo_us_mult.is_some()),
            "no claude-* model with a geo_us_mult — the data residency multiplier was not read"
        );
        assert!(
            stored.iter().any(|(id, price)| {
                id.starts_with("gpt-") && price.source.as_deref() == Some("openrouter")
            }),
            "no gpt-* model priced from OpenRouter"
        );
    }
}
