//! Loopback origins an agent's browser may reach: the per-project `browser_project` setting, the ports
//! that can never be allowed, and the owner decision a refused origin turns into.
//!
//! The sidecar fences loopback off for agent profiles (consequence `loopback`). A project can lift that
//! fence for the dev servers its owner names, and only the owner can: the setting file is in the
//! project state directory the agent's write guard covers, and the one path an agent has is to *ask*,
//! which files a decision in the inbox. Approving appends the origin; refusing changes nothing.

use std::collections::BTreeSet;
use std::sync::{LazyLock, Mutex};

use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;

use crate::browser_client::Placement;
use crate::settings::Scope;

/// Prefix of the decision id: `loopback:<project>:<origin>`. The id carries the payload, so one origin
/// asked twice in one project is one open decision, and the approval needs no side table.
const DECISION_PREFIX: &str = "loopback:";

/// The consequence the sidecar reports for a loopback origin it refused.
pub const LOOPBACK: &str = "loopback";

/// The project's browser settings (`browser_project.yaml`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct BrowserProject {
    /// Loopback origins the project's agent browsers may open, e.g. `http://localhost:5173`: http or https, host `localhost`, `127.0.0.1` or `[::1]`, an explicit port, no path.
    pub local_origins: Vec<String>,
}

impl BrowserProject {
    /// Every origin valid and none repeated.
    pub(crate) fn check(&self) -> Result<(), String> {
        let mut seen = BTreeSet::new();
        for raw in &self.local_origins {
            let origin = normalise_origin(raw)?;
            if origin != *raw {
                return Err(format!("`{raw}` is not in canonical form (`{origin}`)"));
            }
            if !seen.insert(origin) {
                return Err(format!("`{raw}` is listed twice"));
            }
        }
        Ok(())
    }
}

/// `scheme://host:port` in lower case, or why `raw` is not an origin an owner can allow.
pub fn normalise_origin(raw: &str) -> Result<String, String> {
    let text = raw.trim().to_ascii_lowercase();
    let (scheme, rest) = text
        .split_once("://")
        .ok_or_else(|| format!("`{raw}` has no scheme"))?;
    if scheme != "http" && scheme != "https" {
        return Err(format!("`{raw}`: the scheme must be http or https"));
    }
    if rest.is_empty() || rest.contains(['/', '?', '#', '@', ' ']) {
        return Err(format!(
            "`{raw}` must be only scheme, host and port, with no path"
        ));
    }
    let (host, port) = if rest.starts_with('[') {
        let end = rest
            .find("]:")
            .ok_or_else(|| format!("`{raw}` needs an explicit port"))?;
        (&rest[..=end], &rest[end + 2..])
    } else {
        rest.rsplit_once(':')
            .ok_or_else(|| format!("`{raw}` needs an explicit port"))?
    };
    if !matches!(host, "localhost" | "127.0.0.1" | "[::1]") {
        return Err(format!(
            "`{raw}`: the host must be localhost, 127.0.0.1 or [::1]"
        ));
    }
    let port: u16 = port
        .parse()
        .ok()
        .filter(|port| *port != 0)
        .ok_or_else(|| format!("`{raw}` has no valid port"))?;
    Ok(format!("{scheme}://{host}:{port}"))
}

/// The origin of a page URL (its path, query and fragment dropped), when it is one an owner can allow.
pub fn origin_of_url(url: &str) -> Option<String> {
    let url = url.trim();
    let (scheme, rest) = url.split_once("://")?;
    let authority = rest.split(['/', '?', '#']).next()?;
    normalise_origin(&format!("{scheme}://{authority}")).ok()
}

/// The port of a normalised origin.
fn port_of(origin: &str) -> Option<u16> {
    origin.rsplit(':').next()?.parse().ok()
}

// ---------------------------------------------------------------------------------------------------
// Forbidden ports.

/// CDP ports the shell registered a browser on since this process started. They are picked at run time,
/// so the núcleo learns them as the shell registers each one (`embedded::Registry::register`).
static CDP_PORTS: LazyLock<Mutex<BTreeSet<u16>>> = LazyLock::new(|| Mutex::new(BTreeSet::new()));

/// Remembers a shell CDP port as one no origin can ever be allowed on.
pub fn note_cdp_port(port: u16) {
    CDP_PORTS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(port);
}

/// The ports no agent browser may reach however the project's list reads: the daemon's HTTP port, every
/// sidecar's port and the shell's CDP ports. Sorted, no repeats.
pub fn forbidden_ports() -> Vec<u16> {
    let mut ports: BTreeSet<u16> = BTreeSet::new();
    ports.insert(crate::daemon_client::port());
    for address in [
        crate::sidecar::EMAIL_FETCH_ADDR,
        crate::sidecar::WEB_ADDR,
        crate::sidecar::BROWSER_ADDR,
        crate::sidecar::QUOTA_ADDR,
        crate::sidecar::REMOTE_ADDR,
    ] {
        if let Some(port) = address.rsplit(':').next().and_then(|p| p.parse().ok()) {
            ports.insert(port);
        }
    }
    ports.extend(
        CDP_PORTS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .copied(),
    );
    ports.into_iter().collect()
}

// ---------------------------------------------------------------------------------------------------
// The placement.

/// The project's allowed origins; empty with no project or when the document cannot be read (the fence
/// stays shut rather than guessing).
pub fn origins_of(project_id: &str) -> Vec<String> {
    if project_id.is_empty() {
        return Vec::new();
    }
    crate::project::rules::browser_project(project_id)
        .map(|doc| doc.local_origins)
        .unwrap_or_default()
}

/// `placement` with what the sidecar needs to judge loopback: the project's list and the forbidden
/// ports. For an agent profile, in every place the núcleo builds one.
pub fn decorate(placement: Placement, project_id: &str) -> Placement {
    placement
        .with_local_origins(origins_of(project_id))
        .with_forbidden_ports(forbidden_ports())
}

// ---------------------------------------------------------------------------------------------------
// The decision.

/// The decision id for `origin` in `project_id`.
fn decision_id(project_id: &str, origin: &str) -> String {
    format!("{DECISION_PREFIX}{project_id}:{origin}")
}

/// Files the owner decision "Allow <origin> in this project?" for a page the fence refused as loopback.
///
/// Nothing is filed with no project, for a URL that is not an allowable origin, for an origin already
/// on the list, or for a forbidden port: those are not the owner's to allow. Returns whether a decision
/// is now open (a repeat of an open one counts).
pub async fn ask_for_origin(pool: &SqlitePool, project_id: Option<&str>, url: &str) -> bool {
    let Some(project_id) = project_id.filter(|id| !id.is_empty() && *id != super::NO_PROJECT)
    else {
        return false;
    };
    let Some(origin) = origin_of_url(url) else {
        return false;
    };
    if port_of(&origin).is_none_or(|port| forbidden_ports().contains(&port)) {
        return false;
    }
    if origins_of(project_id).contains(&origin) {
        return false;
    }
    let question =
        format!("Allow {origin} in this project? [Allow {origin} in this project] [Refuse]");
    match crate::tree_job::decisions::ask_ownerless(
        pool,
        &decision_id(project_id, &origin),
        &question,
    )
    .await
    {
        Ok(_) => true,
        Err(error) => {
            tracing::warn!(%error, origin, "browser: could not file the loopback decision");
            false
        }
    }
}

/// Whether an owner's answer to a decision reads as "yes".
fn approves(answer: &str) -> bool {
    let word = answer
        .trim()
        .split(|c: char| !c.is_alphanumeric())
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    matches!(
        word.as_str(),
        "allow" | "approve" | "approved" | "yes" | "sim" | "permitir"
    )
}

/// What an answered decision does when it is a loopback one: an approval appends the origin to the
/// project's list (validated, deduped); a refusal, or a decision of another kind, does nothing.
pub async fn on_answered(pool: &SqlitePool, decision_id: &str, answer: &str) {
    let Some(rest) = decision_id.strip_prefix(DECISION_PREFIX) else {
        return;
    };
    if !approves(answer) {
        return;
    }
    let Some((project_id, origin)) = rest.split_once(':') else {
        return;
    };
    let Ok(origin) = normalise_origin(origin) else {
        return;
    };
    if port_of(&origin).is_none_or(|port| forbidden_ports().contains(&port)) {
        return;
    }
    let result = crate::settings::update::<BrowserProject, _>(
        pool,
        &Scope::project(project_id),
        "owner",
        move |doc| {
            if !doc.local_origins.contains(&origin) {
                doc.local_origins.push(origin);
            }
        },
    )
    .await;
    if let Err(error) = result {
        tracing::warn!(%error, decision_id, "browser: could not add the approved loopback origin");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::{Settings, testing};
    use crate::tree_job::decisions;

    #[test]
    fn origins_are_normalised_and_the_rest_refused() {
        assert_eq!(
            normalise_origin("http://localhost:5173").unwrap(),
            "http://localhost:5173"
        );
        assert_eq!(
            normalise_origin(" HTTPS://LocalHost:8443 ").unwrap(),
            "https://localhost:8443"
        );
        assert_eq!(
            normalise_origin("http://127.0.0.1:3000").unwrap(),
            "http://127.0.0.1:3000"
        );
        assert_eq!(
            normalise_origin("http://[::1]:3000").unwrap(),
            "http://[::1]:3000"
        );
        for bad in [
            "localhost:5173",
            "ftp://localhost:21",
            "http://localhost",
            "http://localhost:0",
            "http://localhost:99999",
            "http://localhost:5173/app",
            "http://localhost:5173/",
            "http://example.com:5173",
            "http://[::1]",
            "http://user@localhost:5173",
            "",
        ] {
            assert!(normalise_origin(bad).is_err(), "{bad} was accepted");
        }
    }

    #[test]
    fn the_document_refuses_a_repeat_or_a_bad_origin() {
        let doc = |origins: &[&str]| BrowserProject {
            local_origins: origins.iter().map(|o| (*o).to_owned()).collect(),
        };
        assert!(doc(&[]).check().is_ok());
        assert!(
            doc(&["http://localhost:5173", "http://localhost:3000"])
                .check()
                .is_ok()
        );
        assert!(
            doc(&["http://localhost:5173", "http://localhost:5173"])
                .check()
                .is_err()
        );
        assert!(doc(&["http://example.com:80"]).check().is_err());
        assert!(doc(&["HTTP://localhost:5173"]).check().is_err());
    }

    #[test]
    fn a_page_url_reduces_to_its_origin() {
        assert_eq!(
            origin_of_url("http://localhost:5173/a/b?x=1#top").as_deref(),
            Some("http://localhost:5173")
        );
        assert_eq!(origin_of_url("https://example.com/"), None);
        assert_eq!(origin_of_url("not a url"), None);
    }

    #[test]
    fn the_forbidden_ports_cover_the_daemon_the_sidecars_and_the_shell() {
        note_cdp_port(41_234);
        let ports = forbidden_ports();
        assert!(ports.contains(&crate::daemon_client::port()));
        for port in [8793, 8794, 8795, 8796, 8797, 41_234] {
            assert!(ports.contains(&port), "{port} missing from {ports:?}");
        }
        assert!(
            ports.windows(2).all(|pair| pair[0] < pair[1]),
            "not sorted and unique"
        );
    }

    #[test]
    fn the_placement_carries_the_project_list_and_the_forbidden_ports() {
        let doc = BrowserProject {
            local_origins: vec!["http://localhost:5173".to_owned()],
        };
        let _installed = testing::install(Settings::from_snapshot(
            crate::settings::Snapshot::default().with_document(&Scope::project("alpha"), doc),
        ));
        let placement = decorate(Placement::project("alpha", Vec::new()), "alpha");
        assert_eq!(
            placement.local_origins,
            vec!["http://localhost:5173".to_owned()]
        );
        assert!(placement.forbidden_ports.contains(&8795));

        let none = decorate(
            Placement::ephemeral("embedded-none"),
            super::super::NO_PROJECT,
        );
        assert!(none.local_origins.is_empty());
        assert!(
            none.forbidden_ports.contains(&8795),
            "the fence ports go out with no project too"
        );
    }

    async fn open_decisions(pool: &SqlitePool) -> Vec<(String, String)> {
        sqlx::query_as("SELECT id, question FROM decisions WHERE status = 'open' ORDER BY id")
            .fetch_all(pool)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn a_refused_origin_files_one_decision() {
        let _installed = testing::install(Settings::new());
        let pool = crate::testdb::fresh_pool().await;

        assert!(ask_for_origin(&pool, Some("alpha"), "http://localhost:5173/page").await);
        assert!(ask_for_origin(&pool, Some("alpha"), "http://localhost:5173/other").await);

        let open = open_decisions(&pool).await;
        assert_eq!(open.len(), 1, "asking twice is one decision: {open:?}");
        assert_eq!(open[0].0, "loopback:alpha:http://localhost:5173");
        assert!(
            open[0]
                .1
                .contains("Allow http://localhost:5173 in this project")
        );
        assert!(open[0].1.contains("[Refuse]"));
    }

    #[tokio::test]
    async fn no_decision_for_no_project_a_forbidden_port_or_a_listed_origin() {
        let doc = BrowserProject {
            local_origins: vec!["http://localhost:5173".to_owned()],
        };
        let _installed = testing::install(Settings::from_snapshot(
            crate::settings::Snapshot::default().with_document(&Scope::project("alpha"), doc),
        ));
        let pool = crate::testdb::fresh_pool().await;

        assert!(!ask_for_origin(&pool, None, "http://localhost:5173").await);
        assert!(!ask_for_origin(&pool, Some(""), "http://localhost:5173").await);
        assert!(!ask_for_origin(&pool, Some("alpha"), "http://localhost:5173").await);
        assert!(!ask_for_origin(&pool, Some("alpha"), "http://localhost:8795").await);
        let daemon = format!("http://127.0.0.1:{}", crate::daemon_client::port());
        assert!(!ask_for_origin(&pool, Some("alpha"), &daemon).await);
        assert!(!ask_for_origin(&pool, Some("alpha"), "https://example.com").await);

        assert!(open_decisions(&pool).await.is_empty());
    }

    #[tokio::test]
    async fn approving_appends_the_origin_once_and_refusing_changes_nothing() {
        let _installed = testing::install(Settings::new());
        let pool = crate::testdb::fresh_pool().await;

        ask_for_origin(&pool, Some("alpha"), "http://localhost:5173").await;
        ask_for_origin(&pool, Some("alpha"), "http://localhost:3000").await;

        decisions::answer(
            &pool,
            "loopback:alpha:http://localhost:3000",
            "Refuse",
            "owner",
        )
        .await
        .unwrap();
        assert!(
            origins_of("alpha").is_empty(),
            "a refusal must change nothing"
        );

        decisions::answer(
            &pool,
            "loopback:alpha:http://localhost:5173",
            "Allow",
            "owner",
        )
        .await
        .unwrap();
        assert_eq!(
            origins_of("alpha"),
            vec!["http://localhost:5173".to_owned()]
        );

        // The same approval arriving again (a second decision row for the origin) adds nothing.
        on_answered(&pool, "loopback:alpha:http://localhost:5173", "Allow").await;
        assert_eq!(
            origins_of("alpha"),
            vec!["http://localhost:5173".to_owned()]
        );
    }

    #[tokio::test]
    async fn an_approval_never_reaches_a_forbidden_port() {
        let _installed = testing::install(Settings::new());
        let pool = crate::testdb::fresh_pool().await;

        on_answered(&pool, "loopback:alpha:http://localhost:8795", "Allow").await;
        on_answered(&pool, "loopback:alpha:http://example.com:80", "Allow").await;

        assert!(origins_of("alpha").is_empty());
    }
}
