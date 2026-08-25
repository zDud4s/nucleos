//! The intention layer of a project's map: what a spec decided, one line at a time.
//!
//! **Pure, and that is what makes the hard part testable.** The prompt and the parse are where the
//! design of this slice actually lives, and a function that needs a model running to be exercised
//! is a function nobody exercises. Nothing here knows what SQL is, what HTTP is, or which model
//! answered — it takes text and returns data. `map_store.rs` holds the rows, `http.rs` holds the
//! routes, and the runner that answers is chosen by whoever calls.
//!
//! **The map does not read a decision table; it provokes one.** Two of this repository's 38 specs
//! carry a numbered decision table, so reading one is not a strategy. A model reads the document
//! and proposes; the owner approves line by line; nothing reaches the map unapproved — §4.
//!
//! **The model compresses, and never certifies.** This is a model reading a document a model
//! helped write, and that objection is fair. What answers it is the shape of the product: twelve
//! numbered lines are read in two minutes, and the step that does not happen today — the owner
//! looking — starts happening. Nothing here decides anything; it makes a thousand lines small
//! enough that human attention fits again.

use serde::{Deserialize, Serialize};

/// What kind of decision this is, and therefore what it will one day ask of the owner (§4.1).
///
/// **Two variants, and the missing third is the point.** §4.1 names three kinds. Type A — about
/// scope, process, or the document itself — can be implemented by no code and asks nothing of
/// anybody, so it is dropped when the extraction is approved and never stored. Giving it a variant
/// here would make it a row this table can hold, and the whole argument for dropping it is that it
/// cannot be one.
///
/// **Two wire forms on purpose, and they are not interchangeable.** `as_str`/`from_wire` are the
/// STORAGE form — `b` and `c` — which is what the `map_decisions.kind` column holds and what its
/// `CHECK` constrains it to. The derived `Serialize`/`Deserialize` are the JSON form —
/// `countable` and `character` — which is what the window reads, because a list meant to be
/// approved at a glance cannot label its two kinds `b` and `c`. Both are pinned by tests below,
/// so neither can be renamed into agreement with the other by accident: a `Kind` serialised into
/// the column would fail its `CHECK`, and a column value handed to serde would not parse.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    /// Names a number, a set, or a coverage claim — something that can later be counted and agreed
    /// or disagreed with. Asks the owner nothing while it holds, and reaches them when it breaks.
    Countable,
    /// About character: what a thing is, or is not. No count decides it, so total coverage and the
    /// wrong thing are compatible. This is the kind that only a stamp can answer.
    Character,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Countable => "b",
            Self::Character => "c",
        }
    }

    /// `b` or `c`, and nothing else.
    ///
    /// Returns `None` rather than falling back the way [`crate::chats::Brain::from_wire`] does, and
    /// the difference is deliberate: a brain nobody chose has a safe default, and a decision whose
    /// kind nobody could read has none. Guessing `c` would put a stamp request in front of the
    /// owner for something no stamp was owed on; guessing `b` would silence something that needed
    /// looking at. Dropping the line is the only answer that claims nothing.
    pub fn from_wire(value: &str) -> Option<Self> {
        match value {
            "b" => Some(Self::Countable),
            "c" => Some(Self::Character),
            _ => None,
        }
    }
}

/// How much of a document is sent. Beyond this it is cut — at a line boundary where there is one —
/// and the cut is stated.
///
/// Not a token count, because this module does not know which model will answer and the two runners
/// it feeds count differently. Bytes are the honest unit for a limit whose only job is to keep one
/// document from being larger than any of them.
pub const MAX_SPEC_BYTES: usize = 60_000;

/// The document, cut at a line if it must be, and whether it was.
fn bounded(source: &str) -> (&str, bool) {
    if source.len() <= MAX_SPEC_BYTES {
        return (source, false);
    }
    // Backed off to a character boundary before slicing at all. These specs are Portuguese, so
    // byte 60_000 lands inside a `ç`, an `ã` or a `§` often enough, and `&source[..60_000]`
    // panics there rather than truncating. A daemon that died because a document happened to be
    // the wrong length would be the worst shape this failure could take — invisible until the one
    // spec that triggers it, and then fatal.
    let mut ceiling = MAX_SPEC_BYTES;
    while ceiling > 0 && !source.is_char_boundary(ceiling) {
        ceiling -= 1;
    }
    let cut = source[..ceiling].rfind('\n').unwrap_or(ceiling);
    (&source[..cut], true)
}

/// What to ask a model about one spec.
///
/// **The whole design of this slice is in this string**, so it is worth saying what each rule is
/// buying. A model that invents produces an approved line nothing decided, and the owner then owes
/// a stamp on it forever — that is the failure that makes the map worse than no map. A line with no
/// section can never be anchored to code, so it sits in the list being neither true nor false. And
/// the three kinds are what decide whether this costs an afternoon a week or thirty seconds a day,
/// which is why they are spelled out rather than named.
///
/// Deliberately says nothing about the document's language. These specs are Portuguese and the
/// decisions must come back in the document's own words: a translated decision is a paraphrase, and
/// a paraphrase is exactly the thing the owner cannot check at a glance.
pub fn extraction_prompt(spec_slug: &str, source: &str) -> String {
    let (body, was_cut) = bounded(source);
    let cut_note = if was_cut {
        "\n\n[The document was truncated at a line boundary to fit. Decisions after the cut are \
         not yours to guess at.]"
    } else {
        ""
    };

    format!(
        "You are reading one design document and listing the decisions it fixes.\n\
         \n\
         A decision is something the document SETTLES about the product — a rule that code either \
         follows or does not. Three kinds exist, and only two of them belong in your list.\n\
         \n\
         1. About scope, process, or the document itself. No code can implement it. Example: \"Two \
         specs, and the page comes first.\" LEAVE THESE OUT ENTIRELY.\n\
         2. Names something countable — a number, a set, a coverage claim — so that something could \
         later count the code and agree or disagree. Example: \"agent, command, decision, fan\", \
         which is four kinds and an enum that has four. kind = \"b\".\n\
         3. About character: what a thing IS or IS NOT. No count decides it. Example: \"Code mode is \
         a review surface, not an IDE.\" kind = \"c\".\n\
         \n\
         Rules:\n\
         - One entry per decision. Never merge two into one line.\n\
         - `section` is the heading the decision came from, copied verbatim from the document, \
         including its number. If you cannot point at one heading, leave it out.\n\
         - `text` is ONE sentence saying what was decided, in the document's own language. Not a \
         summary of the section — the decision.\n\
         - Invent nothing. If the document settles four things, return four. An empty list is a \
         valid answer.\n\
         - At most 20 entries, and prefer fewer.\n\
         \n\
         Answer with JSON only, shaped exactly like this and nothing else:\n\
         {{\"decisions\":[{{\"section\":\"...\",\"text\":\"...\",\"kind\":\"b\"}}]}}\n\
         \n\
         The document is `{spec_slug}`:\n\
         \n\
         ----- BEGIN DOCUMENT -----\n\
         {body}\n\
         ----- END DOCUMENT -----{cut_note}"
    )
}

/// One decision, as proposed and not yet approved.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Extracted {
    /// The heading it came from, verbatim. Without this it can never be joined to code.
    pub section: String,
    /// Its place in the list the owner reads, 1-based. Counts what survived the parse, not what
    /// the model proposed — a list numbered with gaps invites the question "where is 3?", and the
    /// answer would be "it was malformed", which is not the owner's business.
    pub ordinal: i64,
    pub text: String,
    pub kind: Kind,
}

#[derive(Deserialize)]
struct RawAnswer {
    /// Untyped elements on purpose. A `Vec<RawDecision>` fails the WHOLE array when one element is
    /// not an object — `{"decisions":[{...good...}, 42]}` returned nothing at all — and an empty
    /// list is this module's word for "this document decided nothing". A malformed line must cost
    /// its own row and no others.
    decisions: Vec<serde_json::Value>,
}

#[derive(Deserialize)]
struct RawDecision {
    /// `Option<String>` and not `String`, because `#[serde(default)]` fills in for an ABSENT key
    /// and does nothing at all for a key present as `null`. The prompt tells the model to "leave it
    /// out" when it cannot name a heading, and a model constrained to emit JSON answers that with
    /// `"section": null` at least as readily as by omitting the key. Typed as `String`, that single
    /// null failed the whole batch: nine good decisions thrown away because a tenth had one odd
    /// field, and the owner told the document decided nothing.
    #[serde(default)]
    section: Option<String>,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    kind: Option<String>,
}

/// The decisions in a model's answer, or none.
///
/// **Never fails, and that is deliberate.** Every failure here — unparseable JSON, prose around it,
/// a kind nobody can read, a line with no section — means the same thing to the person who pressed
/// the button: this spec produced nothing, press again or pick another brain. A `Result` would ask
/// the route to turn four different noises into one sentence anyway, and an error that always
/// becomes the same sentence is a longer way of writing an empty list.
///
/// A line missing its section, missing its text, or carrying a kind that is not `b` or `c` is
/// dropped rather than repaired. §4.1's type A arrives here as `"a"` and is dropped by exactly the
/// same rule, which is what the missing `Kind` variant buys.
pub fn parse_extraction(answer: &str) -> Vec<Extracted> {
    let Some(raw) = json_object(answer).and_then(|slice| {
        serde_json::from_str::<RawAnswer>(slice).ok()
    }) else {
        return Vec::new();
    };

    let mut out = Vec::new();
    for value in raw.decisions {
        // Converted one element at a time, so a line that is not an object costs its own row and
        // not the batch.
        let Ok(row) = serde_json::from_value::<RawDecision>(value) else {
            continue;
        };
        let section = row.section.unwrap_or_default();
        let text = row.text.unwrap_or_default();
        let kind_wire = row.kind.unwrap_or_default();
        let section = section.trim();
        let text = text.trim();
        if section.is_empty() || text.is_empty() {
            continue;
        }
        let Some(kind) = Kind::from_wire(kind_wire.trim()) else {
            continue;
        };
        out.push(Extracted {
            section: section.to_owned(),
            ordinal: out.len() as i64 + 1,
            text: text.to_owned(),
            kind,
        });
    }
    out
}

/// The outermost `{...}` in a string, or nothing.
///
/// Models wrap JSON in prose and in fences, most of them at least sometimes. Refusing those answers
/// would send the owner back to press the button again for a reason that was never theirs.
///
/// Deliberately not a JSON scanner: it takes the first `{` and the last `}`, which is wrong for an
/// answer containing two separate objects and right for every answer this has actually been handed.
/// The cost of being wrong is an empty list and a second press.
fn json_object(answer: &str) -> Option<&str> {
    let start = answer.find('{')?;
    let end = answer.rfind('}')?;
    (end > start).then(|| &answer[start..=end])
}

/// Ask one runner about one spec.
///
/// **The runner is the parameter, and that is the whole of the owner's choice.** `OllamaRunner` and
/// `ClaudeCliRunner` both implement [`crate::runner::CommandRunner`], so a local brain and a cloud
/// one arrive here as the same type and leave as the same answer. There is no second code path to
/// keep in step, and no heuristic deciding which one was good enough — the owner decided.
///
/// **An empty list and a failure are different answers and never collapse.** Empty means a model
/// read the document and found nothing to fix; an error means nobody read anything. Telling the
/// owner the first when the second happened is precisely the false confidence this feature exists
/// to cure, in the one place it would be easiest to introduce.
pub async fn extract(
    runner: &dyn crate::runner::CommandRunner,
    spec_slug: &str,
    source: &str,
) -> std::io::Result<Vec<Extracted>> {
    // Every field is spelled out because `RunRequest` deliberately has no `Default` — its own doc
    // comment says why: a flag added later must not silently inherit a value nobody chose. Copied
    // from `council.rs`'s cloud seat, which is the closest neighbour (one question, no tools, no
    // resume), and changed only where this call differs.
    let request = crate::runner::RunRequest {
        prompt: extraction_prompt(spec_slug, source),
        // Nothing to reach, so nothing to carry. A run with no tools cannot spend a token, and one
        // handed a key it has no door for is a key that leaked for no reason.
        env: Vec::new(),
        cwd: None,
        plan_only: false,
        resume_session_id: None,
        mcp_config: None,
        // The argument is `local_agent::verdict`'s: a reader that could edit the repository is not
        // reading it. It is also the only policy `OllamaRunner` accepts at all — it refuses
        // anything else outright — so the local half of the owner's choice depends on this value.
        tool_policy: crate::runner::ToolPolicy::None,
        progress_timeout: None,
        // Asks once and reads the answer. There is no tool call for a second turn to follow up on.
        max_turns: Some(1),
        session_id: None,
        fork_session: false,
        // Nobody is watching this stream; the answer is read once, whole, at the end.
        include_partial_messages: false,
        images: Vec::new(),
        // NOT because anything ever steers this — `messages` is `None` and no second turn is sent —
        // but because it is the only way to keep the prompt OFF the command line. `cli_args` pushes
        // the prompt as a positional argument otherwise, and Windows caps a command line at 32 767
        // characters while [`MAX_SPEC_BYTES`] lets a document reach 60 000. Every spec worth reading
        // is over that ceiling, so the argv path would fail with `ERROR_FILENAME_EXCED_RANGE` — os
        // error 206, whose name says filename and whose meaning is argv. `council.rs` records
        // losing a whole run to exactly this.
        steerable: true,
        // Nothing to govern: the policy above leaves no tool for a classifier to judge a call to.
        classifier_governs_tools: false,
        messages: None,
        // The strict default. A spec is a document, and a document being read is not a reason to
        // open a connector to it.
        ambient_mcp: false,
        // `None` keeps whatever the runner was built with, and that is the point of this signature:
        // the owner chose a brain by choosing which runner arrives here, not by naming a model
        // string that only one of the two would honour.
        model: None,
        effort: None,
        fallback_model: Vec::new(),
        add_dirs: Vec::new(),
        max_budget_usd: None,
        agents: Vec::new(),
        append_system_prompt: None,
        // Empty, and not a belt for the policy's braces: `ToolPolicy::None` already denies every
        // built-in, and naming some of them here would read as though the rest were allowed.
        denied_tools: Vec::new(),
        session_name: None,
        context_window: None,
        // Only ever read beside an `mcp_config`, and there is none.
        allowed_mcp_tools: None,
    };

    // Throwaways: this reads neither. The receiver is bound rather than dropped on the spot, and
    // that is deliberate belt-and-braces — both runners send the session id best-effort (`let _ =
    // session_tx.send(..)`), so a closed channel is already harmless, but a function that relies on
    // that is relying on a detail of somebody else's error handling.
    let (session_tx, _session_rx) = tokio::sync::mpsc::unbounded_channel();
    let transcript = std::sync::Arc::new(std::sync::Mutex::new(String::new()));

    let outcome = runner.run_prompt(request, session_tx, transcript).await?;

    // The two runners do not answer in the same shape, and this is the seam where that shows.
    // `OllamaRunner` puts the model's words straight into `stdout`; `ClaudeCliRunner` puts the whole
    // `--output-format stream-json` transcript there, one event per line, with the answer inside the
    // final `result`. Handed that stream, `parse_extraction` takes the first `{` and the last `}` of
    // the WHOLE thing and deserialises nothing — so every cloud extraction would come back "this
    // spec decided nothing" while the model had in fact answered. `extract_reply` is what the rest
    // of this daemon uses for exactly that, and it returns `None` for an answer that is not a
    // stream, which is why the local runner's plain text passes through it untouched.
    let answer = crate::runner::extract_reply(&outcome.stdout).unwrap_or(outcome.stdout);
    Ok(parse_extraction(&answer))
}

use std::path::Path;

/// Where a project might keep its design documents, in the order they are looked for.
///
/// Probed rather than configured, and rather than detected. A setting would be one more thing to
/// fill in before the map says anything, and §10 is explicit that day one asks for nothing; a
/// detector guessing from file contents would call a long README a spec. Three conventions cover
/// this repository and the two tools that write specs into it, and a project matching none of them
/// gets §11's sentence instead of a wrong answer.
///
/// `.ai/specs` is first and is also the folder [`crate::project_map::structure`] deliberately never
/// walks. That is not a contradiction: the structure layer is about the product, and working
/// material is not product. The intention layer is about what was decided, and that is exactly
/// where the decisions are written down.
const SPEC_FOLDERS: &[&str] = &[".ai/specs", "docs/specs", "docs/superpowers/specs"];

/// Every spec of a project, by path from the root, sorted.
///
/// Sorted rather than in filesystem order, for the same reason [`crate::project_map::structure`]
/// sorts: an order the filesystem chose changes between two reads for no reason anybody can see.
pub fn specs_in(root: &Path) -> Vec<String> {
    let mut found = Vec::new();
    for folder in SPEC_FOLDERS {
        let Ok(entries) = std::fs::read_dir(root.join(folder)) else {
            continue;
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if !name.ends_with(".md") || name.starts_with('.') {
                continue;
            }
            found.push(format!("{folder}/{name}"));
        }
    }
    found.sort();
    found
}

/// What a spec is called, which is its filename without the extension.
///
/// Named by the file and not by the path, so that two projects keeping their specs in different
/// folders produce the same name for the same document — the slug is what the owner reads and what
/// a decision row carries forever.
pub fn spec_slug(path: &str) -> String {
    let file = path.rsplit('/').next().unwrap_or(path);
    file.strip_suffix(".md").unwrap_or(file).to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// A toy tree, so no test depends on the shape of the real repository.
    fn scratch(name: &str) -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!("nucleos-intent-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("scratch");
        root
    }

    fn write(root: &std::path::Path, rel: &str, body: &str) {
        let full = root.join(rel);
        fs::create_dir_all(full.parent().expect("parent")).expect("dirs");
        fs::write(full, body).expect("write");
    }

    #[test]
    fn a_kind_survives_the_round_trip_through_the_wire() {
        assert_eq!(Kind::from_wire("b"), Some(Kind::Countable));
        assert_eq!(Kind::from_wire("c"), Some(Kind::Character));
        assert_eq!(Kind::Countable.as_str(), "b");
        assert_eq!(Kind::Character.as_str(), "c");
    }

    #[test]
    fn the_kind_that_is_not_about_the_product_has_no_wire_form_at_all() {
        // §4.1's type A is decisions about scope, process, or the document itself. It is dropped
        // at extraction and never stored, so there is deliberately no `Kind` for it: a variant
        // would be a row this table can hold, and it must not be able to.
        assert_eq!(Kind::from_wire("a"), None);
        assert_eq!(Kind::from_wire(""), None);
        assert_eq!(Kind::from_wire("countable"), None);
    }

    #[test]
    fn the_storage_form_and_the_json_form_are_different_and_both_are_pinned() {
        // Two forms on purpose — the column holds `b`/`c` under a CHECK, the window reads
        // `countable`/`character`. Pinned here because nothing else compares them, and a rename
        // on either side would otherwise be found by whichever consumer broke first.
        assert_eq!(serde_json::to_string(&Kind::Countable).unwrap(), "\"countable\"");
        assert_eq!(serde_json::to_string(&Kind::Character).unwrap(), "\"character\"");
        assert_eq!(
            serde_json::from_str::<Kind>("\"character\"").unwrap(),
            Kind::Character
        );
        // The storage form is NOT the JSON form, and this is the assertion that says so.
        assert!(serde_json::from_str::<Kind>("\"b\"").is_err());
    }

    #[test]
    fn the_prompt_carries_the_document_and_says_what_a_decision_is() {
        let prompt = extraction_prompt("2026-08-22-workspace-de-projeto-design", "## 1. Alfa\n");

        // The document itself, or the model is answering about nothing.
        assert!(prompt.contains("## 1. Alfa"));
        // The slug, so a model that answers about the wrong file is visibly answering about it.
        assert!(prompt.contains("2026-08-22-workspace-de-projeto-design"));
        // The three kinds, because §4.1 is the whole of what makes this cheap to approve.
        assert!(prompt.contains("scope, process, or the document itself"));
        assert!(prompt.contains(r#""b""#));
        assert!(prompt.contains(r#""c""#));
    }

    #[test]
    fn the_prompt_refuses_the_two_failures_that_would_cost_the_most() {
        let prompt = extraction_prompt("slug", "body");

        // Inventing is the failure that makes the map worse than no map: an approved line that
        // nothing decided becomes a decision the owner then owes a stamp on forever.
        assert!(prompt.to_lowercase().contains("invent"));
        // A line with no section can never be anchored to code in slice 3, so it is a line that
        // will sit in the list forever being neither true nor false.
        assert!(prompt.contains("leave it out"));
    }

    #[test]
    fn a_document_too_long_to_send_is_cut_at_a_line_and_says_so() {
        // Not a silent truncation: a model handed half a document with no notice answers
        // confidently about a document that does not exist.
        let long = "x".repeat(MAX_SPEC_BYTES + 500);
        let prompt = extraction_prompt("slug", &long);
        assert!(prompt.contains("truncated"));
        assert!(prompt.len() < long.len() + 4_000);
    }

    #[test]
    fn a_long_document_in_the_language_these_specs_are_written_in_does_not_panic() {
        // The test above uses ASCII, where every byte index is a character boundary. These specs
        // are Portuguese, and `&source[..60_000]` panics when byte 60_000 lands inside a `ã`.
        // Seven bytes per repeat, so it does.
        let long = "çã§x".repeat(MAX_SPEC_BYTES);
        let prompt = extraction_prompt("slug", &long);
        assert!(prompt.contains("truncated"));
        assert!(prompt.len() < long.len());
    }

    #[test]
    fn the_answer_becomes_one_row_per_decision_in_the_order_it_came() {
        let answer = r###"{"decisions":[
            {"section":"## 2. Onde vive","text":"Quarto modo no workspace.","kind":"c"},
            {"section":"## 7. O carimbo","text":"Tres veredictos.","kind":"b"}
        ]}"###;

        let found = parse_extraction(answer);

        assert_eq!(found.len(), 2);
        assert_eq!(found[0].section, "## 2. Onde vive");
        assert_eq!(found[0].kind, Kind::Character);
        assert_eq!(found[0].ordinal, 1, "ordinals are 1-based and are the reading order");
        assert_eq!(found[1].ordinal, 2);
    }

    #[test]
    fn a_model_that_wraps_its_json_in_prose_is_still_understood() {
        // Every model does this at least sometimes, and refusing the answer would send the owner
        // back to press the button again for a reason that is not theirs.
        let answer = "Sure! Here is the list:\n```json\n{\"decisions\":[{\"section\":\"§1\",\
                      \"text\":\"Alfa.\",\"kind\":\"b\"}]}\n```\nHope that helps.";
        assert_eq!(parse_extraction(answer).len(), 1);
    }

    #[test]
    fn a_line_missing_what_anchors_it_is_dropped_rather_than_kept_half_useful() {
        // A decision with no section can never be joined to code, and one with no text says
        // nothing. Both would sit in the approval list forever being neither true nor false.
        let answer = r#"{"decisions":[
            {"section":"","text":"Alfa.","kind":"b"},
            {"section":"§1","text":"   ","kind":"b"},
            {"section":"§2","text":"Beta.","kind":"a"},
            {"section":"§3","text":"Gama.","kind":"c"}
        ]}"#;

        let found = parse_extraction(answer);

        assert_eq!(found.len(), 1, "only the last one is whole");
        assert_eq!(found[0].text, "Gama.");
        assert_eq!(found[0].ordinal, 1, "the ordinal counts what survived, not what was proposed");
    }

    #[test]
    fn an_answer_that_is_not_json_at_all_is_no_decisions_and_never_a_panic() {
        assert!(parse_extraction("I could not read that document.").is_empty());
        assert!(parse_extraction("").is_empty());
    }

    #[test]
    fn the_specs_of_a_project_are_found_where_projects_actually_keep_them() {
        let root = scratch("specs");
        write(&root, ".ai/specs/2026-08-22-alfa-design.md", "# Alfa");
        write(&root, "docs/specs/beta.md", "# Beta");
        write(&root, "docs/superpowers/specs/gama.md", "# Gama");
        write(&root, "docs/specs/notes.txt", "not a spec");
        write(&root, "README.md", "not a spec either");

        let found = specs_in(&root);

        assert_eq!(
            found,
            vec![
                ".ai/specs/2026-08-22-alfa-design.md".to_string(),
                "docs/specs/beta.md".to_string(),
                "docs/superpowers/specs/gama.md".to_string(),
            ],
            "sorted, so two reads of an unchanged project agree"
        );

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_project_with_nowhere_to_keep_specs_says_so_by_returning_none() {
        // §11: a project with no specs shows its structure and says what is missing. An empty list
        // is that sentence's input, and is not an error.
        let root = scratch("nospecs");
        write(&root, "src/main.rs", "fn main() {}");

        assert!(specs_in(&root).is_empty());

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_spec_is_named_by_its_file_and_not_by_where_it_sits() {
        // The slug is what the owner sees and what a decision row carries. Two projects keeping
        // specs in different folders must produce the same name for the same document.
        assert_eq!(spec_slug("docs/specs/beta.md"), "beta");
        assert_eq!(spec_slug(".ai/specs/2026-08-22-alfa-design.md"), "2026-08-22-alfa-design");
        assert_eq!(spec_slug("beta.md"), "beta");
    }

    #[test]
    fn a_null_field_costs_its_own_line_and_never_the_whole_batch() {
        // `#[serde(default)]` fills in for an ABSENT key and does nothing for one present as
        // `null`. The prompt says "leave it out" when the model cannot name a heading, and a model
        // constrained to emit JSON answers that with `null` at least as readily as by omitting the
        // key. Typed as `String` this cost the whole batch, and an empty list is this module's word
        // for "the document decided nothing" — the two must never be the same answer.
        let answer = r###"{"decisions":[
            {"section":"## 1. Alfa","text":"Alfa.","kind":"b"},
            {"section":null,"text":"Beta.","kind":"c"},
            {"section":"## 3. Gama","text":"Gama.","kind":null}
        ]}"###;

        let found = parse_extraction(answer);

        assert_eq!(found.len(), 1, "the whole line survives; the two half-formed ones do not");
        assert_eq!(found[0].text, "Alfa.");
    }

    #[test]
    fn a_line_that_is_not_even_an_object_costs_its_own_line_too() {
        let answer = r###"{"decisions":[42,{"section":"## 1. Alfa","text":"Alfa.","kind":"b"},"nonsense"]}"###;

        let found = parse_extraction(answer);

        assert_eq!(found.len(), 1);
        assert_eq!(found[0].ordinal, 1, "the ordinal counts what survived, not what was proposed");
    }

    /// A runner that answers with exactly this stdout and records what it was handed.
    ///
    /// The canned outcome is spelled out rather than defaulted, because `FakeCommandRunner` with
    /// `canned: None` answers a `result` EVENT — `{"type":"result",...,"result":"fake output"}` —
    /// and a test of what this module makes of an answer must decide what the answer was.
    fn fake_answering(stdout: &str) -> crate::runner::FakeCommandRunner {
        crate::runner::FakeCommandRunner {
            canned: std::sync::Mutex::new(Some(crate::runner::RunOutcome {
                exit_code: 0,
                stdout: stdout.to_owned(),
                stderr: String::new(),
                session_id: None,
                cost_usd: None,
                input_tokens: None,
                output_tokens: None,
                cache_read_tokens: None,
                cache_creation_tokens: None,
                num_turns: None,
                compacted: false,
            })),
            ..Default::default()
        }
    }

    /// A runner whose launch fails — nobody read anything, as opposed to reading and finding none.
    ///
    /// `fail_times` is a countdown the fake decrements, so `1` fails the first call and would let a
    /// second through. One call is all this makes, and a larger number would be a retry budget this
    /// function does not have.
    fn fake_failing() -> crate::runner::FakeCommandRunner {
        crate::runner::FakeCommandRunner {
            fail_times: std::sync::Mutex::new(1),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn the_decisions_of_a_spec_come_back_from_whichever_brain_was_asked() {
        let runner = fake_answering(
            r#"{"decisions":[{"section":"§1","text":"Alfa.","kind":"c"}]}"#,
        );

        let found = extract(&runner, "slug", "## §1\nbody").await.expect("extract");

        assert_eq!(found.len(), 1);
        assert_eq!(found[0].text, "Alfa.");
    }

    #[tokio::test]
    async fn a_model_that_says_nothing_useful_is_an_empty_list_and_not_an_error() {
        // The owner pressed a button. "This spec produced nothing, press again or pick another
        // brain" is a sentence; a 500 is not.
        let runner = fake_answering("I am unable to help with that.");
        assert!(extract(&runner, "slug", "body").await.expect("extract").is_empty());
    }

    #[tokio::test]
    async fn a_runner_that_fails_is_reported_rather_than_read_as_an_empty_spec() {
        // These two must never collapse. An empty list means the model read it and found nothing;
        // a failure means nobody read anything, and telling the owner the first when the second
        // happened is exactly the false confidence this whole feature exists to cure.
        let runner = fake_failing();
        assert!(extract(&runner, "slug", "body").await.is_err());
    }

    #[tokio::test]
    async fn the_document_reaches_the_model_and_the_model_gets_no_tools() {
        // Two properties in one assertion because both are about what the runner was HANDED, which
        // is the only place either can be observed. A reader that could edit the repository is not
        // reading it.
        let runner = fake_answering(r#"{"decisions":[]}"#);
        let _ = extract(&runner, "the-slug", "## 1. Alfa\n").await.expect("extract");

        let prompt = runner.last_prompt.lock().unwrap().clone().expect("a prompt was sent");
        assert!(prompt.contains("## 1. Alfa"));
        assert!(prompt.contains("the-slug"));
        assert_eq!(
            *runner.last_tool_policy.lock().unwrap(),
            Some(crate::runner::ToolPolicy::None)
        );
    }

    #[tokio::test]
    async fn the_cloud_brain_answers_in_events_and_its_answer_is_still_found() {
        // Not in the plan, and it is the difference between this working and this appearing to.
        // `ClaudeCliRunner` — half of the owner's choice — returns the whole `stream-json`
        // transcript as `stdout`, one event per line. `parse_extraction` reads first-`{`-to-last-`}`
        // across whatever it is given, so on that stream it deserialises nothing and returns an
        // empty list: every cloud extraction would say "this spec decided nothing" while the model
        // had answered in full. That is the exact collapse the test above forbids, arriving by the
        // other door.
        let runner = fake_answering(
            r#"{"type":"system","subtype":"init","session_id":"s"}
{"type":"result","subtype":"success","result":"{\"decisions\":[{\"section\":\"§1\",\"text\":\"Alfa.\",\"kind\":\"b\"}]}"}"#,
        );

        let found = extract(&runner, "slug", "body").await.expect("extract");

        assert_eq!(found.len(), 1, "the answer lives inside the final `result` event");
        assert_eq!(found[0].text, "Alfa.");
    }
}
