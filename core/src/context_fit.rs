//! The context-fit card of a work node (spec 2026-11-12, sections 2 and 4): did the node get too
//! little context (`under`), too much (`over`), or a task cut badly (`split`)?
//!
//! **Pure.** [`assess`] reads an [`Inputs`] the IO half (`store`) gathered from rows already in the
//! database and answers a [`Card`]: the verdicts, the evidence of each signal that fired, and a
//! short given/used summary. An empty verdict list is `fit`. A signal whose source had no data
//! arrives as `None` and abstains; it never fires and never errors. Every threshold comes from
//! [`Thresholds`] (the `context_fit` section of `devtime.yaml`); the only numbers written here are
//! the "one is enough" counts of the signals that are a plain yes/no.

pub mod learn;
pub mod store;

use serde::Serialize;
use serde_json::{Map, Value, json};

pub use crate::config::DevtimeContextFitConfig as Thresholds;

/// Bumped when a signal or its meaning changes: a card computed under an older version is
/// recomputed (`store::recompute_outdated`).
///
/// 2: `used` also lists the read paths (`outside_paths`, `pointers_read_paths`,
/// `pointers_unread_paths`) the pointer learner (`learn`) groups across nodes.
pub const RULES_VERSION: i64 = 2;

/// The devtime rule ids that make a node `split` when they fire in its run (s1).
const LOOP_RULES: [&str; 4] = ["D1", "D3", "D7", "D8"];
/// The rule id for a late scope discovery (u5).
const LATE_SCOPE_RULE: &str = "D14";
/// The rule id for a long context (o4).
const LONG_CONTEXT_RULE: &str = "D9";

/// One of the three diagnostics. `fit` is the absence of all of them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Verdict {
    Under,
    Over,
    Split,
}

impl Verdict {
    pub fn as_str(self) -> &'static str {
        match self {
            Verdict::Under => "under",
            Verdict::Over => "over",
            Verdict::Split => "split",
        }
    }
}

/// Everything the signals read for one node. `None` is "no data": the signal abstains.
#[derive(Debug, Clone, Default)]
pub struct Inputs {
    /// The node's layer (`core`, `shell`, ...), the group the o3 percentile is taken over.
    pub layer: String,
    pub owned_paths: Vec<String>,
    pub pointers: Vec<String>,
    /// The tools the run was given (`runs.tools_given`).
    pub tools_given: Option<Vec<String>>,
    /// The first `usage` of the run (`runs.initial_context`).
    pub initial_context: Option<i64>,
    /// The node's ceiling (`work_nodes.budget_tokens`).
    pub context_ceiling: Option<i64>,
    pub context_peak: Option<i64>,
    /// How the node closed: `done`, `blocked`, `handoff`, `failed`.
    pub status: String,
    /// Distinct files read before the first edit that are in neither the owned paths nor the
    /// pointers (u1).
    pub outside_reads_before_edit: Option<u32>,
    /// How many of `pointers` the run read at all (o1).
    pub pointers_read: Option<u32>,
    /// The paths behind `outside_reads_before_edit`, as read.
    pub outside_paths: Vec<String>,
    /// The pointers the run read, and those it never opened.
    pub pointers_read_paths: Vec<String>,
    pub pointers_unread_paths: Vec<String>,
    pub fence_refusals: u32,
    pub tools_requested: u32,
    pub tools_refused: u32,
    pub contract_requests: u32,
    /// The devtime rule ids that fired in this node's run.
    pub d_rules: Vec<String>,
    pub handoffs: u32,
    pub fix_rounds: u32,
    pub rescues: u32,
    pub sibling_merge_conflict: bool,
    /// Total tokens of the node's runs, for the summary.
    pub tokens: Option<i64>,
    /// Tokens per changed line of this node (o3).
    pub tokens_per_line: Option<f64>,
    /// The `o3_pct` percentile of the same layer's nodes over the window, and how many nodes it
    /// was taken over.
    pub layer_p90: Option<f64>,
    pub layer_nodes: u32,
    /// The node closed `blocked` and its reason asks for information (u6).
    pub blocked_asking_info: bool,
}

/// One signal that fired: its id (`u1`..`s5`), the value seen and the threshold it met.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Evidence {
    pub signal: &'static str,
    pub value: f64,
    pub threshold: f64,
}

/// The answer: verdicts (empty = `fit`), the evidence of each, and the two summaries.
#[derive(Debug, Clone, PartialEq)]
pub struct Card {
    pub verdicts: Vec<Verdict>,
    pub evidence: Vec<Evidence>,
    pub given: Value,
    pub used: Value,
}

impl Card {
    pub fn is_fit(&self) -> bool {
        self.verdicts.is_empty()
    }
}

/// The verdict a signal id belongs to, by its letter.
fn verdict_of(signal: &str) -> Verdict {
    match signal.as_bytes().first() {
        Some(b'u') => Verdict::Under,
        Some(b'o') => Verdict::Over,
        _ => Verdict::Split,
    }
}

fn has_rule(rules: &[String], wanted: &str) -> bool {
    rules.iter().any(|rule| rule == wanted)
}

fn share(part: i64, whole: i64) -> Option<f64> {
    (whole > 0).then(|| part as f64 / whole as f64)
}

/// The `pct` percentile of `values` by nearest rank, or `None` for no values.
pub fn percentile(values: &mut [f64], pct: u32) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    values.sort_by(|a, b| a.total_cmp(b));
    let rank = (f64::from(pct.min(100)) / 100.0 * values.len() as f64).ceil() as usize;
    values.get(rank.saturating_sub(1)).copied()
}

/// Computes the card of one node. Pure: the same inputs and thresholds give the same card.
pub fn assess(inputs: &Inputs, cfg: &Thresholds) -> Card {
    let mut fired: Vec<Evidence> = Vec::new();
    let mut fire = |signal: &'static str, value: f64, threshold: f64| {
        fired.push(Evidence {
            signal,
            value,
            threshold,
        });
    };

    // under
    if let Some(reads) = inputs.outside_reads_before_edit
        && reads >= cfg.u1_reads
    {
        fire("u1", f64::from(reads), f64::from(cfg.u1_reads));
    }
    if inputs.fence_refusals >= 1 {
        fire("u2", f64::from(inputs.fence_refusals), 1.0);
    }
    let tool_asks = inputs.tools_requested + inputs.tools_refused;
    if tool_asks >= 1 {
        fire("u3", f64::from(tool_asks), 1.0);
    }
    if inputs.contract_requests >= 1 {
        fire("u4", f64::from(inputs.contract_requests), 1.0);
    }
    if has_rule(&inputs.d_rules, LATE_SCOPE_RULE) {
        fire("u5", 1.0, 1.0);
    }
    if inputs.blocked_asking_info {
        fire("u6", 1.0, 1.0);
    }

    // over
    if let Some(read) = inputs.pointers_read
        && !inputs.pointers.is_empty()
    {
        let total = inputs.pointers.len();
        let unread = total.saturating_sub(read as usize);
        let unread_share = unread as f64 / total as f64;
        if unread_share >= cfg.o1_unread_share {
            fire("o1", unread_share, cfg.o1_unread_share);
        }
    }
    if let (Some(initial), Some(ceiling)) = (inputs.initial_context, inputs.context_ceiling)
        && let Some(initial_share) = share(initial, ceiling)
        && initial_share > cfg.o2_initial_share
    {
        fire("o2", initial_share, cfg.o2_initial_share);
    }
    if let (Some(value), Some(p)) = (inputs.tokens_per_line, inputs.layer_p90)
        && inputs.layer_nodes >= cfg.o3_min_nodes
        && value > p
    {
        fire("o3", value, p);
    }
    if has_rule(&inputs.d_rules, LONG_CONTEXT_RULE) {
        fire("o4", 1.0, 1.0);
    }

    // split
    let loops = inputs
        .d_rules
        .iter()
        .filter(|rule| LOOP_RULES.contains(&rule.as_str()))
        .count();
    if loops >= 1 {
        fire("s1", loops as f64, 1.0);
    }
    if inputs.handoffs >= cfg.s2_handoffs {
        fire("s2", f64::from(inputs.handoffs), f64::from(cfg.s2_handoffs));
    }
    let rounds = inputs.fix_rounds + inputs.rescues;
    if rounds >= 1 {
        fire("s3", f64::from(rounds), 1.0);
    }
    if inputs.sibling_merge_conflict {
        fire("s4", 1.0, 1.0);
    }
    if let (Some(peak), Some(ceiling)) = (inputs.context_peak, inputs.context_ceiling)
        && let Some(peak_share) = share(peak, ceiling)
        && peak_share >= cfg.s5_peak_share
        && inputs.status != "done"
    {
        fire("s5", peak_share, cfg.s5_peak_share);
    }

    let mut verdicts = Vec::new();
    for verdict in [Verdict::Under, Verdict::Over, Verdict::Split] {
        if fired.iter().any(|e| verdict_of(e.signal) == verdict) {
            verdicts.push(verdict);
        }
    }
    Card {
        verdicts,
        evidence: fired,
        given: given_of(inputs),
        used: used_of(inputs),
    }
}

fn given_of(inputs: &Inputs) -> Value {
    json!({
        "owned_paths": inputs.owned_paths.len(),
        "pointers": inputs.pointers.len(),
        "tools": inputs.tools_given.as_ref().map(Vec::len),
        "initial_context": inputs.initial_context,
        "ceiling": inputs.context_ceiling,
    })
}

fn used_of(inputs: &Inputs) -> Value {
    let mut used = Map::new();
    used.insert("status".into(), json!(inputs.status));
    used.insert(
        "outside_reads_before_edit".into(),
        json!(inputs.outside_reads_before_edit),
    );
    used.insert("pointers_read".into(), json!(inputs.pointers_read));
    used.insert("outside_paths".into(), json!(inputs.outside_paths));
    used.insert(
        "pointers_read_paths".into(),
        json!(inputs.pointers_read_paths),
    );
    used.insert(
        "pointers_unread_paths".into(),
        json!(inputs.pointers_unread_paths),
    );
    used.insert("fence_refusals".into(), json!(inputs.fence_refusals));
    used.insert("tools_requested".into(), json!(inputs.tools_requested));
    used.insert("tools_refused".into(), json!(inputs.tools_refused));
    used.insert("contract_requests".into(), json!(inputs.contract_requests));
    used.insert("findings".into(), json!(inputs.d_rules));
    used.insert("handoffs".into(), json!(inputs.handoffs));
    used.insert("fix_rounds".into(), json!(inputs.fix_rounds));
    used.insert("rescues".into(), json!(inputs.rescues));
    used.insert("peak".into(), json!(inputs.context_peak));
    used.insert("tokens".into(), json!(inputs.tokens));
    used.insert("tokens_per_line".into(), json!(inputs.tokens_per_line));
    Value::Object(used)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> Thresholds {
        Thresholds::default()
    }

    /// A node that trips nothing: it read its pointers, started small, closed done.
    fn clean() -> Inputs {
        Inputs {
            layer: "core".into(),
            owned_paths: vec!["core/src/a.rs".into()],
            pointers: vec!["core/src/b.rs".into(), "core/src/c.rs".into()],
            tools_given: Some(vec!["Read".into(), "Edit".into()]),
            initial_context: Some(100),
            context_ceiling: Some(1000),
            context_peak: Some(300),
            status: "done".into(),
            outside_reads_before_edit: Some(0),
            pointers_read: Some(2),
            tokens: Some(5000),
            tokens_per_line: Some(10.0),
            layer_p90: Some(50.0),
            layer_nodes: 100,
            ..Inputs::default()
        }
    }

    fn signals(inputs: &Inputs) -> Vec<&'static str> {
        assess(inputs, &cfg())
            .evidence
            .iter()
            .map(|e| e.signal)
            .collect()
    }

    /// The signal fires alone for `bad` and not for `ok`; the verdict follows its letter.
    fn alone(signal: &str, ok: Inputs, bad: Inputs) {
        assert_eq!(signals(&ok), Vec::<&str>::new(), "{signal} below");
        assert_eq!(signals(&bad), vec![signal], "{signal} at the threshold");
        let card = assess(&bad, &cfg());
        assert_eq!(card.verdicts, vec![verdict_of(signal)], "{signal}");
    }

    #[test]
    fn a_clean_node_is_fit() {
        let card = assess(&clean(), &cfg());
        assert!(card.is_fit());
        assert!(card.evidence.is_empty());
    }

    #[test]
    fn u1_reads_outside_the_given_before_the_first_edit() {
        let mut ok = clean();
        ok.outside_reads_before_edit = Some(cfg().u1_reads - 1);
        let mut bad = clean();
        bad.outside_reads_before_edit = Some(cfg().u1_reads);
        alone("u1", ok, bad);
    }

    #[test]
    fn u1_abstains_without_data() {
        let mut none = clean();
        none.outside_reads_before_edit = None;
        assert!(assess(&none, &cfg()).is_fit());
    }

    #[test]
    fn u2_a_fence_refusal() {
        let mut bad = clean();
        bad.fence_refusals = 1;
        alone("u2", clean(), bad);
    }

    #[test]
    fn u3_a_tool_requested_or_refused() {
        let mut asked = clean();
        asked.tools_requested = 1;
        alone("u3", clean(), asked);
        let mut refused = clean();
        refused.tools_refused = 1;
        alone("u3", clean(), refused);
    }

    #[test]
    fn u4_a_contract_request() {
        let mut bad = clean();
        bad.contract_requests = 1;
        alone("u4", clean(), bad);
    }

    #[test]
    fn u5_late_scope() {
        let mut bad = clean();
        bad.d_rules = vec!["D14".into()];
        alone("u5", clean(), bad);
    }

    #[test]
    fn u6_blocked_asking_for_information() {
        let mut bad = clean();
        bad.blocked_asking_info = true;
        alone("u6", clean(), bad);
    }

    #[test]
    fn o1_unread_pointers_at_the_share() {
        // Two pointers: one unread is 0.5, the default share; none unread is 0.
        let mut ok = clean();
        ok.pointers_read = Some(2);
        let mut bad = clean();
        bad.pointers_read = Some(1);
        alone("o1", ok, bad);
        // Four pointers, three read: 0.25 is below the share.
        let mut four = clean();
        four.pointers = (0..4).map(|i| format!("p{i}")).collect();
        four.pointers_read = Some(3);
        assert!(assess(&four, &cfg()).is_fit());
    }

    #[test]
    fn o1_abstains_without_pointers_or_without_a_read_count() {
        let mut none = clean();
        none.pointers.clear();
        none.pointers_read = Some(0);
        assert!(assess(&none, &cfg()).is_fit());
        let mut unknown = clean();
        unknown.pointers_read = None;
        assert!(assess(&unknown, &cfg()).is_fit());
    }

    #[test]
    fn o2_initial_context_is_strictly_above_the_share_of_the_ceiling() {
        let mut at = clean();
        at.initial_context = Some(400); // exactly 0.4: not over
        assert!(assess(&at, &cfg()).is_fit());
        let mut bad = clean();
        bad.initial_context = Some(401);
        alone("o2", clean(), bad);
    }

    #[test]
    fn o2_abstains_without_a_ceiling() {
        let mut none = clean();
        none.context_ceiling = None;
        none.initial_context = Some(900);
        assert!(assess(&none, &cfg()).is_fit());
    }

    #[test]
    fn o3_tokens_per_line_above_the_layer_percentile() {
        let mut at = clean();
        at.tokens_per_line = Some(50.0); // equal to p90: not above
        assert!(assess(&at, &cfg()).is_fit());
        let mut bad = clean();
        bad.tokens_per_line = Some(50.5);
        alone("o3", clean(), bad);
    }

    #[test]
    fn o3_abstains_below_the_minimum_node_count() {
        let mut few = clean();
        few.tokens_per_line = Some(1000.0);
        few.layer_nodes = cfg().o3_min_nodes - 1;
        assert!(assess(&few, &cfg()).is_fit());
        few.layer_nodes = cfg().o3_min_nodes;
        assert_eq!(signals(&few), vec!["o3"]);
        let mut no_p90 = clean();
        no_p90.tokens_per_line = Some(1000.0);
        no_p90.layer_p90 = None;
        assert!(assess(&no_p90, &cfg()).is_fit());
    }

    #[test]
    fn o4_long_context() {
        let mut bad = clean();
        bad.d_rules = vec!["D9".into()];
        alone("o4", clean(), bad);
    }

    #[test]
    fn s1_a_loop_rule_in_the_run() {
        for rule in ["D1", "D3", "D7", "D8"] {
            let mut bad = clean();
            bad.d_rules = vec![rule.into()];
            alone("s1", clean(), bad);
        }
        let mut other = clean();
        other.d_rules = vec!["D4".into()];
        assert!(assess(&other, &cfg()).is_fit());
    }

    #[test]
    fn s2_handoffs_at_the_threshold() {
        let mut ok = clean();
        ok.handoffs = cfg().s2_handoffs - 1;
        let mut bad = clean();
        bad.handoffs = cfg().s2_handoffs;
        alone("s2", ok, bad);
    }

    #[test]
    fn s3_a_fix_round_or_a_rescue() {
        let mut fix = clean();
        fix.fix_rounds = 1;
        alone("s3", clean(), fix);
        let mut rescue = clean();
        rescue.rescues = 1;
        alone("s3", clean(), rescue);
    }

    #[test]
    fn s4_a_merge_conflict_with_a_sibling() {
        let mut bad = clean();
        bad.sibling_merge_conflict = true;
        alone("s4", clean(), bad);
    }

    #[test]
    fn s5_peak_at_the_share_without_closing_done() {
        // 799 of 1000 is below 0.8; 800 is at it.
        let mut ok = clean();
        ok.context_peak = Some(799);
        ok.status = "handoff".into();
        let mut bad = clean();
        bad.context_peak = Some(800);
        bad.status = "handoff".into();
        alone("s5", ok, bad);
        // The same peak on a node that closed done is not a split.
        let mut done = clean();
        done.context_peak = Some(900);
        assert!(assess(&done, &cfg()).is_fit());
    }

    #[test]
    fn under_and_split_together_keep_both_verdicts_and_their_evidence() {
        let mut both = clean();
        both.fence_refusals = 2;
        both.d_rules = vec!["D1".into()];
        let card = assess(&both, &cfg());
        assert_eq!(card.verdicts, vec![Verdict::Under, Verdict::Split]);
        let ids: Vec<_> = card.evidence.iter().map(|e| e.signal).collect();
        assert_eq!(ids, vec!["u2", "s1"]);
        assert_eq!(card.evidence[0].value, 2.0);
        assert_eq!(card.evidence[0].threshold, 1.0);
    }

    #[test]
    fn the_thresholds_move_with_the_configuration() {
        let mut inputs = clean();
        inputs.outside_reads_before_edit = Some(3);
        let strict = Thresholds {
            u1_reads: 3,
            ..Thresholds::default()
        };
        assert!(assess(&inputs, &Thresholds::default()).is_fit());
        assert_eq!(assess(&inputs, &strict).verdicts, vec![Verdict::Under]);
    }

    #[test]
    fn the_summaries_name_what_was_given_and_used() {
        let card = assess(&clean(), &cfg());
        assert_eq!(card.given["owned_paths"], 1);
        assert_eq!(card.given["pointers"], 2);
        assert_eq!(card.given["tools"], 2);
        assert_eq!(card.given["ceiling"], 1000);
        assert_eq!(card.used["pointers_read"], 2);
        assert_eq!(card.used["tokens"], 5000);
        assert_eq!(card.used["status"], "done");
    }

    #[test]
    fn percentile_is_nearest_rank() {
        let mut values: Vec<f64> = (1..=10).map(f64::from).collect();
        assert_eq!(percentile(&mut values, 90), Some(9.0));
        assert_eq!(percentile(&mut values, 100), Some(10.0));
        assert_eq!(percentile(&mut values, 10), Some(1.0));
        assert_eq!(percentile(&mut [], 90), None);
    }

    #[test]
    fn the_default_thresholds_are_the_spec_values() {
        let t = Thresholds::default();
        assert_eq!(t.u1_reads, 8);
        assert_eq!(t.o1_unread_share, 0.5);
        assert_eq!(t.o2_initial_share, 0.4);
        assert_eq!((t.o3_pct, t.o3_window_days, t.o3_min_nodes), (90, 30, 20));
        assert_eq!(t.s2_handoffs, 2);
        assert_eq!(t.s5_peak_share, 0.8);
    }

    #[test]
    fn the_section_is_optional_and_partial_in_devtime_yaml() {
        let absent = crate::config::parse_devtime_config("enabled: true\n").unwrap();
        assert_eq!(absent.context_fit, Thresholds::default());
        let partial =
            crate::config::parse_devtime_config("context_fit:\n  u1_reads: 12\n").unwrap();
        assert_eq!(partial.context_fit.u1_reads, 12);
        assert_eq!(partial.context_fit.s2_handoffs, 2);
        assert!(crate::config::parse_devtime_config("context_fit:\n  o3_pct: 0\n").is_err());
        assert!(crate::config::parse_devtime_config("context_fit:\n  s5_peak_share: 2\n").is_err());
    }

    /// Criterion 12: a card in well under 50 ms. A thousand cards of a node that fires many
    /// signals must fit in 500 ms, i.e. under 0.5 ms each, a hundredth of the budget per card.
    #[test]
    fn a_card_costs_a_small_fraction_of_the_budget() {
        let mut busy = clean();
        busy.fence_refusals = 3;
        busy.d_rules = vec!["D1".into(), "D9".into(), "D14".into()];
        busy.handoffs = 5;
        busy.pointers_read = Some(0);
        let config = cfg();
        let started = std::time::Instant::now();
        for _ in 0..1000 {
            std::hint::black_box(assess(std::hint::black_box(&busy), &config));
        }
        assert!(
            started.elapsed() < std::time::Duration::from_millis(500),
            "1000 cards took {:?}",
            started.elapsed()
        );
    }
}
