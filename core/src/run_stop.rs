//! Pure, DB-free derivation logic for `GET /runs/{id}/stop` (spec
//! `.ai/specs/2026-08-29-porque-parou-design.md` §7): "porque parou" — why a run stopped.
//!
//! `runs.rs` is over 3900 lines and `http.rs` is over 11000; nothing about this report belongs in
//! either. This module is responsible for the parts that take state and times in and hand a verdict
//! out — deriving the response's `kind` from a run's `status`, deriving the timeout verdict from
//! elapsed time and the two independent ceilings, and the response's own shapes. Reading the run row
//! and the `shadow_decisions` rows stays in the HTTP handler, which calls the functions here to turn
//! what it read into a response.

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
pub fn derive_kind(status: &str) -> Option<Kind> {
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
pub fn derive_timeout_verdict(elapsed_seconds: i64, mode: &str) -> TimeoutPayload {
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
pub fn decisions_recorded(mode: &str) -> bool {
    mode != "real"
}

/// Whether `kind` is one of the two the response carries `leading_up` for (spec §5.1): only `gate`
/// and `timeout` bring the decisions leading up to the stop; every other kind's `leading_up` is
/// `null`, not an omitted field and not an empty array standing in for "not applicable".
pub fn kind_shows_leading_up(kind: Kind) -> bool {
    matches!(kind, Kind::Gate | Kind::Timeout)
}

/// Whether `kind` is the one the response carries the `gate` object for (spec §5.1's table: only
/// the `awaiting_approval` row lists "objecto `gate`" in its "traz" column — the `timed_out` row
/// lists only "objecto `timeout` + `leading_up`"). A `timeout` stop was not caused by any one
/// decision the way a pending gate is, so nothing is singled out of `leading_up` for it: `gate`
/// stays `null` and every fetched decision — not `leading_up.len() + 1` of them — lands in
/// `leading_up`.
pub fn kind_shows_gate(kind: Kind) -> bool {
    matches!(kind, Kind::Gate)
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

/// How much of a raw `tool_input` a [`GateDecisionView`] carries verbatim before it is cut (spec
/// §5.2: "pode ser um ficheiro inteiro"). Not spec-fixed; chosen to keep one decision readable
/// without making a `leading_up` list of a hundred of them heavy.
const TOOL_INPUT_PREVIEW_CHARS: usize = 2000;

/// How much of a failed run's `stderr` the report keeps, counted from the END (spec §5.1: "cauda do
/// `stderr`"). Nothing upstream bounds this field — `runner.rs` drains the child's stderr pipe with
/// `read_to_string`, so what a subprocess wrote is what the `runs` row holds — and this route is a
/// summary, not the detail view: `GET /runs/{id}` is where the whole of it stays readable.
///
/// The same ceiling as `TOOL_INPUT_PREVIEW_CHARS` beside it, deliberately. Both answer the same
/// question — how much raw text one field of a summary may carry — and a second number here would
/// be a second thing to justify, with nothing to justify it from: neither is spec-fixed.
const STDERR_TAIL_CHARS: usize = TOOL_INPUT_PREVIEW_CHARS;

/// PURE: the last [`STDERR_TAIL_CHARS`] characters of `value`, or all of it when it is already
/// shorter. Counted in `chars` and not bytes, like the `tool_input` cut above: a byte-indexed slice
/// of arbitrary subprocess output lands mid-character sooner or later, and the panic it raises would
/// be reported as the stop route failing rather than as the encoding trap it is.
fn tail_of(value: String) -> String {
    let total = value.chars().count();
    if total <= STDERR_TAIL_CHARS {
        return value;
    }
    value.chars().skip(total - STDERR_TAIL_CHARS).collect()
}

/// PURE: builds one [`GateDecisionView`] from a `shadow_decisions` row's own columns, applying the
/// §5.2 truncation. The HTTP handler reads the row; this is the shaping of it into the response.
#[allow(clippy::too_many_arguments)]
pub fn gate_decision_view(
    tool_name: String,
    action_class: String,
    decision: String,
    reason: Option<String>,
    classifier_version: i64,
    policy_digest: Option<String>,
    tool_input: Option<String>,
    created_at: String,
) -> GateDecisionView {
    let tool_input_truncated = tool_input
        .as_deref()
        .is_some_and(|raw| raw.chars().count() > TOOL_INPUT_PREVIEW_CHARS);
    let tool_input = if tool_input_truncated {
        tool_input.map(|raw| raw.chars().take(TOOL_INPUT_PREVIEW_CHARS).collect())
    } else {
        tool_input
    };
    GateDecisionView {
        tool_name,
        action_class,
        decision,
        reason,
        classifier_version,
        policy_digest,
        tool_input,
        tool_input_truncated,
        created_at,
    }
}

/// The one sentence every response carries (spec §5: "sempre presente"), one per `kind` — even
/// `completed` and `running`, which are not a stop in the sense the rest of the response describes.
fn summary_for(kind: Kind) -> &'static str {
    match kind {
        Kind::Gate => "the run is paused, waiting for a human to approve or deny a tool call",
        Kind::Timeout => "the run timed out",
        Kind::Failed => "the run failed",
        Kind::Cancelled => "the run was cancelled",
        Kind::Interrupted => "the run was interrupted",
        Kind::Superseded => "the run was superseded by a later run",
        Kind::Completed => "the run completed",
        Kind::Running => "the run is still running",
    }
}

/// PURE: assembles the full [`RunStopResponse`] (spec §5, §7's "montar a resposta") from `kind`
/// plus everything the HTTP handler read — the run's own columns and, for `gate` and `timeout`
/// kinds, the `shadow_decisions` rows newest-first. `kind` arrives already derived rather than
/// being derived again in here, because the handler needs it first anyway, to decide whether the
/// `shadow_decisions` query is worth making.
///
/// `decisions` splits in one place: for `kind: gate` only, its first row (the most recent) becomes
/// `gate` and the rest become `leading_up`; for `kind: timeout`, nothing is singled out — every row
/// passed in becomes `leading_up` and `gate` stays `None` (spec §5.1's table credits `timed_out`
/// with "objecto `timeout` + `leading_up`" only, not `gate`). Every other kind gets `None` for both,
/// per spec §5.1's "os campos ausentes vão a null e não omitidos". Every other kind-specific field
/// follows the same rule: present in the struct, `None` unless `kind` is the one kind that uses it.
///
/// Callers must size `decisions` to match: `leading + 1` rows for `kind: gate` (one becomes `gate`,
/// `leading` remain), `leading` rows for `kind: timeout` (all become `leading_up`) — otherwise
/// `leading_up`'s length silently drifts from what `?leading=` promised.
#[allow(clippy::too_many_arguments)]
pub fn build_response(
    run_id: i64,
    status: String,
    kind: Kind,
    mode: &str,
    elapsed_seconds: i64,
    exit_code: Option<i64>,
    stderr_tail: Option<String>,
    successor_run_id: Option<i64>,
    decisions: Vec<GateDecisionView>,
) -> RunStopResponse {
    let mut decisions = decisions.into_iter();
    let gate = if kind_shows_gate(kind) {
        decisions.next()
    } else {
        None
    };
    let leading_up = if kind_shows_leading_up(kind) {
        Some(decisions.collect())
    } else {
        None
    };
    let timeout =
        matches!(kind, Kind::Timeout).then(|| derive_timeout_verdict(elapsed_seconds, mode));

    RunStopResponse {
        run_id,
        status,
        kind,
        summary: summary_for(kind).to_owned(),
        decisions_recorded: decisions_recorded(mode),
        gate,
        timeout,
        leading_up,
        exit_code: if kind == Kind::Failed {
            exit_code
        } else {
            None
        },
        stderr_tail: if kind == Kind::Failed {
            stderr_tail.map(tail_of)
        } else {
            None
        },
        successor_run_id: if kind == Kind::Superseded {
            successor_run_id
        } else {
            None
        },
    }
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

    fn decision(tool_input: Option<&str>) -> GateDecisionView {
        gate_decision_view(
            "Bash".to_owned(),
            "read-local".to_owned(),
            "allow".to_owned(),
            Some("read-only".to_owned()),
            11,
            Some("abc123".to_owned()),
            tool_input.map(str::to_owned),
            "2026-08-29T00:00:00Z".to_owned(),
        )
    }

    #[test]
    fn a_short_tool_input_passes_through_unmarked() {
        let view = decision(Some(r#"{"command":"ls"}"#));
        assert_eq!(view.tool_input.as_deref(), Some(r#"{"command":"ls"}"#));
        assert!(!view.tool_input_truncated);
    }

    #[test]
    fn a_tool_input_over_the_preview_cap_is_cut_and_flagged() {
        let huge = "x".repeat(TOOL_INPUT_PREVIEW_CHARS * 2);
        let view = decision(Some(&huge));
        assert_eq!(
            view.tool_input.as_ref().map(String::len),
            Some(TOOL_INPUT_PREVIEW_CHARS)
        );
        assert!(view.tool_input_truncated);
    }

    #[test]
    fn no_tool_input_at_all_is_not_a_truncation() {
        let view = decision(None);
        assert_eq!(view.tool_input, None);
        assert!(!view.tool_input_truncated);
    }

    /// Spec §5.1: only `gate` and `timeout` carry `gate`/`leading_up`; the other six kinds carry
    /// neither, whatever `decisions` the handler happened to pass in.
    #[test]
    fn only_the_gate_kind_singles_out_a_gate_from_decisions() {
        let decisions = vec![decision(Some("most recent")), decision(Some("older"))];

        let gate_response = build_response(
            1,
            "irrelevant".to_owned(),
            Kind::Gate,
            "shadow",
            0,
            None,
            None,
            None,
            decisions.clone(),
        );
        assert_eq!(
            gate_response.gate.as_ref().map(|g| &g.tool_input),
            Some(&Some("most recent".to_owned()))
        );
        assert_eq!(gate_response.leading_up.map(|rest| rest.len()), Some(1));

        for kind in [
            Kind::Failed,
            Kind::Cancelled,
            Kind::Interrupted,
            Kind::Superseded,
            Kind::Completed,
            Kind::Running,
        ] {
            let response = build_response(
                1,
                "irrelevant".to_owned(),
                kind,
                "shadow",
                0,
                None,
                None,
                None,
                decisions.clone(),
            );
            assert_eq!(response.gate, None, "{kind:?}");
            assert_eq!(response.leading_up, None, "{kind:?}");
        }
    }

    /// Spec §5.1's table credits `timed_out` with "objecto `timeout` + `leading_up`" only — not
    /// `gate`. A timeout was not caused by any one decision the way a pending gate is, so nothing is
    /// singled out: every row the caller passes in lands in `leading_up`, and `gate` stays `None`
    /// even though `shadow_decisions` rows exist for the run.
    #[test]
    fn the_timeout_kind_never_singles_out_a_gate() {
        let decisions = vec![decision(Some("most recent")), decision(Some("older"))];

        let response = build_response(
            1,
            "timed_out".to_owned(),
            Kind::Timeout,
            "shadow",
            0,
            None,
            None,
            None,
            decisions,
        );
        assert_eq!(response.gate, None);
        assert_eq!(response.leading_up.map(|rest| rest.len()), Some(2));
    }

    /// Spec §5.1: each kind-specific field is `Some` only for the one kind that uses it.
    #[test]
    fn kind_specific_fields_are_populated_only_for_their_own_kind() {
        let failed = build_response(
            1,
            "failed".to_owned(),
            Kind::Failed,
            "real",
            0,
            Some(1),
            Some("boom".to_owned()),
            None,
            vec![],
        );
        assert_eq!(failed.exit_code, Some(1));
        assert_eq!(failed.stderr_tail.as_deref(), Some("boom"));
        assert_eq!(failed.successor_run_id, None);

        let superseded = build_response(
            1,
            "superseded".to_owned(),
            Kind::Superseded,
            "real",
            0,
            Some(1),
            Some("boom".to_owned()),
            Some(2),
            vec![],
        );
        assert_eq!(superseded.exit_code, None);
        assert_eq!(superseded.stderr_tail, None);
        assert_eq!(superseded.successor_run_id, Some(2));
    }

    /// Spec §5.1 calls this field "cauda do `stderr`", and a run's `stderr` has no ceiling anywhere
    /// before it: `runner.rs` drains the child's pipe with `read_to_string` into a `String` that
    /// grows to whatever the subprocess wrote. A summary report that hands that back whole is a log
    /// viewer wearing the wrong field name, so what survives the cut is the END — where a failure
    /// says what it was.
    #[test]
    fn a_stderr_past_the_ceiling_keeps_its_tail_and_not_its_head() {
        let huge = format!("{}THE ACTUAL ERROR", "x".repeat(STDERR_TAIL_CHARS * 2));

        let response = build_response(
            1,
            "failed".to_owned(),
            Kind::Failed,
            "real",
            0,
            Some(1),
            Some(huge.clone()),
            None,
            vec![],
        );

        let tail = response.stderr_tail.expect("kind: failed carries it");
        assert_eq!(tail.chars().count(), STDERR_TAIL_CHARS);
        assert!(
            tail.ends_with("THE ACTUAL ERROR"),
            "the end is what was kept"
        );
        assert!(
            huge.ends_with(&tail),
            "the cut must keep a suffix, verbatim"
        );
    }

    /// The cut is a ceiling, not a reshaping: anything already short enough comes back untouched,
    /// which is what every real `stderr` in this database is today.
    #[test]
    fn a_stderr_within_the_ceiling_is_handed_back_whole() {
        let response = build_response(
            1,
            "failed".to_owned(),
            Kind::Failed,
            "real",
            0,
            Some(1),
            Some("boom".to_owned()),
            None,
            vec![],
        );
        assert_eq!(response.stderr_tail.as_deref(), Some("boom"));
    }

    /// A cut that landed mid-character would produce bytes no JSON encoder can emit. Counting in
    /// `chars` rather than bytes is what makes that unrepresentable, and this holds it: the ceiling
    /// is reached with multi-byte characters only.
    #[test]
    fn a_multibyte_stderr_is_cut_on_a_character_and_never_mid_byte() {
        let huge = "é".repeat(STDERR_TAIL_CHARS * 2);

        let response = build_response(
            1,
            "failed".to_owned(),
            Kind::Failed,
            "real",
            0,
            Some(1),
            Some(huge.clone()),
            None,
            vec![],
        );

        let tail = response.stderr_tail.expect("kind: failed carries it");
        assert_eq!(tail.chars().count(), STDERR_TAIL_CHARS);
        assert!(huge.ends_with(&tail));
    }

    #[test]
    fn build_response_derives_the_timeout_payload_only_for_the_timeout_kind() {
        let timeout = build_response(
            1,
            "timed_out".to_owned(),
            Kind::Timeout,
            "worktree",
            37 * 60,
            None,
            None,
            None,
            vec![],
        );
        assert_eq!(
            timeout.timeout.map(|payload| payload.verdict),
            Some(Verdict::Silence)
        );

        let completed = build_response(
            1,
            "completed".to_owned(),
            Kind::Completed,
            "worktree",
            37 * 60,
            None,
            None,
            None,
            vec![],
        );
        assert_eq!(completed.timeout, None);
    }
}
