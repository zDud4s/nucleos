//! PURE: a session's devtime rows become spans per lane (`main`, `agent:<id>`, `bg:<tool_use_id>`)
//! that partition each lane's wall time, with the gap precedence wait_background > wait_human
//! (up to the idle threshold) > idle. No I/O.
//!
//! One state machine runs per lane over its time-ordered events: a message in flight is `model`,
//! tool calls in flight are one `tool` (or `subagent`) group, and what is left after a message that
//! asked for nothing is a gap, labelled by [`Index::gap`]. A background Bash gets a lane of its own
//! (one `tool` span from launch to its end); a background agent's lane is simply the agent's own
//! records, so nothing here is ever measured from the parent's `tool_use`/`tool_result` pair.
//!
//! Choices the rules leave open:
//! - A lane's events are its messages, tool starts and ends, the human prompts (main only) and the
//!   interrupt / notification / compaction markers. A compaction marker moves no state and cuts no
//!   interval, but it does count towards where the lane ends.
//! - A background attempt's tool span covers only its launch result (`ended_at`); the real end
//!   (`bg_ended_at`) belongs to the work's own lane.
//! - A background lane that would be empty (it ended at its launch instant) gets no span at all:
//!   the lanes promise no zero-length span.
//! - The `idle` threshold counts from where the `wait_background` part of a gap stops, not from the
//!   gap's start.

use std::collections::{BTreeMap, HashMap};
use std::time::Duration;

use chrono::{DateTime, Utc};

use crate::devtime_parse::PARSER_VERSION;
use crate::devtime_store::{AttemptRow, SessionRows, SpanRow};

type Ms = i64;

const MAIN: &str = "main";

fn parse_ts(text: &str) -> Option<Ms> {
    DateTime::parse_from_rfc3339(text)
        .ok()
        .map(|moment| moment.timestamp_millis())
}

fn format_ts(ms: Ms) -> String {
    DateTime::<Utc>::from_timestamp_millis(ms)
        .unwrap_or_default()
        .format("%Y-%m-%dT%H:%M:%S%.3fZ")
        .to_string()
}

fn is_agent(attempt: &AttemptRow) -> bool {
    attempt.kind == "agent" || attempt.tool_name == "Agent" || attempt.tool_name == "Task"
}

fn confidence_of(text: Option<&str>) -> &'static str {
    if text == Some("inferred") {
        "inferred"
    } else {
        "exact"
    }
}

/// A span before it is written out as a row.
#[derive(Debug, Clone)]
struct Piece {
    kind: &'static str,
    start: Ms,
    end: Ms,
    attempt_id: Option<String>,
    attempt_ids: Vec<String>,
    context: Option<i64>,
    confidence: &'static str,
}

impl Piece {
    fn plain(kind: &'static str, start: Ms, end: Ms) -> Piece {
        Piece {
            kind,
            start,
            end,
            attempt_id: None,
            attempt_ids: Vec::new(),
            context: None,
            confidence: "exact",
        }
    }
}

enum Kind {
    Prompt,
    ToolEnd(usize),
    Notification(String),
    Interrupt,
    MsgStart,
    MsgEnd { tool_use: bool, context: i64 },
    ToolStart(usize),
    Compact,
}

impl Kind {
    /// Tie-break among events with the same timestamp. A result lands before the next message
    /// starts, a notification before the resume it causes, and a message ends before its own
    /// `tool_use` starts, so the model span that asked for a tool is the one that carries its context.
    fn order(&self) -> u8 {
        match self {
            Kind::Prompt => 0,
            Kind::ToolEnd(_) => 1,
            Kind::Notification(_) => 2,
            Kind::Interrupt => 3,
            Kind::MsgStart => 4,
            Kind::MsgEnd { .. } => 5,
            Kind::ToolStart(_) => 6,
            Kind::Compact => 7,
        }
    }
}

struct Event {
    ts: Ms,
    kind: Kind,
}

/// What ended a gap.
#[derive(Clone, Copy)]
enum Closed<'a> {
    Human,
    Notification(&'a str),
    Other,
}

/// A background piece of work that outlives its launch.
struct Background {
    attempt_id: String,
    start: Ms,
    end: Ms,
    confidence: &'static str,
}

struct Index {
    idle: Ms,
    /// Keyed by the launching `tool_use_id`, which is also what its notification refers to.
    background: BTreeMap<String, Background>,
    /// The earliest sighting of each `bg_notification`, by the same key.
    notifications: HashMap<String, Ms>,
}

impl Index {
    /// Labels the gap `[a, b)`: `wait_background`, then `wait_human` (up to `idle`), then `idle`.
    fn gap(&self, a: Ms, b: Ms, closed: Closed<'_>, out: &mut Vec<Piece>) {
        let active = |bg: &Background| bg.start <= a && bg.end > a;
        let waiting = match closed {
            Closed::Notification(reference) => {
                self.background.get(reference).filter(|bg| active(bg))
            }
            _ => self
                .background
                .iter()
                .filter(|(key, bg)| {
                    active(bg)
                        && self
                            .notifications
                            .get(key.as_str())
                            .is_some_and(|at| *at > a && *at <= b)
                })
                .min_by_key(|(key, _)| self.notifications[key.as_str()])
                .map(|(_, bg)| bg),
        };
        let mut rest = a;
        if let Some(bg) = waiting {
            let end = bg.end.min(b);
            if end > a {
                out.push(Piece {
                    kind: "wait_background",
                    start: a,
                    end,
                    attempt_id: Some(bg.attempt_id.clone()),
                    attempt_ids: vec![bg.attempt_id.clone()],
                    context: None,
                    confidence: bg.confidence,
                });
                rest = end;
            }
        }
        if rest >= b {
            return;
        }
        if matches!(closed, Closed::Human) {
            let human_end = b.min(rest.saturating_add(self.idle));
            if human_end > rest {
                out.push(Piece::plain("wait_human", rest, human_end));
            }
            if human_end < b {
                out.push(Piece::plain("idle", human_end, b));
            }
        } else {
            out.push(Piece::plain("idle", rest, b));
        }
    }
}

/// Tool calls running together: from the first `tool_use` to the last `tool_result`.
struct Group {
    start: Ms,
    end: Ms,
    attempts: Vec<usize>,
}

struct Walk<'a> {
    ix: &'a Index,
    attempts: &'a [AttemptRow],
    out: Vec<Piece>,
    cursor: Ms,
    pending: Vec<usize>,
    group: Option<Group>,
    gap: bool,
}

impl Walk<'_> {
    /// Accounts for `[cursor, to)` in the state the machine is in now.
    fn advance(&mut self, to: Ms, closed: Closed<'_>, context: Option<i64>) {
        if to <= self.cursor {
            return;
        }
        if let Some(group) = self.group.as_mut() {
            group.end = to;
        } else if self.gap {
            self.ix.gap(self.cursor, to, closed, &mut self.out);
        } else {
            let mut piece = Piece::plain("model", self.cursor, to);
            piece.context = context;
            self.out.push(piece);
        }
        self.cursor = to;
    }

    fn flush_group(&mut self) {
        let Some(group) = self.group.take() else {
            return;
        };
        if group.end <= group.start {
            return;
        }
        let attempts: Vec<&AttemptRow> =
            group.attempts.iter().map(|i| &self.attempts[*i]).collect();
        let kind = if attempts.iter().any(|a| is_agent(a) && a.background == 0) {
            "subagent"
        } else {
            "tool"
        };
        // The critical path is the longest attempt; the first one wins a tie.
        let mut longest: Option<(Ms, &AttemptRow)> = None;
        for attempt in attempts.iter().copied() {
            let started = parse_ts(&attempt.started_at).unwrap_or(group.start);
            let ended = attempt
                .ended_at
                .as_deref()
                .and_then(parse_ts)
                .unwrap_or(group.end);
            let length = ended - started;
            if longest.is_none_or(|(best, _)| length > best) {
                longest = Some((length, attempt));
            }
        }
        self.out.push(Piece {
            kind,
            start: group.start,
            end: group.end,
            attempt_id: longest.map(|(_, a)| a.attempt_id.clone()),
            attempt_ids: attempts.iter().map(|a| a.attempt_id.clone()).collect(),
            context: None,
            confidence: "exact",
        });
    }

    fn run(mut self, events: &[Event], end: Ms) -> Vec<Piece> {
        for event in events {
            match &event.kind {
                Kind::Compact => continue,
                Kind::ToolEnd(i) if !self.pending.contains(i) => continue,
                Kind::Notification(_) if !self.gap => continue,
                _ => {}
            }
            // A message (or a tool call) that follows a gap with no notification in it means the
            // model never stopped: the gap was model time all along.
            if self.gap && matches!(event.kind, Kind::MsgStart | Kind::ToolStart(_)) {
                self.gap = false;
            }
            let closed = match &event.kind {
                Kind::Prompt => Closed::Human,
                Kind::Notification(reference) => Closed::Notification(reference),
                _ => Closed::Other,
            };
            let context = match event.kind {
                Kind::MsgEnd { context, .. } => Some(context),
                _ => None,
            };
            self.advance(event.ts, closed, context);
            match &event.kind {
                Kind::Prompt => {
                    self.flush_group();
                    self.pending.clear();
                    self.gap = false;
                }
                Kind::MsgEnd { tool_use, .. } => {
                    if !tool_use && self.pending.is_empty() {
                        self.gap = true;
                    }
                }
                Kind::ToolStart(i) => {
                    if self.pending.is_empty() {
                        self.group = Some(Group {
                            start: event.ts,
                            end: event.ts,
                            attempts: Vec::new(),
                        });
                    }
                    self.pending.push(*i);
                    if let Some(group) = self.group.as_mut() {
                        group.attempts.push(*i);
                    }
                }
                Kind::ToolEnd(i) => {
                    self.pending.retain(|p| p != i);
                    if self.pending.is_empty() {
                        self.flush_group();
                    }
                }
                Kind::Interrupt => {
                    self.flush_group();
                    self.pending.clear();
                    self.gap = true;
                }
                Kind::Notification(_) => self.gap = false,
                Kind::MsgStart | Kind::Compact => {}
            }
        }
        self.advance(end, Closed::Other, None);
        self.flush_group();
        self.out
    }
}

/// Adjacent spans of one kind become one, for model, wait and idle. Tool groups never merge: two
/// consecutive groups are two decisions.
fn merge(pieces: Vec<Piece>) -> Vec<Piece> {
    let mut merged: Vec<Piece> = Vec::new();
    for piece in pieces {
        if let Some(last) = merged.last_mut() {
            let mergeable = matches!(
                piece.kind,
                "model" | "wait_human" | "wait_background" | "idle"
            ) && last.kind == piece.kind
                && last.end == piece.start
                && last.confidence == piece.confidence
                && last.attempt_id == piece.attempt_id;
            if mergeable {
                last.end = piece.end;
                last.context = piece.context;
                continue;
            }
        }
        merged.push(piece);
    }
    merged
}

fn to_row(session_id: &str, lane: &str, piece: &Piece) -> SpanRow {
    SpanRow {
        id: 0,
        session_id: session_id.to_string(),
        lane: lane.to_string(),
        kind: piece.kind.to_string(),
        started_at: format_ts(piece.start),
        ended_at: format_ts(piece.end),
        attempt_id: piece.attempt_id.clone(),
        attempt_ids: serde_json::to_string(&piece.attempt_ids).unwrap_or_else(|_| "[]".to_string()),
        context_tokens: piece.context,
        waste: None,
        rule_id: None,
        confidence: piece.confidence.to_string(),
        parser_version: PARSER_VERSION,
    }
}

/// Turns a session's rows into spans, one gap-free, overlap-free partition per lane.
pub fn build_spans(rows: &SessionRows, idle: Duration) -> Vec<SpanRow> {
    let session_id = rows
        .turns
        .iter()
        .map(|r| r.session_id.as_str())
        .chain(rows.messages.iter().map(|r| r.session_id.as_str()))
        .chain(rows.attempts.iter().map(|r| r.session_id.as_str()))
        .chain(rows.markers.iter().map(|r| r.session_id.as_str()))
        .find(|id| !id.is_empty())
        .unwrap_or("");

    let mut lanes: BTreeMap<String, Vec<Event>> = BTreeMap::new();
    for turn in &rows.turns {
        if let Some(ts) = parse_ts(&turn.started_at) {
            lanes.entry(MAIN.to_string()).or_default().push(Event {
                ts,
                kind: Kind::Prompt,
            });
        }
    }
    for message in &rows.messages {
        let (Some(first), Some(last)) = (parse_ts(&message.first_at), parse_ts(&message.last_at))
        else {
            continue;
        };
        let events = lanes.entry(message.lane.clone()).or_default();
        events.push(Event {
            ts: first,
            kind: Kind::MsgStart,
        });
        events.push(Event {
            ts: last.max(first),
            kind: Kind::MsgEnd {
                tool_use: message.has_tool_use != 0,
                context: message.input_tokens
                    + message.cache_read_tokens
                    + message.cache_creation_tokens,
            },
        });
    }
    for (i, attempt) in rows.attempts.iter().enumerate() {
        let Some(start) = parse_ts(&attempt.started_at) else {
            continue;
        };
        let events = lanes.entry(attempt.lane.clone()).or_default();
        events.push(Event {
            ts: start,
            kind: Kind::ToolStart(i),
        });
        // A background launch answers at once; one that never recorded its answer ends where it began.
        let ended = attempt.ended_at.as_deref().and_then(parse_ts);
        match (ended, attempt.background != 0) {
            (Some(end), _) => events.push(Event {
                ts: end.max(start),
                kind: Kind::ToolEnd(i),
            }),
            (None, true) => events.push(Event {
                ts: start,
                kind: Kind::ToolEnd(i),
            }),
            (None, false) => {}
        }
    }
    let mut notifications: HashMap<String, Ms> = HashMap::new();
    let mut references: HashMap<&str, Ms> = HashMap::new();
    for marker in &rows.markers {
        let Some(ts) = parse_ts(&marker.ts) else {
            continue;
        };
        let reference = marker.r#ref.as_deref().unwrap_or("");
        let kind = match marker.kind.as_str() {
            "interrupt" => Kind::Interrupt,
            "bg_notification" => {
                let earliest = notifications.entry(reference.to_string()).or_insert(ts);
                *earliest = (*earliest).min(ts);
                Kind::Notification(reference.to_string())
            }
            "compact_boundary" => Kind::Compact,
            "bg_ref" => {
                let latest = references.entry(reference).or_insert(ts);
                *latest = (*latest).max(ts);
                continue;
            }
            _ => continue,
        };
        lanes
            .entry(marker.lane.clone())
            .or_default()
            .push(Event { ts, kind });
    }
    lanes.retain(|name, _| !name.starts_with("bg:"));
    for events in lanes.values_mut() {
        events.sort_by_key(|event| (event.ts, event.kind.order()));
    }
    let ranges: HashMap<&str, (Ms, Ms)> = lanes
        .iter()
        .filter_map(|(name, events)| Some((name.as_str(), (events.first()?.ts, events.last()?.ts))))
        .collect();

    let mut background: BTreeMap<String, Background> = BTreeMap::new();
    let mut bash_lanes: Vec<(String, Piece)> = Vec::new();
    for attempt in rows.attempts.iter().filter(|a| a.background != 0) {
        let Some(start) = parse_ts(&attempt.started_at) else {
            continue;
        };
        let recorded_end = attempt.bg_ended_at.as_deref().and_then(parse_ts);
        let (end, confidence) = if is_agent(attempt) {
            let lane = attempt.agent_id.as_ref().map(|id| format!("agent:{id}"));
            match lane.as_deref().and_then(|name| ranges.get(name)) {
                Some((_, last)) => (*last, "exact"),
                None => (recorded_end.unwrap_or(start), "inferred"),
            }
        } else if let Some(end) = recorded_end {
            (end, confidence_of(attempt.bg_confidence.as_deref()))
        } else {
            let referenced = attempt
                .bg_task_id
                .as_deref()
                .and_then(|task| references.get(task).copied())
                .filter(|at| *at > start);
            (referenced.unwrap_or(start), "inferred")
        };
        let end = end.max(start);
        if !is_agent(attempt) && end > start {
            bash_lanes.push((
                format!("bg:{}", attempt.tool_use_id),
                Piece {
                    kind: "tool",
                    start,
                    end,
                    attempt_id: Some(attempt.attempt_id.clone()),
                    attempt_ids: vec![attempt.attempt_id.clone()],
                    context: None,
                    confidence,
                },
            ));
        }
        background.insert(
            attempt.tool_use_id.clone(),
            Background {
                attempt_id: attempt.attempt_id.clone(),
                start,
                end,
                confidence,
            },
        );
    }

    let index = Index {
        idle: i64::try_from(idle.as_millis()).unwrap_or(i64::MAX),
        background,
        notifications,
    };
    let mut names: Vec<&String> = lanes.keys().collect();
    names.sort_by(|a, b| (a.as_str() != MAIN, a.as_str()).cmp(&(b.as_str() != MAIN, b.as_str())));

    let mut spans = Vec::new();
    for name in names {
        let events = &lanes[name];
        let (Some(first), Some(last)) = (events.first(), events.last()) else {
            continue;
        };
        let walk = Walk {
            ix: &index,
            attempts: &rows.attempts,
            out: Vec::new(),
            cursor: first.ts,
            pending: Vec::new(),
            group: None,
            gap: false,
        };
        for piece in merge(walk.run(events, last.ts)) {
            spans.push(to_row(session_id, name, &piece));
        }
    }
    for (name, piece) in &bash_lanes {
        spans.push(to_row(session_id, name, piece));
    }
    spans
}

#[cfg(test)]
mod tests {
    use super::build_spans;
    use crate::devtime_store::{AttemptRow, MarkerRow, MessageRow, SessionRows, SpanRow, TurnRow};
    use std::time::Duration;

    const IDLE: Duration = Duration::from_secs(900);

    fn ts(secs: i64) -> String {
        let base = chrono::DateTime::parse_from_rfc3339("2026-10-04T10:00:00.000Z").unwrap();
        (base + chrono::Duration::seconds(secs))
            .format("%Y-%m-%dT%H:%M:%S%.3fZ")
            .to_string()
    }

    fn turn(seq: i64, start: i64) -> TurnRow {
        TurnRow {
            session_id: "s1".into(),
            seq,
            started_at: ts(start),
            ended_at: ts(start),
            parser_version: 1,
            ..Default::default()
        }
    }

    /// Every message carries 100 + 50 + 10 = 160 tokens of context.
    fn msg(lane: &str, id: &str, first: i64, last: i64, tool_use: bool) -> MessageRow {
        MessageRow {
            session_id: "s1".into(),
            lane: lane.into(),
            message_id: id.into(),
            first_at: ts(first),
            last_at: ts(last),
            input_tokens: 100,
            cache_read_tokens: 50,
            cache_creation_tokens: 10,
            output_tokens: 5,
            has_tool_use: tool_use as i64,
            parser_version: 1,
            ..Default::default()
        }
    }

    fn att(
        id: &str,
        lane: &str,
        message: &str,
        kind: &str,
        start: i64,
        end: Option<i64>,
    ) -> AttemptRow {
        AttemptRow {
            attempt_id: id.into(),
            session_id: "s1".into(),
            lane: lane.into(),
            message_id: Some(message.into()),
            tool_use_id: format!("toolu_{id}"),
            kind: kind.into(),
            tool_name: if kind == "agent" { "Agent" } else { "Bash" }.into(),
            started_at: ts(start),
            ended_at: end.map(ts),
            outcome: "ok".into(),
            parser_version: 1,
            ..Default::default()
        }
    }

    fn background(mut attempt: AttemptRow, ended: Option<i64>, task: Option<&str>) -> AttemptRow {
        attempt.background = 1;
        attempt.bg_ended_at = ended.map(ts);
        attempt.bg_confidence = ended.map(|_| "exact".to_string());
        attempt.bg_task_id = task.map(str::to_string);
        attempt
    }

    fn marker(lane: &str, kind: &str, reference: &str, at: i64) -> MarkerRow {
        MarkerRow {
            session_id: "s1".into(),
            lane: lane.into(),
            ts: ts(at),
            kind: kind.into(),
            r#ref: Some(reference.into()),
            parser_version: 1,
            ..Default::default()
        }
    }

    fn lane_of<'a>(spans: &'a [SpanRow], lane: &str) -> Vec<&'a SpanRow> {
        spans.iter().filter(|span| span.lane == lane).collect()
    }

    /// The invariant of the whole module: a lane's spans are sorted, touch end to start, begin and
    /// end where the lane does, and none is empty.
    fn assert_partition(spans: &[SpanRow], lane: &str, start: i64, end: i64) {
        let own = lane_of(spans, lane);
        assert!(!own.is_empty(), "lane {lane} has no spans");
        assert_eq!(own[0].started_at, ts(start), "lane {lane} starts late");
        assert_eq!(
            own[own.len() - 1].ended_at,
            ts(end),
            "lane {lane} ends early"
        );
        for span in &own {
            assert!(
                span.started_at < span.ended_at,
                "lane {lane}: empty span {span:?}"
            );
        }
        for pair in own.windows(2) {
            assert_eq!(
                pair[0].ended_at, pair[1].started_at,
                "lane {lane}: gap or overlap between {:?} and {:?}",
                pair[0], pair[1]
            );
        }
    }

    fn shape(spans: &[SpanRow], lane: &str) -> Vec<(String, String, String)> {
        lane_of(spans, lane)
            .iter()
            .map(|s| (s.kind.clone(), s.started_at.clone(), s.ended_at.clone()))
            .collect()
    }

    fn expect(items: &[(&str, i64, i64)]) -> Vec<(String, String, String)> {
        items
            .iter()
            .map(|(kind, a, b)| (kind.to_string(), ts(*a), ts(*b)))
            .collect()
    }

    #[test]
    fn main_lane_partitions_wall_time_without_gaps_or_overlaps() {
        let rows = SessionRows {
            turns: vec![turn(1, 0), turn(2, 60)],
            messages: vec![
                msg("main", "m1", 2, 5, true),
                msg("main", "m2", 10, 14, false),
                msg("main", "m3", 62, 70, false),
            ],
            attempts: vec![att("a1", "main", "m1", "tool", 4, Some(9))],
            markers: vec![],
        };
        let spans = build_spans(&rows, IDLE);
        assert_partition(&spans, "main", 0, 70);
        assert_eq!(
            shape(&spans, "main"),
            expect(&[
                ("model", 0, 4),
                ("tool", 4, 9),
                ("model", 9, 14),
                ("wait_human", 14, 60),
                ("model", 60, 70),
            ])
        );
        let main = lane_of(&spans, "main");
        assert_eq!(main[2].context_tokens, Some(160));
        assert_eq!(main[4].context_tokens, Some(160));
        assert_eq!(main[0].session_id, "s1");
        assert_eq!(main[0].confidence, "exact");
    }

    #[test]
    fn parallel_tool_uses_in_one_message_give_one_tool_span() {
        let rows = SessionRows {
            turns: vec![],
            messages: vec![msg("main", "m1", 1, 3, true)],
            attempts: vec![
                att("a1", "main", "m1", "tool", 3, Some(10)),
                att("a2", "main", "m1", "tool", 3, Some(6)),
                att("a3", "main", "m1", "tool", 4, Some(12)),
            ],
            markers: vec![],
        };
        let spans = build_spans(&rows, IDLE);
        assert_partition(&spans, "main", 1, 12);
        assert_eq!(
            shape(&spans, "main"),
            expect(&[("model", 1, 3), ("tool", 3, 12)])
        );
        let tool = &lane_of(&spans, "main")[1];
        assert_eq!(tool.attempt_ids, r#"["a1","a2","a3"]"#);
        // The critical path is the longest attempt.
        assert_eq!(tool.attempt_id.as_deref(), Some("a3"));
    }

    #[test]
    fn background_bash_gives_launch_instant_and_wait_background() {
        let mut launch = background(
            att("a1", "main", "m1", "tool", 2, Some(3)),
            Some(50),
            Some("b1"),
        );
        launch.tool_use_id = "toolu_bg".into();
        let rows = SessionRows {
            turns: vec![turn(1, 0)],
            messages: vec![
                msg("main", "m1", 1, 2, true),
                msg("main", "m2", 4, 5, false),
                msg("main", "m3", 51, 55, false),
            ],
            attempts: vec![launch],
            markers: vec![marker("main", "bg_notification", "toolu_bg", 50)],
        };
        let spans = build_spans(&rows, IDLE);
        assert_partition(&spans, "main", 0, 55);
        assert_partition(&spans, "bg:toolu_bg", 2, 50);
        assert_eq!(
            shape(&spans, "main"),
            expect(&[
                ("model", 0, 2),
                ("tool", 2, 3),
                ("model", 3, 5),
                ("wait_background", 5, 50),
                ("model", 50, 55),
            ])
        );
        let main = lane_of(&spans, "main");
        assert_eq!(main[3].attempt_id.as_deref(), Some("a1"));
        let bg = lane_of(&spans, "bg:toolu_bg");
        assert_eq!(bg.len(), 1);
        assert_eq!(bg[0].kind, "tool");
        assert_eq!(bg[0].attempt_id.as_deref(), Some("a1"));
        assert_eq!(bg[0].confidence, "exact");
    }

    #[test]
    fn background_agent_lane_measured_from_its_own_records() {
        let mut launch = background(att("a1", "main", "m1", "agent", 2, Some(3)), None, None);
        launch.tool_use_id = "toolu_ag".into();
        launch.agent_id = Some("ag1".into());
        let rows = SessionRows {
            turns: vec![],
            messages: vec![
                msg("main", "m1", 1, 2, true),
                msg("main", "m2", 4, 5, false),
                msg("main", "m3", 33, 35, false),
                msg("agent:ag1", "ma1", 10, 20, true),
                msg("agent:ag1", "ma2", 25, 30, false),
            ],
            attempts: vec![launch, att("a2", "agent:ag1", "ma1", "tool", 15, Some(18))],
            markers: vec![marker("main", "bg_notification", "toolu_ag", 32)],
        };
        let spans = build_spans(&rows, IDLE);
        assert_partition(&spans, "main", 1, 35);
        assert_partition(&spans, "agent:ag1", 10, 30);
        // The agent lane ends at the agent's own last record, not at the parent's launch result.
        assert_eq!(
            shape(&spans, "agent:ag1"),
            expect(&[("model", 10, 15), ("tool", 15, 18), ("model", 18, 30)])
        );
        assert_eq!(
            shape(&spans, "main"),
            expect(&[
                ("model", 1, 2),
                ("tool", 2, 3),
                ("model", 3, 5),
                ("wait_background", 5, 30),
                ("idle", 30, 32),
                ("model", 32, 35),
            ])
        );
        assert!(lane_of(&spans, "main")[3].attempt_id.as_deref() == Some("a1"));
        assert!(spans.iter().all(|s| !s.lane.starts_with("bg:")));
    }

    #[test]
    fn long_human_gap_splits_into_wait_human_and_idle() {
        let rows = SessionRows {
            turns: vec![turn(1, 0), turn(2, 1205)],
            messages: vec![
                msg("main", "m1", 1, 5, false),
                msg("main", "m2", 1206, 1210, false),
            ],
            attempts: vec![],
            markers: vec![],
        };
        let spans = build_spans(&rows, IDLE);
        assert_partition(&spans, "main", 0, 1210);
        assert_eq!(
            shape(&spans, "main"),
            expect(&[
                ("model", 0, 5),
                ("wait_human", 5, 905),
                ("idle", 905, 1205),
                ("model", 1205, 1210),
            ])
        );
    }

    #[test]
    fn bg_running_but_human_resumes_is_wait_human() {
        let mut launch = background(
            att("a1", "main", "m1", "tool", 2, Some(3)),
            Some(600),
            Some("b1"),
        );
        launch.tool_use_id = "toolu_bg".into();
        let rows = SessionRows {
            turns: vec![turn(1, 0), turn(2, 65)],
            messages: vec![
                msg("main", "m1", 1, 2, true),
                msg("main", "m2", 4, 5, false),
                msg("main", "m3", 66, 70, false),
            ],
            attempts: vec![launch],
            markers: vec![marker("main", "bg_notification", "toolu_bg", 600)],
        };
        let spans = build_spans(&rows, IDLE);
        assert_partition(&spans, "main", 0, 600);
        assert_partition(&spans, "bg:toolu_bg", 2, 600);
        // The human typed at 65, before the notification: that gap is wait_human, and only the
        // later gap that the notification itself closes is wait_background.
        assert_eq!(
            shape(&spans, "main"),
            expect(&[
                ("model", 0, 2),
                ("tool", 2, 3),
                ("model", 3, 5),
                ("wait_human", 5, 65),
                ("model", 65, 70),
                ("wait_background", 70, 600),
            ])
        );
    }

    #[test]
    fn subagent_lane_partitions_recursively() {
        let mut outer = att("a1", "main", "m0", "agent", 2, Some(40));
        outer.agent_id = Some("ag1".into());
        let mut inner = att("a2", "agent:ag1", "ma1", "agent", 12, Some(30));
        inner.agent_id = Some("ag2".into());
        let rows = SessionRows {
            turns: vec![],
            messages: vec![
                msg("main", "m0", 1, 2, true),
                msg("agent:ag1", "ma1", 5, 12, true),
                msg("agent:ag1", "ma2", 31, 38, false),
                msg("agent:ag2", "mb1", 14, 28, false),
            ],
            attempts: vec![outer, inner],
            markers: vec![],
        };
        let spans = build_spans(&rows, IDLE);
        assert_partition(&spans, "main", 1, 40);
        assert_partition(&spans, "agent:ag1", 5, 38);
        assert_partition(&spans, "agent:ag2", 14, 28);
        assert_eq!(
            shape(&spans, "agent:ag1"),
            expect(&[("model", 5, 12), ("subagent", 12, 30), ("model", 30, 38)])
        );
        assert_eq!(shape(&spans, "agent:ag2"), expect(&[("model", 14, 28)]));
    }

    #[test]
    fn foreground_agent_is_a_subagent_span() {
        let mut agent = att("a1", "main", "m1", "agent", 2, Some(20));
        agent.agent_id = Some("ag1".into());
        let rows = SessionRows {
            turns: vec![],
            messages: vec![
                msg("main", "m1", 1, 2, true),
                msg("main", "m2", 21, 22, true),
            ],
            attempts: vec![
                agent,
                att("a2", "main", "m1", "tool", 2, Some(5)),
                att("a3", "main", "m2", "tool", 22, Some(25)),
            ],
            markers: vec![],
        };
        let spans = build_spans(&rows, IDLE);
        assert_partition(&spans, "main", 1, 25);
        assert_eq!(
            shape(&spans, "main"),
            expect(&[
                ("model", 1, 2),
                ("subagent", 2, 20),
                ("model", 20, 22),
                ("tool", 22, 25),
            ])
        );
        let main = lane_of(&spans, "main");
        assert_eq!(main[1].attempt_ids, r#"["a1","a2"]"#);
        assert_eq!(main[1].attempt_id.as_deref(), Some("a1"));
    }

    #[test]
    fn bash_without_notification_ends_inferred() {
        let mut referenced = background(
            att("a1", "main", "m1", "tool", 2, Some(3)),
            None,
            Some("b9"),
        );
        referenced.tool_use_id = "toolu_x".into();
        let mut lonely = background(
            att("a2", "main", "m1", "tool", 2, Some(3)),
            None,
            Some("b8"),
        );
        lonely.tool_use_id = "toolu_y".into();
        let rows = SessionRows {
            turns: vec![],
            messages: vec![
                msg("main", "m1", 1, 2, true),
                msg("main", "m2", 4, 6, false),
            ],
            attempts: vec![referenced, lonely],
            markers: vec![
                marker("main", "bg_ref", "b9", 30),
                marker("main", "bg_ref", "b9", 40),
            ],
        };
        let spans = build_spans(&rows, IDLE);
        assert_partition(&spans, "main", 1, 6);
        assert_partition(&spans, "bg:toolu_x", 2, 40);
        let bg = lane_of(&spans, "bg:toolu_x");
        assert_eq!(bg.len(), 1);
        assert_eq!(bg[0].kind, "tool");
        assert_eq!(bg[0].confidence, "inferred");
        // Ended at the launch instant, so the lane would be empty and is left out.
        assert!(lane_of(&spans, "bg:toolu_y").is_empty());
        assert!(
            lane_of(&spans, "main")
                .iter()
                .all(|s| s.confidence == "exact")
        );
    }
}
