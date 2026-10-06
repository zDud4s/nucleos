//! Family B: repeated and avoidable failures (B1 to B7): precision, project and environment knowledge,
//! estimation, machine and permission failures. Rules read the session's facts and the thresholds in
//! `DevtimeRulesConfig`; they never write a row.
//!
//! Stub: registers the entry point and finds nothing until the family's rules are written.

use crate::config::DevtimeRulesConfig;
use crate::devtime_rules::{RuleOutput, SessionFacts};

pub fn run(_facts: &SessionFacts, _cfg: &DevtimeRulesConfig) -> RuleOutput {
    RuleOutput::default()
}
