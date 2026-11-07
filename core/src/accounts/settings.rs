//! The `account_pool` settings document: how the pool decides. Written through the generic
//! `PUT /settings/machine/account_pool`; read at once at the next chat turn or run launch.

use std::collections::BTreeMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use super::AccountChoice;
use crate::settings::{Document, Kind, Scope};

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct AccountPoolDoc {
    /// Default for new chats and runs: "auto" | "provider:<p>" | "account:<id>".
    pub default_choice: AccountChoice,
    pub weights: Weights,
    /// Per engine; absent = on. Effective only when the adapter is Portable/SharedStore.
    pub mid_switch: BTreeMap<String, bool>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Weights {
    pub urgency: f64,
    pub risk_penalty: f64,
    /// A window at or above this fraction counts as exhausted (`quota::EXHAUSTED_AT`).
    pub exhausted_at: f64,
    /// Score margin a switch back must beat (score units).
    pub hysteresis: f64,
    /// Fallback projection per live session per window.
    pub per_session_fraction: f64,
    pub default_cache_ttl_secs: u64,
}

impl Default for Weights {
    fn default() -> Self {
        Self {
            urgency: 1.0,
            risk_penalty: 2.0,
            exhausted_at: 0.95,
            hysteresis: 0.15,
            per_session_fraction: 0.02,
            default_cache_ttl_secs: 300,
        }
    }
}

impl Document for AccountPoolDoc {
    const NAME: &'static str = "account_pool";
    const KIND: Kind = Kind::Machine;
    fn finish(self) -> Result<Self, String> {
        let w = &self.weights;
        if [w.exhausted_at, w.per_session_fraction]
            .iter()
            .any(|f| !(0.0..=1.0).contains(f))
        {
            return Err(
                "account_pool weights: exhausted_at and per_session_fraction must be within 0..=1"
                    .into(),
            );
        }
        if [w.urgency, w.risk_penalty, w.hysteresis]
            .iter()
            .any(|v| !v.is_finite() || *v < 0.0)
        {
            return Err(
                "account_pool weights: urgency, risk_penalty and hysteresis must be finite and not negative"
                    .into(),
            );
        }
        Ok(self)
    }
}

/// The document from the settings snapshot; the default when absent.
pub fn current() -> Arc<AccountPoolDoc> {
    crate::settings::get::<AccountPoolDoc>(&Scope::Machine)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_the_contract_and_stray_fields_are_refused() {
        let doc: AccountPoolDoc = serde_json::from_value(serde_json::json!({})).unwrap();
        assert_eq!(doc, AccountPoolDoc::default());
        assert_eq!(doc.default_choice, AccountChoice::Auto);
        assert_eq!(doc.weights.default_cache_ttl_secs, 300);
        assert!(doc.clone().finish().is_ok());
        assert!(
            serde_json::from_value::<AccountPoolDoc>(serde_json::json!({ "wieghts": {} })).is_err()
        );
        let pinned: AccountPoolDoc =
            serde_json::from_value(serde_json::json!({ "default_choice": "provider:x" })).unwrap();
        assert_eq!(pinned.default_choice, AccountChoice::Provider("x".into()));
    }

    #[test]
    fn finish_rejects_out_of_range_weights() {
        let mut doc = AccountPoolDoc::default();
        doc.weights.exhausted_at = 1.5;
        assert!(doc.finish().is_err());
    }
}
