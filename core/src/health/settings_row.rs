//! The two rows that read stored settings: `llm_router` (the `router` document, then the router itself)
//! and `settings` (documents the daemon could not read and serves as defaults).

use std::time::Duration;

use super::{FailureCategory, PROBE_TIMEOUT, SubsystemReadout};

/// What the stored `router` document asks of the daemon, before anybody has asked the router
/// anything.
#[derive(Debug, PartialEq)]
pub(super) enum RouterPlan {
    /// No document, or every surface off: routing is not asked for, so there is nothing to measure.
    Off,
    /// A document that names a router off this machine. The daemon runs without routing, but the
    /// owner believes it is on, which is what a red row is for.
    Refused,
    /// Routing is on. The URL and the budget are the document's own.
    Ask { url: String, timeout: Duration },
}

/// PURE: the stored router document (`None` for an absent one) as a plan.
///
/// The conversion is `RouterDoc::to_config`, the loopback fence, so the probe can never be pointed
/// at an address the router client itself would refuse.
pub(super) fn router_plan(doc: Option<&crate::models::route_doc::RouterDoc>) -> RouterPlan {
    let Some(doc) = doc else {
        return RouterPlan::Off;
    };
    match doc.to_config() {
        Err(_) => RouterPlan::Refused,
        Ok(config) if config.is_off() => RouterPlan::Off,
        Ok(config) => RouterPlan::Ask {
            timeout: Duration::from_millis(config.timeout_ms).min(PROBE_TIMEOUT),
            url: config.url,
        },
    }
}

/// PURE: the row. `reachable` matters only for a plan that asks.
///
/// An unreachable router is `degraded` and not `down`: the daemon falls back to the choice it
/// made before routing existed, so nothing stops, and a `down` here would take the whole readout
/// down for an adviser.
pub(super) fn router_row(plan: &RouterPlan, reachable: bool) -> SubsystemReadout {
    match (plan, reachable) {
        (RouterPlan::Off, _) => {
            SubsystemReadout::disabled("llm_router", FailureCategory::NotConfigured)
        }
        (RouterPlan::Refused, _) => {
            SubsystemReadout::down("llm_router", FailureCategory::NotConfigured)
        }
        (RouterPlan::Ask { .. }, true) => SubsystemReadout::ok("llm_router"),
        (RouterPlan::Ask { .. }, false) => {
            SubsystemReadout::degraded("llm_router", FailureCategory::Unreachable)
        }
    }
}

/// Reads the stored `router` document, then asks the router for its tiers.
/// Task text never travels: `targets` is a GET with no body.
pub(super) async fn router_probe() -> SubsystemReadout {
    let doc = crate::settings::get_opt::<crate::models::route_doc::RouterDoc>(
        &crate::settings::Scope::Machine,
    );
    router_answer(router_plan(doc.as_deref())).await
}

/// The probe's one question, for a plan already made: does the router answer `targets`?
pub(super) async fn router_answer(plan: RouterPlan) -> SubsystemReadout {
    let reachable = match &plan {
        RouterPlan::Ask { url, timeout } => crate::router_client::RouterClient::new(url, *timeout)
            .targets()
            .await
            .is_ok(),
        _ => false,
    };
    router_row(&plan, reachable)
}

/// PURE: the settings row, from the snapshot the daemon is reading.
///
/// `down` + `NotConfigured` with `counts` keyed by document name when any stored document cannot be
/// read, `ok` otherwise. A document that cannot be read is served as its default, so the owner
/// believes a setting holds while the daemon runs on another.
pub(super) fn settings_row(snapshot: &crate::settings::Snapshot) -> SubsystemReadout {
    let mut counts: std::collections::BTreeMap<&'static str, i64> =
        std::collections::BTreeMap::new();
    for record in snapshot.unreadable() {
        if let Some(doc_type) = crate::settings::doc_type(&record.name) {
            *counts.entry(doc_type.name).or_insert(0) += 1;
        }
    }
    if counts.is_empty() {
        return SubsystemReadout::ok("settings");
    }
    let mut row = SubsystemReadout::down("settings", FailureCategory::NotConfigured);
    row.counts = Some(counts);
    row
}

/// The settings row for the snapshot the daemon is reading right now.
pub(super) async fn settings_probe() -> SubsystemReadout {
    settings_row(&crate::settings::global().snapshot())
}
