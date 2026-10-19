//! The daemon keeps the price block of `nucleos-models.yaml` fresh itself, instead of waiting for
//! somebody to remember `scripts/refresh-models.py`: an estimated cost read off prices nobody has
//! refreshed in months is a wrong number shown with a straight face.
//!
//! [`parse`] holds the pure part: two fetched bodies in, the two rendered YAML blocks out, with no
//! network, no file and no clock, so the exact text it renders can be pinned by a test. This module
//! is the rest: fetching, writing the file, and deciding when.
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
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use chrono::{DateTime, NaiveDate, Utc};
use serde::Serialize;

use crate::config::ModelsConfig;

/// Prices older than this many days are refreshed by the loop. A week, because the vendors change
/// prices with a model launch, not daily, and a daily refresh would be two fetches a day for a file
/// that almost never moves.
pub const STALE_AFTER_DAYS: i64 = 7;

/// The least time between two automatic refreshes. Without it, a model that never gets a price (a
/// vendor page that is down, an id no vendor lists) would refetch on every page view.
pub const AUTO_RETRY_HOURS: i64 = 24;

/// How long after startup the loop first looks: long enough that a refresh never competes with the
/// daemon coming up, short enough that a stale file is fixed in the first minute.
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
    /// The price blocks were written, or were already exactly what the vendors publish. `false`
    /// means the file was left as it was and `problems` says why.
    pub ok: bool,
    /// The date the file's prices are now as of: the refresh's own date when `ok`, otherwise the
    /// file's existing `prices_as_of` (best effort; `None` when it had none or did not parse).
    pub prices_as_of: Option<String>,
    /// Everything worth a person's eye: the parse's warnings and problems (a model written without
    /// a price, a source it could not read) and any IO failure. Empty on a clean run.
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
/// `None`, which [`parse::refresh_from_bodies`] turns into "could not read …" and a file left
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

/// The models file's text, or the problem line that says why there is none.
fn read_models_file(path: &Path) -> Result<String, String> {
    let shown = crate::config::MODELS_CONFIG_DISPLAY_PATH;
    std::fs::read_to_string(path).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            format!("{shown} does not exist — prices unchanged")
        } else {
            format!("could not read {shown} ({error}) — prices unchanged")
        }
    })
}

/// The `prices_as_of` a models file's text declares, best effort: `None` when it has none or the
/// text does not parse.
fn prices_as_of_in(text: &str) -> Option<String> {
    crate::config::parse_models_config(text)
        .ok()
        .and_then(|config| config.prices_as_of)
}

/// Refreshes the price blocks of the models file at `path` from `sources`, dated `now`'s UTC day.
///
/// A missing file is not created: `claude_model` and `codex_model` are required keys, so a file
/// holding only prices would not parse and the daemon would ignore it. That is checked before any
/// fetch, so a machine without the file costs no network.
///
/// The rest of the file is kept byte for byte ([`parse::splice_price_blocks`]); it is written only
/// when the text changed, and never when the result would no longer parse — a refresh must never be
/// the reason the models file stops loading.
///
/// The text spliced is read AFTER the fetch, not before: a fetch can take up to [`FETCH_TIMEOUT`],
/// and a splice of the text read before it would put back whatever a System-page write, a hand
/// edit or anything else changed meanwhile. The re-read, the splice and the write happen under
/// [`crate::config::MODELS_FILE_WRITE`], so the System page's write door cannot land in between.
pub async fn refresh_file(
    client: &reqwest::Client,
    sources: &PriceSources,
    path: &Path,
    now: DateTime<Utc>,
) -> PriceRefreshStatus {
    let shown = crate::config::MODELS_CONFIG_DISPLAY_PATH;
    let before = match read_models_file(path) {
        Ok(text) => text,
        Err(problem) => return PriceRefreshStatus::new(now, false, None, vec![problem]),
    };
    let (pricing, catalogue) = tokio::join!(
        fetch(client, &sources.pricing_url),
        fetch(client, &sources.openrouter_url)
    );
    let today = now.date_naive().to_string();
    let (blocks, mut problems) =
        parse::refresh_from_bodies(pricing.as_deref(), catalogue.as_deref(), &today);
    let Some(blocks) = blocks else {
        return PriceRefreshStatus::new(now, false, prices_as_of_in(&before), problems);
    };

    // Held until the write has landed (or been given up); no `await` below it.
    let _models_file_write = crate::config::MODELS_FILE_WRITE.lock().await;
    let original = match read_models_file(path) {
        Ok(text) => text,
        Err(problem) => {
            problems.push(problem);
            return PriceRefreshStatus::new(now, false, None, problems);
        }
    };
    let current_as_of = prices_as_of_in(&original);
    let spliced = parse::splice_price_blocks(&original, &blocks);
    if spliced != original {
        if let Err(error) = crate::config::parse_models_config(&spliced) {
            // The likeliest cause, named because it is invisible in the file: the splice ends the
            // block at the first column-zero line (the rule `scripts/refresh-models.py` splices
            // `assistant_choices` by), so a commented-out row there leaves every row below it in
            // place, and the first of them that the new block also lists is a duplicate key.
            problems.push(format!(
                "the refreshed {shown} would not parse ({error}) — prices unchanged; a comment in \
                 column zero inside `model_prices` ends the block early and leaves the rows below \
                 it in place, so indent or remove it"
            ));
            return PriceRefreshStatus::new(now, false, current_as_of, problems);
        }
        if let Err(error) = crate::project::state::write_atomically(path, &spliced) {
            problems.push(format!(
                "could not write {shown} ({error}) — prices unchanged"
            ));
            return PriceRefreshStatus::new(now, false, current_as_of, problems);
        }
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
    /// refresh leaves its ids to be retried. Not recomputed from the refreshed file: an id the
    /// refresh priced drops out of the page's list by itself, and an id it could not price (an
    /// alias) stays here and does not trigger again. A refresh started by the loop or the button
    /// leaves it alone, since it knows no unpriced list.
    known_unpriced: BTreeSet<String>,
}

/// The price refresh of one models file: its sources, its client, and its state.
pub struct Refresher {
    path: PathBuf,
    sources: PriceSources,
    client: reqwest::Client,
    /// Serialises refreshes, so two never write the file at once. The button waits on it; an
    /// automatic refresh that finds it held skips, since the refresh running is doing its job.
    running: tokio::sync::Mutex<()>,
    state: std::sync::Mutex<RefreshState>,
}

impl Refresher {
    pub fn new(path: PathBuf, sources: PriceSources, client: reqwest::Client) -> Self {
        Refresher {
            path,
            sources,
            client,
            running: tokio::sync::Mutex::new(()),
            state: std::sync::Mutex::new(RefreshState::default()),
        }
    }

    /// The daemon's refresher: the real vendor URLs, and a client with [`FETCH_TIMEOUT`].
    pub fn for_daemon(path: PathBuf) -> Self {
        // `expect` for the reason `ConfiguredAssistants::new` gives: the causes are TLS backend
        // and proxy environment problems, i.e. startup misconfiguration.
        let client = reqwest::Client::builder()
            .timeout(FETCH_TIMEOUT)
            .build()
            .expect("HTTP client for the price refresh (check TLS and proxy environment)");
        Refresher::new(path, PriceSources::default(), client)
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
        let status = refresh_file(&self.client, &self.sources, &self.path, now).await;
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
    /// for it: it shows this view now, and the next one reads the new prices, the models file
    /// being re-read per request. Tests await the handle.
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

/// Called once from `main` when there is a models file path. A second call changes nothing.
pub fn install(refresher: Arc<Refresher>) {
    if REFRESHER.set(refresher).is_err() {
        tracing::warn!("price refresher installed twice; keeping the first");
    }
}

/// `None` until `main` installs one, which it does only when there is a models file path (so
/// never under tests).
pub fn installed() -> Option<Arc<Refresher>> {
    REFRESHER.get().cloned()
}

/// The last refresh since the daemon started, or `None` when none has run.
pub fn last_status() -> Option<PriceRefreshStatus> {
    installed().and_then(|refresher| refresher.last())
}

/// The button. Without a refresher there is no file to refresh, which is a status, not an error.
pub async fn refresh_now(now: DateTime<Utc>) -> PriceRefreshStatus {
    match installed() {
        Some(refresher) => refresher.refresh_now(now).await,
        None => PriceRefreshStatus::new(
            now,
            false,
            None,
            vec![format!(
                "the daemon has no {} to refresh",
                crate::config::MODELS_CONFIG_DISPLAY_PATH
            )],
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
/// (or [`RETRY_AFTER_FAILURE`] after a failed one, [`sleep_after_tick`]), each reading the models
/// file as it is then, so `auto_refresh_prices: false` takes effect without a restart.
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

    /// The unrelated block a refresh must keep, verbatim.
    const CHOICES: &str =
        "assistant_choices:\n  - { id: \"opus\", label: \"Opus\", brain: \"cloud\" }\n";

    /// A models file with an unrelated block and old prices.
    fn models_file() -> String {
        format!(
            "claude_model: opus\ncodex_model: gpt-a\n{CHOICES}\nprices_as_of: \"2026-01-01\"\nmodel_prices:\n  old: {{ input: 1.0 }}\n"
        )
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

    /// A tempdir holding a models file, and the file's path.
    fn models_dir() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(crate::config::MODELS_CONFIG_FILE);
        std::fs::write(&path, models_file()).unwrap();
        (dir, path)
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
            "on unless the file says otherwise"
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
        let dir = tempfile::tempdir().unwrap();
        // No file: the spawned refresh fails before any fetch (and the sources are dead anyway).
        let refresher = Arc::new(Refresher::new(
            dir.path().join(crate::config::MODELS_CONFIG_FILE),
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
        let (_dir, path) = models_dir();
        let sources = stub_vendors((200, PRICING_MD), (200, GOOD)).await;
        let refresher = Arc::new(Refresher::new(path, sources, reqwest::Client::new()));
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
        let (_dir, path) = models_dir();
        let sources = stub_vendors((200, PRICING_MD), (200, GOOD)).await;
        let refresher = Arc::new(Refresher::new(path, sources, reqwest::Client::new()));
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

    // --- the file --------------------------------------------------------------------

    #[tokio::test]
    async fn a_refresh_writes_the_price_blocks_and_keeps_the_rest_of_the_file() {
        let (_dir, path) = models_dir();
        let sources = stub_vendors((200, PRICING_MD), (200, GOOD)).await;

        let status = refresh_file(
            &reqwest::Client::new(),
            &sources,
            &path,
            at("2026-10-19T08:30:00Z"),
        )
        .await;

        assert!(status.ok, "{status:?}");
        assert_eq!(status.prices_as_of.as_deref(), Some("2026-10-19"));
        assert_eq!(status.at, "2026-10-19T08:30:00Z");
        assert!(status.problems.is_empty(), "{:?}", status.problems);
        let written = std::fs::read_to_string(&path).unwrap();
        assert!(written.contains(CHOICES), "{written}");
        assert!(
            written.contains("prices_as_of: \"2026-10-19\""),
            "{written}"
        );
        assert!(written.contains("claude-opus-4-7:"), "{written}");
        assert!(!written.contains("old: { input: 1.0 }"), "{written}");
        let config = crate::config::parse_models_config(&written).unwrap();
        assert_eq!(config.prices_as_of.as_deref(), Some("2026-10-19"));
        assert_eq!(config.assistant_choices.len(), 1);
    }

    #[tokio::test]
    async fn a_source_answering_500_leaves_the_file_untouched_and_says_why() {
        let (_dir, path) = models_dir();
        let sources = stub_vendors((200, PRICING_MD), (500, "down")).await;

        let status = refresh_file(
            &reqwest::Client::new(),
            &sources,
            &path,
            at("2026-10-19T08:30:00Z"),
        )
        .await;

        assert!(!status.ok);
        assert_eq!(status.prices_as_of.as_deref(), Some("2026-01-01"));
        assert!(!status.problems.is_empty());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), models_file());
    }

    #[tokio::test]
    async fn a_second_refresh_with_the_same_prices_leaves_the_file_as_it_was() {
        let (_dir, path) = models_dir();
        let sources = stub_vendors((200, PRICING_MD), (200, GOOD)).await;
        let client = reqwest::Client::new();
        let now = at("2026-10-19T08:30:00Z");
        assert!(refresh_file(&client, &sources, &path, now).await.ok);
        let first = std::fs::read_to_string(&path).unwrap();
        let first_modified = std::fs::metadata(&path).unwrap().modified().unwrap();

        let again = refresh_file(&client, &sources, &path, now).await;

        assert!(again.ok, "{again:?}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), first);
        assert_eq!(
            std::fs::metadata(&path).unwrap().modified().unwrap(),
            first_modified
        );
    }

    #[tokio::test]
    async fn a_missing_models_file_is_not_created_and_nothing_is_fetched() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(crate::config::MODELS_CONFIG_FILE);
        // Nothing listens there: a fetch would add a "could not read" line to the problems.
        let sources = dead_sources();

        let status = refresh_file(
            &reqwest::Client::new(),
            &sources,
            &path,
            at("2026-10-19T08:30:00Z"),
        )
        .await;

        assert!(!status.ok);
        assert_eq!(status.problems.len(), 1, "{:?}", status.problems);
        assert!(status.problems[0].contains("does not exist"));
        assert!(!path.exists());
    }

    #[tokio::test]
    async fn an_edit_made_while_the_refresh_fetches_is_kept() {
        let (_dir, path) = models_dir();
        // The catalogue answers only after the file has changed under the refresh, which is where
        // a System-page write or a hand edit lands during a slow fetch.
        let edited = models_file().replace("codex_model: gpt-a", "codex_model: gpt-b");
        let app = axum::Router::new()
            .route("/pricing.md", axum::routing::get(|| async { PRICING_MD }))
            .route(
                "/models",
                axum::routing::get({
                    let path = path.clone();
                    move || async move {
                        std::fs::write(&path, edited).unwrap();
                        GOOD
                    }
                }),
            );
        let base = serve(app).await;
        let sources = PriceSources {
            pricing_url: format!("{base}/pricing.md"),
            openrouter_url: format!("{base}/models"),
        };

        let status = refresh_file(
            &reqwest::Client::new(),
            &sources,
            &path,
            at("2026-10-19T08:30:00Z"),
        )
        .await;

        assert!(status.ok, "{status:?}");
        let written = std::fs::read_to_string(&path).unwrap();
        assert!(written.contains("codex_model: gpt-b"), "{written}");
        assert!(written.contains("claude-opus-4-7:"), "{written}");
    }

    #[tokio::test]
    async fn a_column_zero_comment_inside_model_prices_refuses_the_refresh_and_says_so() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(crate::config::MODELS_CONFIG_FILE);
        // The comment ends the block for the splice, so the row under it stays, and it repeats a
        // key the new block writes: the result would not parse.
        let original = "claude_model: opus\ncodex_model: gpt-a\nprices_as_of: \"2026-01-01\"\n\
                        model_prices:\n  old: { input: 1.0 }\n# old row\n  claude-opus-4-7: { input: 1.0 }\n";
        crate::config::parse_models_config(original).expect("the file parses as it is");
        std::fs::write(&path, original).unwrap();
        let sources = stub_vendors((200, PRICING_MD), (200, GOOD)).await;

        let status = refresh_file(
            &reqwest::Client::new(),
            &sources,
            &path,
            at("2026-10-19T08:30:00Z"),
        )
        .await;

        assert!(!status.ok, "{status:?}");
        assert_eq!(status.prices_as_of.as_deref(), Some("2026-01-01"));
        assert!(
            status
                .problems
                .iter()
                .any(|problem| problem.contains("would not parse")
                    && problem.contains("comment in column zero inside `model_prices`")),
            "{:?}",
            status.problems
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
    }

    #[tokio::test]
    async fn a_body_over_the_cap_is_not_read_and_the_refresh_leaves_the_file() {
        let (_dir, path) = models_dir();
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

        let status = refresh_file(
            &reqwest::Client::new(),
            &sources,
            &path,
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
        assert_eq!(std::fs::read_to_string(&path).unwrap(), models_file());
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
        let (_dir, path) = models_dir();
        let sources = stub_vendors((200, PRICING_MD), (200, GOOD)).await;
        let refresher = Refresher::new(path, sources, reqwest::Client::new());
        assert_eq!(refresher.last(), None);

        let status = refresher.refresh_now(at("2026-10-19T08:30:00Z")).await;

        assert!(status.ok, "{status:?}");
        assert_eq!(refresher.last(), Some(status));
    }

    #[tokio::test]
    async fn the_loop_tick_does_nothing_while_auto_refresh_is_off() {
        let (_dir, path) = models_dir();
        let refresher = Refresher::new(path, dead_sources(), reqwest::Client::new());
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
    #[ignore = "needs the internet: reads the real Anthropic pricing page and OpenRouter catalogue; run by hand when the vendor pages change: cargo test -p nucleos-core --lib the_real_pricing_pages -- --ignored --nocapture"]
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

        let (blocks, lines) = parse::refresh_from_bodies(
            pricing.as_deref(),
            catalogue.as_deref(),
            &Utc::now().date_naive().to_string(),
        );
        for line in &lines {
            eprintln!("{line}");
        }
        let blocks = blocks.expect(
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

        // The blocks must also make a models file the daemon accepts, with the shapes the cost
        // accounting depends on.
        let spliced = parse::splice_price_blocks(&models_file(), &blocks);
        let config = crate::config::parse_models_config(&spliced)
            .expect("the spliced file parses as a models file");
        let claude: Vec<_> = config
            .model_prices
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
            config.model_prices.iter().any(|(id, price)| {
                id.starts_with("gpt-") && price.source.as_deref() == Some("openrouter")
            }),
            "no gpt-* model priced from OpenRouter"
        );
    }
}
