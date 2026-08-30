//! Pure, DB-free derivation logic for `GET /runs/{id}/stop` (spec
//! `.ai/specs/2026-08-29-porque-parou-design.md` §7): "porque parou" — why a run stopped.
//!
//! `runs.rs` is over 3900 lines and `http.rs` is over 11000; nothing about this report belongs in
//! either. This module is responsible for the parts that take state and times in and hand a verdict
//! out — deriving the response's `kind` from a run's `status`, deriving the timeout verdict from
//! elapsed time and the two independent ceilings, and the response's own shapes. Reading the run row
//! and the `shadow_decisions` rows stays in the HTTP handler, which calls the functions here to turn
//! what it read into a response.

// This is a bin-only crate, so dead-code reachability starts at `main`, and this module has no
// caller yet: `GET /runs/{id}/stop` is a later item of the same job (spec §7's own handler, in
// `http.rs`) that reads the run row and the `shadow_decisions` rows and calls what is defined here
// to turn them into a response. Until that lands, nothing reachable from `main` ever calls into this
// file.
//
// Scoped to the non-test build, the way `contacts.rs` and `errands.rs` scope their own suppression,
// so it silences only the absence of a production caller. Under `cfg(test)` the lint stays live —
// every item below is exercised by this module's own table tests, and one that stops being exercised
// has to say so. The instruction, not a description: DELETE THIS LINE with the change that wires
// `http.rs` to this module.
#![cfg_attr(not(test), allow(dead_code))]

use serde::Serialize;

use crate::runs::{ENDED_RUN_STATUSES, TERMINAL_RUN_STATUSES};

/// One `kind` per real run status (spec §5.1) — nothing invented. Six of the eight keep their
/// status's own name; only `awaiting_approval` and `timed_out` read differently, because the report
/// is naming what a caller of this route wants to know about the run rather than echoing the column.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Gate,
    Timeout,
    Failed,
    Cancelled,
    Interrupted,
    Superseded,
    Completed,
    Running,
}

/// PURE: derives the response's `kind` from a run's `status` column (spec §5.1's table).
///
/// Gated on [`ENDED_RUN_STATUSES`] and [`TERMINAL_RUN_STATUSES`] — the two lists `runs.rs` already
/// keeps as the source of truth for which statuses exist and what they mean — rather than a third,
/// hand-typed list of valid statuses here that could drift from either. `awaiting_approval` and
/// `running` are the two live statuses in neither list, and are matched first for that reason.
///
/// Returns `None` for a status this route has never seen: an eighth real status appearing here means
/// `runs.rs` grew a status this table does not know about yet, which is a bug to surface rather than
/// a kind to guess at.
pub(crate) fn derive_kind(status: &str) -> Option<Kind> {
    if status == "awaiting_approval" {
        return Some(Kind::Gate);
    }
    if status == "running" {
        return Some(Kind::Running);
    }
    if ENDED_RUN_STATUSES.contains(&status) {
        return Some(match status {
            "timed_out" => Kind::Timeout,
            "failed" => Kind::Failed,
            "cancelled" => Kind::Cancelled,
            "interrupted" => Kind::Interrupted,
            // ENDED_RUN_STATUSES holds exactly these four (runs.rs:3266); a fifth arriving here
            // would mean that list changed without this match learning about it.
            _ => return None,
        });
    }
    if TERMINAL_RUN_STATUSES.contains(&status) {
        return Some(match status {
            "completed" => Kind::Completed,
            "superseded" => Kind::Superseded,
            // Every other TERMINAL_RUN_STATUSES entry is also in ENDED_RUN_STATUSES and was
            // handled above; only `completed` and `superseded` fall through to here.
            _ => return None,
        });
    }
    None
}

/// Only ever `"silence"` or `"undetermined"` (spec §6). Never `"wall"`: affirming the wall clock
/// fired would require knowing when the run actually started, and `runs` has no `started_at` column
/// to answer that from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    Silence,
    Undetermined,
}

/// The fixed literal `measured_from` reports (spec §5, §6): `elapsed_seconds` is always measured
/// from `created_at`, because `runs` has no `started_at` to measure from instead.
pub const MEASURED_FROM: &str = "created_at";

/// The `timeout` object of the response (spec §5, §5.1).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TimeoutPayload {
    pub elapsed_seconds: i64,
    pub wall_ceiling_seconds: i64,
    pub silence_ceiling_seconds: i64,
    pub measured_from: &'static str,
    pub verdict: Verdict,
}

/// PURE: derives the timeout verdict from elapsed time and `mode` (spec §6).
///
/// The two ceilings come from `runs::run_timeout_for_mode` and `runs::progress_timeout_for_mode`
/// (themselves built on `runs::runs_unattended`) rather than a second `base * multiplier` computed
/// here — two copies of that arithmetic is exactly the divergence spec §6 says this feature exists
/// to make visible, so there is only ever the one.
///
/// The verdict itself compares `elapsed_seconds` against the WALL ceiling only. `elapsed_seconds`
/// includes any time the run spent queued, so it is always a majorant of how long the run actually
/// ran. If even that majorant has not reached the wall ceiling, the real running time cannot have
/// reached it either, and the wall clock is ruled out — `"silence"`. Once the majorant reaches or
/// passes the wall ceiling, either clock could have fired, and nothing here can tell them apart —
/// `"undetermined"`. There is no third case.
pub(crate) fn derive_timeout_verdict(elapsed_seconds: i64, mode: &str) -> TimeoutPayload {
    let wall_ceiling_seconds =
        crate::runs::run_timeout_for_mode(crate::state::DEFAULT_RUN_TIMEOUT, mode).as_secs() as i64;
    let silence_ceiling_seconds =
        crate::runs::progress_timeout_for_mode(crate::state::DEFAULT_PROGRESS_TIMEOUT, mode)
            .as_secs() as i64;
    let verdict = if elapsed_seconds < wall_ceiling_seconds {
        Verdict::Silence
    } else {
        Verdict::Undetermined
    };
    TimeoutPayload {
        elapsed_seconds,
        wall_ceiling_seconds,
        silence_ceiling_seconds,
        measured_from: MEASURED_FROM,
        verdict,
    }
}

/// PURE: whether this run's mode ever produces a `shadow_decisions` row at all (spec §5.3).
///
/// A read of `mode` rather than a `COUNT(*) FROM shadow_decisions` — `hooks.rs` only inserts a row
/// for `mode == "shadow"` or `mode == "worktree"`, so a `real` run's classifier verdicts are never
/// written. A zero-row count cannot tell "nothing was decided" from "everything was decided and none
/// of it was ever written", and the second is what is actually true for a `real` run: the gate saw
/// every tool call and recorded none of them. `decisions_recorded` exists so the UI can say that,
/// instead of reading an empty `leading_up` as "this run's gate did nothing".
pub(crate) fn decisions_recorded(mode: &str) -> bool {
    mode != "real"
}

/// Whether `kind` is one of the two the response carries `leading_up` for (spec §5.1): only `gate`
/// and `timeout` bring the decisions leading up to the stop; every other kind's `leading_up` is
/// `null`, not an omitted field and not an empty array standing in for "not applicable".
pub(crate) fn kind_shows_leading_up(kind: Kind) -> bool {
    matches!(kind, Kind::Gate | Kind::Timeout)
}

/// One `shadow_decisions` row as the response reports it (spec §5.2): the same columns the table
/// holds, except `tool_input` — which can be a whole file — is truncated, and the truncation is
/// declared by the sibling `tool_input_truncated` rather than folded into the field as a marker
/// string a reader could mistake for real content.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct GateDecisionView {
    pub tool_name: String,
    pub action_class: String,
    pub decision: String,
    pub reason: Option<String>,
    pub classifier_version: i64,
    pub policy_digest: Option<String>,
    pub tool_input: Option<String>,
    pub tool_input_truncated: bool,
    pub created_at: String,
}

/// The full `GET /runs/{id}/stop` response (spec §5).
///
/// Every kind-specific field is present and `None` rather than omitted for a kind that does not use
/// it (spec §5.1: "os campos ausentes vão a null e não omitidos"), so the shell never has to tell
/// "not applicable" apart from "the daemon stopped sending this field".
#[derive(Debug, Clone, Serialize)]
pub struct RunStopResponse {
    pub run_id: i64,
    pub status: String,
    pub kind: Kind,
    /// A readable sentence, always present — even for `completed` and `running`, which are not a
    /// stop in the sense the rest of the response describes (spec §5.1).
    pub summary: String,
    pub decisions_recorded: bool,
    pub gate: Option<GateDecisionView>,
    pub timeout: Option<TimeoutPayload>,
    /// The `N` decisions before the last one, most recent first — only for `kind: gate` and
    /// `kind: timeout` (spec §5.1, §5.2).
    pub leading_up: Option<Vec<GateDecisionView>>,
    /// `kind: failed` only.
    pub exit_code: Option<i64>,
    /// `kind: failed` only — the tail of `stderr`.
    pub stderr_tail: Option<String>,
    /// `kind: superseded` only.
    pub successor_run_id: Option<i64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_status_the_spec_table_lists_maps_to_its_own_kind() {
        let table = [
            ("awaiting_approval", Kind::Gate),
            ("timed_out", Kind::Timeout),
            ("failed", Kind::Failed),
            ("cancelled", Kind::Cancelled),
            ("interrupted", Kind::Interrupted),
            ("superseded", Kind::Superseded),
            ("completed", Kind::Completed),
            ("running", Kind::Running),
        ];
        assert_eq!(table.len(), 8, "spec §5.1 lists eight statuses");
        for (status, expected) in table {
            assert_eq!(derive_kind(status), Some(expected), "status: {status}");
        }
    }

    /// Built from the same constants `derive_kind` reads, rather than a second hand-typed list of
    /// eight statuses — so a status added to either list in `runs.rs` without a matching arm here
    /// fails this test instead of silently falling through to `None`.
    #[test]
    fn every_status_ended_run_statuses_or_terminal_run_statuses_names_maps_to_a_kind() {
        let live_statuses = ["awaiting_approval", "running"];
        let known_statuses: std::collections::BTreeSet<&str> = ENDED_RUN_STATUSES
            .iter()
            .copied()
            .chain(TERMINAL_RUN_STATUSES.iter().copied())
            .chain(live_statuses)
            .collect();
        assert_eq!(known_statuses.len(), 8, "eight distinct real statuses");
        for status in known_statuses {
            assert!(
                derive_kind(status).is_some(),
                "status {status} (from runs.rs's own status lists) has no kind"
            );
        }
    }

    #[test]
    fn a_status_no_list_recognises_has_no_kind() {
        assert_eq!(derive_kind("not_a_real_status"), None);
    }

    /// The worked example from spec §6: run 900383, 37 minutes elapsed against an autonomous run's
    /// 120-minute wall ceiling. The measured elapsed time (a majorant of how long the run actually
    /// ran) has not reached the ceiling, so the wall clock is ruled out.
    #[test]
    fn run_900383_thirty_seven_minutes_against_a_120_minute_ceiling_is_silence() {
        let elapsed_seconds = 37 * 60;
        let payload = derive_timeout_verdict(elapsed_seconds, "worktree");

        assert_eq!(payload.wall_ceiling_seconds, 120 * 60);
        assert_eq!(payload.verdict, Verdict::Silence);
    }

    /// Elapsed at or past the wall ceiling: either clock could have fired, so the verdict says so
    /// rather than guessing which one.
    #[test]
    fn elapsed_at_or_past_the_wall_ceiling_is_undetermined() {
        // An interactive (non-unattended) mode: the wall ceiling is the unmultiplied base.
        let payload = derive_timeout_verdict(700, "real");
        assert_eq!(payload.wall_ceiling_seconds, 600);
        assert_eq!(payload.verdict, Verdict::Undetermined);

        // Exactly on the ceiling is still "reached it", not "just under".
        let boundary = derive_timeout_verdict(600, "real");
        assert_eq!(boundary.verdict, Verdict::Undetermined);
    }

    #[test]
    fn an_unattended_mode_gets_the_multiplied_ceilings_an_interactive_one_does_not() {
        let unattended = derive_timeout_verdict(0, "shadow");
        let interactive = derive_timeout_verdict(0, "real");

        assert_eq!(unattended.wall_ceiling_seconds, 7200);
        assert_eq!(interactive.wall_ceiling_seconds, 600);
        assert_ne!(
            unattended.silence_ceiling_seconds,
            interactive.silence_ceiling_seconds
        );
    }

    #[test]
    fn no_verdict_is_ever_the_literal_wall() {
        for (elapsed_seconds, mode) in [
            (0, "real"),
            (10_000, "real"),
            (0, "worktree"),
            (99_999, "shadow"),
        ] {
            let payload = derive_timeout_verdict(elapsed_seconds, mode);
            assert_ne!(
                serde_json::to_value(payload.verdict).unwrap(),
                serde_json::json!("wall"),
                "elapsed={elapsed_seconds} mode={mode}"
            );
        }
    }

    #[test]
    fn only_a_real_run_does_not_record_decisions() {
        assert!(!decisions_recorded("real"));
        assert!(decisions_recorded("shadow"));
        assert!(decisions_recorded("worktree"));
    }

    #[test]
    fn only_gate_and_timeout_show_leading_up() {
        assert!(kind_shows_leading_up(Kind::Gate));
        assert!(kind_shows_leading_up(Kind::Timeout));
        for other in [
            Kind::Failed,
            Kind::Cancelled,
            Kind::Interrupted,
            Kind::Superseded,
            Kind::Completed,
            Kind::Running,
        ] {
            assert!(!kind_shows_leading_up(other), "{other:?}");
        }
    }

    /// Spec §5.1: "os campos ausentes vão a null e não omitidos". A `cancelled` response uses only
    /// `summary` — every other kind-specific field must still serialize as `null`, not disappear.
    #[test]
    fn a_kind_specific_field_this_kind_does_not_use_serialises_as_null_not_as_an_omitted_key() {
        let response = RunStopResponse {
            run_id: 1,
            status: "cancelled".to_owned(),
            kind: Kind::Cancelled,
            summary: "the run was cancelled".to_owned(),
            decisions_recorded: true,
            gate: None,
            timeout: None,
            leading_up: None,
            exit_code: None,
            stderr_tail: None,
            successor_run_id: None,
        };

        let value = serde_json::to_value(&response).unwrap();
        for key in [
            "gate",
            "timeout",
            "leading_up",
            "exit_code",
            "stderr_tail",
            "successor_run_id",
        ] {
            assert!(
                value.get(key).is_some(),
                "{key} must be present, even as null"
            );
            assert!(
                value[key].is_null(),
                "{key} should be null for kind cancelled"
            );
        }
    }

    /// Spec §5.2: truncation is declared by the sibling boolean, not folded into `tool_input` as a
    /// marker string a reader could mistake for real content.
    #[test]
    fn tool_input_truncation_is_a_sibling_boolean_not_a_marker_in_the_field_itself() {
        let view = GateDecisionView {
            tool_name: "Bash".to_owned(),
            action_class: "read-local".to_owned(),
            decision: "allow".to_owned(),
            reason: Some("read-only".to_owned()),
            classifier_version: 11,
            policy_digest: Some("abc123".to_owned()),
            tool_input: Some("{\"command\":\"ls\"}".to_owned()),
            tool_input_truncated: true,
            created_at: "2026-08-29T00:00:00Z".to_owned(),
        };

        let value = serde_json::to_value(&view).unwrap();
        assert_eq!(value["tool_input_truncated"], serde_json::json!(true));
        assert_eq!(
            value["tool_input"],
            serde_json::json!("{\"command\":\"ls\"}")
        );
    }
}
