//! Family D, context group: path and sequence rules over reads, edits and files (D1 THRASH, D2, D3
//! FLAILING, D10, D13 UNVERIFIED, D14 LATE SCOPE). Rules read the session's facts and the thresholds in
//! `DevtimeRulesConfig`; they never write a row.
//!
//! Stub: registers the entry point and finds nothing until the family's rules are written.

use crate::config::DevtimeRulesConfig;
use crate::devtime_rules::{RuleOutput, SessionFacts};

pub fn run(_facts: &SessionFacts, _cfg: &DevtimeRulesConfig) -> RuleOutput {
    RuleOutput::default()
}
