//! §spec notch-de-quota
//!
//! Typed HTTP client for the quota sidecar's loopback API.
//!
//! It exists for the same reason `web_client.rs` does: transport belongs in a module of its own, so
//! `quota.rs` can own thresholds, persistence and states without ever learning that the sidecar
//! speaks HTTP.
//!
//! The asymmetry with `web_client.rs` is worth naming, because it is the design (D2): that client
//! carries the owner's search key out to a vendor; this one carries nothing. The provider token
//! never enters this process — the sidecar reads Claude Code's own credential file at the point of
//! use and answers with a fraction. So the only secret on this path is the daemon's own bearer for
//! its own sidecar, and everything that comes back is safe to put in a feed row.

use std::time::Duration;

use serde::Deserialize;

/// One limit period of one provider, exactly as the sidecar reports it.
#[derive(Debug, Clone, Deserialize)]
pub struct WindowReading {
    /// `5h` or `7d`.
    pub window: String,
    /// In [0,1]. The sidecar converts both providers' percentages at its own edge — see
    /// `sidecars/quota/reading/reading.go`, whose package comment is the authority on why nothing
    /// past it is allowed to see a percentage.
    pub used_fraction: f64,
    pub resets_at: Option<chrono::DateTime<chrono::Utc>>,
    /// The reading describes a window that has already rolled over.
    #[serde(default)]
    pub stale: bool,
}

/// Everything the sidecar knows about one provider's quota right now.
#[derive(Debug, Clone, Deserialize)]
pub struct ProviderReading {
    pub provider: String,
    /// `official`, `derived` or `unmeasured`. Parsed into [`crate::quota::Fidelity`] by the domain;
    /// kept as text here so a value this build does not know is a reading to describe rather than a
    /// deserialisation that fails and takes the other providers down with it.
    pub fidelity: String,
    pub read_at: chrono::DateTime<chrono::Utc>,
    /// Absent AND `null` both read as no windows. `#[serde(default)]` alone covers only the absent
    /// field, and the sidecar sends `null`: Go writes a nil slice that way, and every provider it
    /// could not read has one. That null used to fail the whole report, so one rate-limited
    /// provider took the other provider's live figures down with it (2026-09-24).
    #[serde(default, deserialize_with = "null_as_empty")]
    pub windows: Vec<WindowReading>,
    /// Why a reading is `unmeasured`, in words meant for the owner. Never a token, a header, or a
    /// URL with a credential in it — the sidecar's `claude` package is tested for exactly that.
    #[serde(default)]
    pub detail: String,
    /// The vendor's own word for how bad this is. Recorded and never acted on.
    #[serde(default)]
    pub severity: String,
}

/// A list that may arrive as `null`, read as empty. See [`ProviderReading::windows`].
fn null_as_empty<'de, D, T>(deserializer: D) -> Result<Vec<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Ok(Option::<Vec<T>>::deserialize(deserializer)?.unwrap_or_default())
}

#[derive(Debug, Clone, Deserialize)]
pub struct QuotaReport {
    pub providers: Vec<ProviderReading>,
    /// The answer came from the sidecar's TTL window rather than from a fresh call to the vendor.
    /// Surfaced because `read_at` alone cannot distinguish "measured a second ago" from "measured a
    /// minute ago and held", and the notch shows the age of a figure.
    #[serde(default)]
    pub cached: bool,
}

/// What went wrong, in the one shape a caller has to act on.
///
/// Deliberately a single variant, unlike [`crate::web_client::WebError`]'s five. The caller's
/// decision tree has one branch — the sidecar answered or it did not — because a provider that
/// failed is reported INSIDE a successful answer as an `unmeasured` reading with its own reason.
/// Splitting this further would invent distinctions nothing downstream can use.
#[derive(Debug)]
pub struct QuotaError(pub String);

impl std::fmt::Display for QuotaError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "quota sidecar unreachable: {}", self.0)
    }
}

#[derive(Clone)]
pub struct QuotaClient {
    http: reqwest::Client,
    base: String,
    token: String,
}

/// Describes everything except the one thing that must not be described.
///
/// `CouncilRuntime`'s lesson, applied to the third struct that reaches `AppState` holding a
/// credential. The bearer here is only the daemon's key for its own sidecar rather than a provider
/// token (design D2), but it is still the key that opens the sidecar to anything on loopback, and
/// `#[derive(Debug)]` is one `tracing::debug!(?state.quota, …)` away from writing it into a
/// rotating log file — by somebody printing configuration, who will not be thinking about which
/// secret is the small one.
impl std::fmt::Debug for QuotaClient {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("QuotaClient")
            .field("base", &self.base)
            .field("token", &"<redacted>")
            .finish()
    }
}

/// The ceiling on one sidecar call. Above the sidecar's own 15s fetch timeout so a vendor that
/// hangs is reported by the side that knows why, and well under the shell's 60s poll so a stuck
/// call cannot still be in flight when the next one starts.
const CALL_TIMEOUT: Duration = Duration::from_secs(25);

impl QuotaClient {
    pub fn new(addr: &str, token: String) -> Self {
        Self {
            http: reqwest::Client::builder()
                .timeout(CALL_TIMEOUT)
                .build()
                .unwrap_or_default(),
            base: format!("http://{addr}"),
            token,
        }
    }

    /// Ask the sidecar for every provider's current quota.
    pub async fn report(&self) -> Result<QuotaReport, QuotaError> {
        let response = self
            .http
            .get(format!("{}/quota", self.base))
            .bearer_auth(&self.token)
            .send()
            .await
            .map_err(|error| QuotaError(with_causes(&error)))?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(QuotaError(format!("{}: {}", status.as_u16(), body.trim())));
        }
        response
            .json::<QuotaReport>()
            .await
            .map_err(|error| QuotaError(with_causes(&error)))
    }
}

/// An error and every cause under it, on one line.
///
/// `reqwest::Error`'s own text is only its outermost layer — "error decoding response body" — and
/// the reason is one `source()` down. That one line was the whole of what the daemon recorded while
/// the null-window bug above failed every fresh read, and it named no field and no type: the
/// cause, `invalid type: null, expected a sequence`, was there all along and never printed. Nothing
/// under it carries a credential — serde reports a position, not the text it was reading.
fn with_causes(error: &dyn std::error::Error) -> String {
    let mut text = error.to_string();
    let mut cause = error.source();
    while let Some(next) = cause {
        let said = next.to_string();
        if !text.contains(&said) {
            text.push_str(": ");
            text.push_str(&said);
        }
        cause = next.source();
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_base_url_is_loopback_http() {
        let client = QuotaClient::new(crate::sidecar::QUOTA_ADDR, "t".into());
        assert_eq!(client.base, "http://127.0.0.1:8796");
    }

    /// The bearer must not come back out through the one formatter everybody reaches for.
    #[test]
    fn printing_the_client_does_not_print_its_bearer() {
        let client = QuotaClient::new(crate::sidecar::QUOTA_ADDR, "s3cret-bearer".into());

        let printed = format!("{client:?}");

        assert!(!printed.contains("s3cret-bearer"), "{printed}");
        assert!(printed.contains("<redacted>"), "{printed}");
        assert!(
            printed.contains("127.0.0.1:8796"),
            "the guard must still describe what it is guarding: {printed}"
        );
    }

    /// A provider this build has never heard of must arrive intact rather than take the answer down.
    ///
    /// The sidecar's provider list is meant to grow (design D5/D6: up to three, chosen by the
    /// owner), and it ships independently of the daemon. A strict enum here would turn "a third
    /// provider appeared" into "the notch shows nothing", which is the failure the fidelity ladder
    /// exists to avoid.
    #[test]
    fn a_provider_this_build_does_not_know_still_parses() {
        let report: QuotaReport = serde_json::from_str(
            r#"{"providers":[{"provider":"gemini","fidelity":"unmeasured",
                 "read_at":"2026-09-19T05:00:00Z","detail":"no quota source"}],"cached":false}"#,
        )
        .expect("an unknown provider is a reading, not a parse failure");

        assert_eq!(report.providers.len(), 1);
        assert_eq!(report.providers[0].provider, "gemini");
        assert!(report.providers[0].windows.is_empty());
    }

    /// An unreadable provider arrives as `"windows": null`, and it must not take the other one down.
    ///
    /// That is what the sidecar actually sends: `reading.Unavailable` leaves the Go slice nil, and
    /// `encoding/json` writes a nil slice as `null`, not `[]`. `#[serde(default)]` covers an ABSENT
    /// field only — the test above — so the null failed the whole report, and the daemon fell back
    /// to stored figures for a provider that was answering perfectly well. Captured 2026-09-24 with
    /// the usage endpoint rate-limiting: every fresh read failed with "error decoding response
    /// body", and the notch marked BOTH providers last-known for as long as the 429s lasted.
    #[test]
    fn an_unreadable_provider_sent_as_null_windows_leaves_the_other_one_readable() {
        let report: QuotaReport = serde_json::from_str(
            r#"{"providers":[
                 {"provider":"claude","fidelity":"unmeasured","read_at":"2026-09-24T20:52:04Z",
                  "windows":null,"detail":"the usage endpoint answered 429"},
                 {"provider":"codex","fidelity":"derived","read_at":"2026-09-24T20:52:04Z",
                  "windows":[{"window":"5h","used_fraction":0.12,
                              "resets_at":"2026-09-24T22:35:55Z","stale":false}],
                  "detail":"plan plus"}],
               "cached":false}"#,
        )
        .expect("a null window list is an unmeasured provider, not a failed report");

        assert!(report.providers[0].windows.is_empty());
        assert_eq!(report.providers[1].windows.len(), 1);
    }

    /// The reason a decode failed has to reach the log, not just the fact that it did.
    #[test]
    fn a_decode_failure_names_its_cause() {
        #[derive(Debug)]
        struct Outer(serde_json::Error);
        impl std::fmt::Display for Outer {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("error decoding response body")
            }
        }
        impl std::error::Error for Outer {
            fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
                Some(&self.0)
            }
        }
        let inner = serde_json::from_str::<Vec<u8>>("null").unwrap_err();

        let said = with_causes(&Outer(inner));

        assert!(said.starts_with("error decoding response body: "), "{said}");
        assert!(said.contains("invalid type: null"), "{said}");
    }

    /// A window with no reset is a real answer (the 2026-09-19 capture had one), and `severity` is
    /// absent whenever the vendor named none. Neither may be a required field.
    #[test]
    fn an_absent_reset_and_an_absent_severity_are_both_readable() {
        let report: QuotaReport = serde_json::from_str(
            r#"{"providers":[{"provider":"claude","fidelity":"official",
                 "read_at":"2026-09-19T05:00:00Z",
                 "windows":[{"window":"5h","used_fraction":0.54,"resets_at":null}]}]}"#,
        )
        .expect("a window without a reset must parse");

        let window = &report.providers[0].windows[0];
        assert!(window.resets_at.is_none());
        assert!(!window.stale);
        assert_eq!(report.providers[0].severity, "");
        assert!(!report.cached);
    }
}
