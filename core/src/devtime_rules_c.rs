//! Family C: spec and clarity (C1 to C3): lost direction, corrections and reverted work. C4 is deferred and
//! is never emitted. Rules read the session's facts and the thresholds in `DevtimeRulesConfig`; they
//! never write a row.
//!
//! Stub: registers the entry point and finds nothing until the family's rules are written.

use crate::config::DevtimeRulesConfig;
use crate::devtime_rules::{RuleOutput, SessionFacts};

pub fn run(_facts: &SessionFacts, _cfg: &DevtimeRulesConfig) -> RuleOutput {
    RuleOutput::default()
}
