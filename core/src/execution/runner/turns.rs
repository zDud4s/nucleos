//! Cutting a long-lived session into turns, and the usage each turn and run reports.

use super::*;

/// One turn's own numbers, out of a process that may answer more than once.
///
/// `RunOutcome` describes a PROCESS — its exit code, its whole stdout, everything it spent. This
/// describes one answer inside it, which is the unit a conversation is billed and recorded by. The
/// two coincide exactly as long as a process serves one turn, and stop coinciding the moment one
/// serves two.
// `cost_usd` is an `f64`, which is not `Eq`, for the reason `RunOutcome` gives above its own derive.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TurnOutcome {
    pub session_id: Option<String>,
    /// What THIS turn added, never what the process has spent altogether.
    pub cost_usd: Option<f64>,
    pub usage: RunUsage,
    /// Whether the CLI summarised its context while producing THIS turn.
    ///
    /// Per turn and not per process, like the cost above and for the same reason: a process that
    /// serves six turns compacts during one of them, and a flag on the process would mark all six
    /// as the turn where the conversation got shorter.
    pub compacted: bool,
}

/// What one line of a live process's stream means to whoever is recording turns.
///
/// A process that serves one turn needs none of this — its stream IS the turn, and `RunOutcome`
/// describes it. A process that serves several needs somebody to say where one answer ends and the
/// next begins, because the CLI itself only says so in passing, with a `result` line that looks like
/// any other line until it is parsed.
#[derive(Debug, Clone, PartialEq)]
pub enum TurnEvent {
    /// One line belonging to the turn that has not ended yet.
    Line(String),
    /// The turn that was in flight has ended, with its own numbers.
    Ended(TurnOutcome),
}

/// Splits one process's stdout into turns, a line at a time.
///
/// It exists so the rule lives in one place that a test can reach without a subprocess: `execute`
/// feeds it every line and reads back both what to forward and what the turn cost, and nothing else
/// in the daemon has to know that a `result` is a boundary or that the cost on it is cumulative.
pub struct TurnSplitter {
    pub(super) spent: f64,
    /// Belongs to the turn IN FLIGHT, and is cleared when that turn ends.
    ///
    /// It is accumulated rather than read off the `result` line, because it is not on it: the CLI
    /// decides to compact before it answers. Cleared at the boundary and not merely overwritten,
    /// so a process serving six turns does not report the second one's compaction on the four
    /// that follow it.
    pub(super) compacted: bool,
}

impl TurnSplitter {
    pub fn new() -> Self {
        Self {
            spent: 0.0,
            compacted: false,
        }
    }

    /// The events this line produces, in the order a consumer must see them.
    ///
    /// A `result` line produces BOTH — it is the last line of the turn it ends, and it carries the
    /// answer, so a consumer told the turn had ended before being given that line would close every
    /// turn one line short of what it said.
    pub fn line(&mut self, line: String) -> Vec<TurnEvent> {
        self.compacted = compacted_from_line(&line, self.compacted);
        match turn_from_result(&line, self.spent) {
            Some((mut turn, total)) => {
                self.spent = total;
                turn.compacted = std::mem::take(&mut self.compacted);
                vec![TurnEvent::Line(line), TurnEvent::Ended(turn)]
            }
            None => vec![TurnEvent::Line(line)],
        }
    }

    /// Everything the process has reported spending so far, which is what bills the PROCESS rather
    /// than any one turn inside it.
    pub fn spent(&self) -> f64 {
        self.spent
    }
}

/// What one `result` line added, given what the process has already reported spending — and the new
/// running total to carry into the next one. `None` for every line that does not end a turn.
///
/// The two halves are read differently ON PURPOSE, and the reason is measured rather than assumed.
/// Two turns fed down one stdin reported `total_cost_usd` 0.1046 and then 0.2024 — a running total
/// over the process — while `num_turns` read 1 on both and `usage` described only the turn that had
/// just ended. So the cost is differenced and nothing beside it is: differencing the counts would
/// produce a negative number the first time a turn used fewer tokens than the one before it, and
/// recording the cost verbatim would bill each turn for every turn that preceded it.
///
/// The session id is read here as well because a turn is where it becomes true: measured on the same
/// two turns, a live process keeps ONE session across all of them, which is what lets a conversation
/// still be resumed by it after the process is gone.
pub fn turn_from_result(line: &str, already_spent: f64) -> Option<(TurnOutcome, f64)> {
    let line = line.trim();
    let value = serde_json::from_str::<serde_json::Value>(line).ok()?;
    if value.get("type").and_then(|kind| kind.as_str()) != Some("result") {
        return None;
    }
    // `cost_usd` as the fallback spelling for the same reason the stdout loop accepts both: older
    // CLI builds emit it, and a turn whose cost silently read `None` would be a free turn in the
    // ledger.
    let spent = value
        .get("total_cost_usd")
        .or_else(|| value.get("cost_usd"))
        .and_then(serde_json::Value::as_f64);
    let turn = TurnOutcome {
        session_id: value
            .get("session_id")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned),
        cost_usd: spent.map(|total| total - already_spent),
        usage: extract_usage(line),
        // The SPLITTER's to fill: the fact is not on the `result` line this function parses, and
        // inventing it here from nothing would be a quieter way of saying `false`.
        compacted: false,
    };
    // A result carrying no cost at all must not reset the total: the next turn would then be
    // differenced against zero and billed for the whole conversation.
    Some((turn, spent.unwrap_or(already_spent)))
}

pub fn extract_usage(stdout: &str) -> RunUsage {
    let mut usage = RunUsage::default();
    for line in stdout.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(line)
            && value.get("type").and_then(|kind| kind.as_str()) == Some("result")
        {
            usage = RunUsage {
                input_tokens: value
                    .get("usage")
                    .and_then(|result_usage| result_usage.get("input_tokens"))
                    .and_then(serde_json::Value::as_i64),
                output_tokens: value
                    .get("usage")
                    .and_then(|result_usage| result_usage.get("output_tokens"))
                    .and_then(serde_json::Value::as_i64),
                cache_read_tokens: value
                    .get("usage")
                    .and_then(|result_usage| result_usage.get("cache_read_input_tokens"))
                    .and_then(serde_json::Value::as_i64),
                cache_creation_tokens: value
                    .get("usage")
                    .and_then(|result_usage| result_usage.get("cache_creation_input_tokens"))
                    .and_then(serde_json::Value::as_i64),
                num_turns: value.get("num_turns").and_then(serde_json::Value::as_i64),
            };
        }
    }
    usage
}

/// PURE: what a stream that never reached a `result` still says it used.
///
/// [`extract_usage`] reads the `result` event, and a headless run emits exactly one, at the very
/// end. A run stopped before it — a turn ceiling, a progress deadline, a stream that broke — used to
/// write NULL in every column, whatever it had burned: run 900463 was stopped at its ceiling after
/// 125 responses and nearly ten million cache-read tokens, and recorded none of them.
///
/// Every `assistant` event carries its message's `usage`, and three of its fields are exact,
/// because the input side is settled before the model writes a word. Summed once per message — the
/// blocks of one message repeat the same figures, so summing events would count an answer once per
/// block — they matched the `result` of run 900473 to the token: 28 input, 1,023,866 cache read,
/// 68,996 cache creation.
///
/// `output_tokens` is not one of them and stays `None`. The figure on an `assistant` event is a
/// count taken while the message was still being written: the same run's messages summed to 40
/// against a `result` of 8,124. Written down, that would read as measured and be wrong by two
/// orders of magnitude; unknown is the honest value, as it is everywhere else in [`RunUsage`].
///
/// `num_turns` is counted by the same [`TurnCounter`] the ceiling reads, so a run stopped at its
/// ceiling records the number that stopped it. It is not the CLI's own `num_turns`, which counts
/// something else (16 against 14 messages on 900473) and never arrived here anyway.
///
/// And no cost. Nothing here prices tokens, and the budget already charges a run with no cost by
/// how long it ran (`budget::compute_spend`); a figure built from the input side alone would
/// displace that estimate with a smaller one.
pub fn usage_without_a_result(stdout: &str) -> RunUsage {
    let mut turns = TurnCounter::default();
    let mut usage = RunUsage::default();
    for line in stdout.lines() {
        if !turns.line(line) {
            continue;
        }
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line.trim()) else {
            continue;
        };
        let Some(reported) = value
            .get("message")
            .and_then(|message| message.get("usage"))
        else {
            continue;
        };
        let add = |total: &mut Option<i64>, field: &str| {
            if let Some(tokens) = reported.get(field).and_then(serde_json::Value::as_i64) {
                *total = Some(total.unwrap_or(0).saturating_add(tokens));
            }
        };
        add(&mut usage.input_tokens, "input_tokens");
        add(&mut usage.cache_read_tokens, "cache_read_input_tokens");
        add(
            &mut usage.cache_creation_tokens,
            "cache_creation_input_tokens",
        );
    }
    usage.num_turns = (turns.count() > 0).then_some(turns.count());
    usage
}
