//! Pure dossier, extraction, parsing, layer mapping, and backoff helpers.
//! The worker stays in the parent while these deterministic pieces live here.

use super::*;

// ---- P3: the pure parts (dossier, extraction prompt, strict parsing, layer mapping, backoff) ----

pub(super) use crate::knowledge::{Kind, Layer};

/// The most characters a dossier may hold before whole blocks start to go (spec §4.3).
pub const DOSSIER_CEILING: usize = 24_000;
/// The most characters kept of one stdout or gate entry, tail first; applied when gathering.
pub const ENTRY_CHARS: usize = 4_000;
pub const MAX_ITEMS: usize = 5;
pub const MAX_ATTEMPTS: i64 = 3;
pub const BACKOFF_BASE: chrono::Duration = chrono::Duration::minutes(5);

/// What failed and what finally passed, for a `gate_recovered` cause.
pub struct Recovery {
    pub failed_headline: String,
    pub passing_run: (i64, String),
}

/// Everything the dossier may say, already gathered and already clipped per entry.
pub struct DossierInputs {
    pub cause: Cause,
    pub owner_context: Vec<String>,
    pub reviews: Vec<(i64, String)>,
    pub job_prompt: Option<String>,
    pub items: Vec<String>,
    pub outcome: Option<String>,
    pub recovery: Option<Recovery>,
    pub known_titles: Vec<String>,
    pub gate_outputs: Vec<(i64, String)>,
    /// Chat exchanges: run id, what the owner wrote, the answer.
    pub conversation: Vec<(i64, String, String)>,
}

/// The text handed to the model and the runs whose text survived the cut.
pub struct Dossier {
    pub text: String,
    pub runs: Vec<i64>,
}

/// Keep the first `width` characters (char-safe).
pub(crate) fn clip_head(s: &str, width: usize) -> String {
    s.chars().take(width).collect()
}

fn present(s: &str) -> bool {
    !s.trim().is_empty()
}

/// Break the dossier's own markers inside a text, so a block cannot close the dossier or pass for
/// another block (spec `2026-10-07-fronteira-prompts-design.md` §5.1; the technique of
/// `judge::break_fence_markers`, with this prompt's markers). The heading `blocks` writes is added
/// after, so it stays whole.
fn break_dossier_markers(text: &str) -> String {
    // One pass is not enough: `replace` skips overlapping matches, so `--- DOSSIER ---- DOSSIER ---`
    // would keep a whole marker. A zero-width space inside the leading dashes only ever removes a
    // `---`, never makes one, so repeating until none is left ends.
    let mut text = text.to_string();
    for marker in ["--- END OF DOSSIER ---", "--- DOSSIER ---"] {
        let broken = marker.replacen("---", "-\u{200B}--", 1);
        while text.contains(marker) {
            text = text.replace(marker, &broken);
        }
    }
    text.split('\n')
        .map(|line| {
            let indent = line.len() - line.trim_start().len();
            match line[indent..].strip_prefix("##") {
                Some(rest) => format!("{}#\u{200B}#{rest}", &line[..indent]),
                None => line.to_string(),
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Every raw text leaves redacted (`judge::redact_for_judge`: it goes to a cloud model by default)
/// and with the dossier's markers broken.
pub(crate) fn scrub(text: &str) -> String {
    break_dossier_markers(&crate::judge::redact_for_judge(text))
}

/// `inputs` with every text field scrubbed, so `blocks` formats only treated text.
fn scrubbed(inputs: &DossierInputs) -> DossierInputs {
    DossierInputs {
        cause: inputs.cause,
        owner_context: inputs.owner_context.iter().map(|t| scrub(t)).collect(),
        reviews: inputs
            .reviews
            .iter()
            .map(|(id, t)| (*id, scrub(t)))
            .collect(),
        job_prompt: inputs.job_prompt.as_deref().map(scrub),
        items: inputs.items.iter().map(|t| scrub(t)).collect(),
        outcome: inputs.outcome.as_deref().map(scrub),
        recovery: inputs.recovery.as_ref().map(|r| Recovery {
            failed_headline: scrub(&r.failed_headline),
            passing_run: (r.passing_run.0, scrub(&r.passing_run.1)),
        }),
        known_titles: inputs.known_titles.iter().map(|t| scrub(t)).collect(),
        gate_outputs: inputs
            .gate_outputs
            .iter()
            .map(|(id, t)| (*id, scrub(t)))
            .collect(),
        conversation: inputs
            .conversation
            .iter()
            .map(|(id, asked, answered)| (*id, scrub(asked), scrub(answered)))
            .collect(),
    }
}

/// Render the six blocks of spec §4.3 in order, each with the run ids it names. A block with
/// nothing in it is omitted.
fn blocks(inputs: &DossierInputs) -> Vec<(String, Vec<i64>)> {
    let scrubbed = scrubbed(inputs);
    let inputs = &scrubbed;
    let mut out: Vec<(String, Vec<i64>)> = Vec::new();

    // 1. Owner context: understood, never quoted.
    let notes: Vec<&str> = inputs
        .owner_context
        .iter()
        .map(String::as_str)
        .filter(|n| present(n))
        .collect();
    if !notes.is_empty() {
        out.push((
            format!(
                "## Owner context: for your understanding only. Never quote it.\n{}",
                notes.join("\n---\n")
            ),
            Vec::new(),
        ));
    }

    // 2. Review output.
    let reviews: Vec<&(i64, String)> = inputs.reviews.iter().filter(|(_, t)| present(t)).collect();
    if !reviews.is_empty() {
        let body: Vec<String> = reviews
            .iter()
            .map(|(id, text)| format!("Review run #{id}:\n{text}"))
            .collect();
        out.push((
            format!("## Review output\n{}", body.join("\n\n")),
            reviews.iter().map(|(id, _)| *id).collect(),
        ));
    }

    // 2b. A conversation: the exchanges of one chat, each named by its run.
    let exchanges: Vec<&(i64, String, String)> = inputs
        .conversation
        .iter()
        .filter(|(_, asked, answered)| present(asked) || present(answered))
        .collect();
    if !exchanges.is_empty() {
        let body: Vec<String> = exchanges
            .iter()
            .map(|(id, asked, answered)| {
                format!("Run #{id}:\nOwner: {asked}\nAssistant: {answered}")
            })
            .collect();
        out.push((
            format!("## Conversation\n{}", body.join("\n\n")),
            exchanges.iter().map(|(id, _, _)| *id).collect(),
        ));
    }

    // 3. The job: prompt, items, outcome.
    let mut job: Vec<String> = Vec::new();
    if let Some(prompt) = inputs.job_prompt.as_deref().filter(|p| present(p)) {
        job.push(format!("Job prompt:\n{prompt}"));
    }
    let items: Vec<&str> = inputs
        .items
        .iter()
        .map(String::as_str)
        .filter(|i| present(i))
        .collect();
    if !items.is_empty() {
        job.push(format!("Items:\n- {}", items.join("\n- ")));
    }
    if let Some(outcome) = inputs.outcome.as_deref().filter(|o| present(o)) {
        job.push(format!("Outcome:\n{outcome}"));
    }
    if !job.is_empty() {
        out.push((format!("## The job\n{}", job.join("\n\n")), Vec::new()));
    }

    // 4. A recovery: what failed, what passed.
    if let Some(rec) = &inputs.recovery {
        let (id, stdout) = &rec.passing_run;
        let mut parts: Vec<String> = Vec::new();
        if present(&rec.failed_headline) {
            parts.push(format!("What failed:\n{}", rec.failed_headline));
        }
        if present(stdout) {
            parts.push(format!("The run that passed (run #{id}):\n{stdout}"));
        }
        if !parts.is_empty() {
            out.push((format!("## Recovery\n{}", parts.join("\n\n")), vec![*id]));
        }
    }

    // 5. What is already known, so it is not repeated.
    let known: Vec<&str> = inputs
        .known_titles
        .iter()
        .map(String::as_str)
        .filter(|t| present(t))
        .collect();
    if !known.is_empty() {
        out.push((
            format!("## Already known (do not repeat)\n- {}", known.join("\n- ")),
            Vec::new(),
        ));
    }

    // 6. Gate output.
    let gates: Vec<&(i64, String)> = inputs
        .gate_outputs
        .iter()
        .filter(|(_, t)| present(t))
        .collect();
    if !gates.is_empty() {
        let body: Vec<String> = gates
            .iter()
            .map(|(id, text)| format!("Gate output of run #{id}:\n{text}"))
            .collect();
        out.push((
            format!("## Gate output\n{}", body.join("\n\n")),
            gates.iter().map(|(id, _)| *id).collect(),
        ));
    }

    out
}

/// Build the dossier. While the text is over `ceiling` characters the LAST remaining block goes,
/// whole, until only the first is left; if that alone is still over, it is clipped. A dropped
/// block is announced by one closing line, which may itself pass the ceiling.
pub fn dossier(inputs: &DossierInputs, ceiling: usize) -> Dossier {
    let all = blocks(inputs);
    let total = all.len();
    let join = |n: usize| -> String {
        all[..n]
            .iter()
            .map(|(t, _)| t.as_str())
            .collect::<Vec<_>>()
            .join("\n\n")
    };

    let mut kept = total;
    while kept > 1 && join(kept).chars().count() > ceiling {
        kept -= 1;
    }
    let mut text = join(kept);
    if kept == 1 && text.chars().count() > ceiling {
        text = clip_head(&text, ceiling);
    }
    let dropped = total - kept;
    if dropped > 0 {
        let noun = if dropped == 1 { "block" } else { "blocks" };
        text.push_str(&format!(
            "\n\n[{dropped} {noun} left out to keep this within its size limit]"
        ));
    }

    let mut runs: Vec<i64> = all[..kept]
        .iter()
        .flat_map(|(_, ids)| ids.iter().copied())
        .collect();
    runs.sort_unstable();
    runs.dedup();
    Dossier { text, runs }
}

const STANDING_HEAD: &str = "You read the dossier below, about one finished piece of work, and write \
down what is worth remembering the next time. Answer with a JSON array and nothing else: 0 to 5 items, \
each an object with `layer` (episodic, semantic or procedural), `title` (one line), `body` (short) and \
`files` (repository paths it concerns). Prefer lessons that would still hold in another project, and say \
in the body when one is specific to this repository. Never quote the owner context. Do not repeat the \
known titles. `[]` is a good answer when nothing is worth keeping.";

/// The standing instruction of an extraction call, by the cause that queued the work (spec B10).
pub fn standing_for(cause: Cause) -> String {
    let tail = match cause {
        Cause::JobLanded => " The work landed: ask what made it land, not only what went wrong.",
        Cause::CalendarWeek => {
            " The dossier is one week of the owner's calendar, read from another project's point of view: ask for rhythms and habits of the owner (days, hours, durations), as `episodic` items, never the names of third parties."
        }
        Cause::ChatExchanges => {
            " This dossier is a conversation between the owner and an assistant. Write down the preferences, decisions and corrections the owner stated that would still matter in a later session, never a summary of the conversation."
        }
        Cause::JobFailed | Cause::GateRecovered | Cause::ReviewBlocking | Cause::RunExhausted => "",
    };
    format!("{STANDING_HEAD}{tail}")
}

/// The whole extraction prompt: the standing instruction, one line naming why the work was queued
/// (the closed [`Cause`] vocabulary, never free text), then the dossier.
pub fn extraction_prompt(cause: Cause, dossier: &str) -> String {
    let why = cause.as_str();
    let standing = standing_for(cause);
    format!(
        "{standing}\n\nWhy this was queued: {why}\n\n--- DOSSIER ---\n{dossier}\n--- END OF DOSSIER ---"
    )
}

/// The JSON schema of the answer: an array of at most [`MAX_ITEMS`] items.
pub fn items_format() -> serde_json::Value {
    serde_json::json!({
        "type": "array",
        "maxItems": MAX_ITEMS,
        "items": {
            "type": "object",
            "properties": {
                "layer": { "type": "string", "enum": ["episodic", "semantic", "procedural"] },
                "title": { "type": "string" },
                "body": { "type": "string" },
                "files": { "type": "array", "items": { "type": "string" } }
            },
            "required": ["layer", "title", "body"]
        }
    })
}

/// One learning the model proposed, already checked.
#[derive(Debug)]
pub struct Item {
    pub layer: Layer,
    pub title: String,
    pub body: String,
    pub files: Vec<String>,
}

/// Why an answer was refused whole. The text of the answer is never part of it.
#[derive(Debug)]
pub enum ParseError {
    NotAnArray,
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ParseError::NotAnArray => f.write_str("the answer is not a JSON array"),
        }
    }
}

impl std::error::Error for ParseError {}

pub(super) fn strip_fence(answer: &str) -> &str {
    let s = answer.trim();
    let Some(rest) = s.strip_prefix("```") else {
        return s;
    };
    let rest = rest.strip_prefix("json").unwrap_or(rest);
    match rest.trim_end().strip_suffix("```") {
        Some(inner) => inner.trim(),
        None => s,
    }
}

fn parse_item(value: &serde_json::Value) -> Option<Item> {
    let obj = value.as_object()?;
    let layer = Layer::parse(obj.get("layer")?.as_str()?)?;
    if layer == Layer::Working {
        return None;
    }
    let title = obj.get("title")?.as_str()?.trim();
    let body = obj.get("body")?.as_str()?.trim();
    if title.is_empty() || body.is_empty() {
        return None;
    }
    let files = obj
        .get("files")
        .and_then(|f| f.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    Some(Item {
        layer,
        title: title.to_string(),
        body: body.to_string(),
        files,
    })
}

/// Strict parse: anything that is not a JSON array fails whole; inside an array a bad element is
/// dropped alone, and only the first [`MAX_ITEMS`] good ones are kept.
pub fn parse_items(answer: &str) -> Result<Vec<Item>, ParseError> {
    let values = serde_json::from_str::<Vec<serde_json::Value>>(strip_fence(answer))
        .map_err(|_| ParseError::NotAnArray)?;
    Ok(values
        .iter()
        .filter_map(parse_item)
        .take(MAX_ITEMS)
        .collect())
}

/// How a learning enters the store: recorded as it is, or proposed for a person to approve.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Door {
    Record,
    Propose,
}

/// The model picks only the layer; kind and door follow from it.
pub fn door(layer: Layer) -> (Kind, Door) {
    match layer {
        Layer::Episodic => (Kind::Memory, Door::Record),
        Layer::Semantic => (Kind::Memory, Door::Propose),
        Layer::Procedural => (Kind::Prompt, Door::Propose),
        // The parser never lets `working` through; if one arrives it is proposed like a rule.
        Layer::Working => (Kind::Memory, Door::Propose),
    }
}

/// When to try again after the `attempts_after_failure`-th failure; `None` once it is final.
pub fn retry_at(
    attempts_after_failure: i64,
    now: chrono::DateTime<chrono::Utc>,
) -> Option<chrono::DateTime<chrono::Utc>> {
    if attempts_after_failure >= MAX_ATTEMPTS {
        return None;
    }
    let exp = attempts_after_failure.clamp(0, 16) as u32;
    Some(now + BACKOFF_BASE * (1i32 << exp))
}
