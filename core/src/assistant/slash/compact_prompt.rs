//! The text a `/compact` turn sends the model (spec §7.2): a versioned prompt asking for a
//! `nucleos-compact v1` handoff document and nothing else.
//!
//! Adapted from the owner's `nucleos-compact` skill with three changes: no file is written (the
//! answer itself is the handoff), the header names the format version, and the skill's "Then"
//! part is gone. Every engine receives the same text. The version string is stored on each
//! `chat_compactions` row, so a changed prompt must come with a new version.

/// The format a compaction written with [`COMPACT_PROMPT_V1`] follows.
pub const COMPACT_PROMPT_VERSION: &str = "nucleos-compact v1";

/// The compaction prompt; `{date}` is replaced by [`build`].
pub const COMPACT_PROMPT_V1: &str = "\
You are compacting THIS conversation into a handoff document. A new context will continue the work from that document alone, plus anything it re-reads from disk. Anything you leave out is lost.

Reply with the handoff document only. Do not call tools, do not continue the task, do not ask questions.

Use exactly these sections:

# Handoff — <one-line title>
Compacted: {date}  ·  Format: nucleos-compact v1

## 1. Request and intent
What the user wants, in their words where it matters; constraints and preferences they stated (quote them).

## 2. Decisions and why
Each decision taken, who took it (user / assistant), and the reason.

## 3. State of the work
Done / in progress / pending, and the single next step.

## 4. Files
Each file read or changed: path and what matters about it. Paths only — do not paste file contents that can be re-read.

## 5. Exact facts
Literal values the work produced or found that cannot be re-read from disk cheaply. Copy verbatim, never paraphrase:
- measured results: test counts, failing/passing test names, timings, sizes, benchmark numbers;
- error messages and the command or URL that produced them;
- findings from outside the repository: web pages, APIs, prices, specifications, versions;
- identifiers: branch and worktree names, ids, ports, constant values with file:line.
Prefer completeness here over prose elsewhere.

## 6. User messages
Every user message, condensed, keeping every instruction.

## 7. Latest exchange (verbatim)
The user's last request before this compaction request and your reply to it, copied as they were (trim tool output to what matters).

Budget: sections 1–6 together at most ~10k tokens; section 7 at most ~12k tokens.";

/// The prompt for one compaction: `date` fills the header, and a non-blank `focus` is appended
/// last as the user's extra instruction.
pub fn build(focus: Option<&str>, date: &str) -> String {
    let mut prompt = COMPACT_PROMPT_V1.replace("{date}", date);
    if let Some(focus) = focus.map(str::trim).filter(|f| !f.is_empty()) {
        prompt.push_str("\n\nAdditional focus from the user: ");
        prompt.push_str(focus);
    }
    prompt
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_and_seven_sections_in_order() {
        assert_eq!(COMPACT_PROMPT_VERSION, "nucleos-compact v1");
        let p = build(None, "2026-10-28");
        let headings = [
            "## 1. Request and intent",
            "## 2. Decisions and why",
            "## 3. State of the work",
            "## 4. Files",
            "## 5. Exact facts",
            "## 6. User messages",
            "## 7. Latest exchange (verbatim)",
        ];
        let mut from = 0;
        for h in headings {
            let at = p[from..]
                .find(h)
                .unwrap_or_else(|| panic!("missing or out of order: {h}"));
            from += at + h.len();
        }
        assert_eq!(p.matches("\n## ").count(), 7);
    }

    #[test]
    fn header_and_reply_only_rule_and_no_then() {
        let p = build(None, "2026-10-28");
        assert!(p.contains("# Handoff — <one-line title>\n"));
        assert!(p.contains("Compacted: 2026-10-28  ·  Format: nucleos-compact v1\n"));
        assert!(p.contains(&format!("Format: {COMPACT_PROMPT_VERSION}")));
        assert!(!p.contains("{date}"));
        assert!(p.contains(
            "Reply with the handoff document only. Do not call tools, do not continue the task, do not ask questions."
        ));
        assert!(!p.contains("Then"));
        assert!(p.ends_with("section 7 at most ~12k tokens."));
    }

    #[test]
    fn focus_is_appended_last_and_blank_focus_is_ignored() {
        let p = build(Some("  the migration plan "), "2026-10-28");
        assert!(p.ends_with("\n\nAdditional focus from the user: the migration plan"));
        assert_eq!(build(Some("   "), "d"), build(None, "d"));
        assert_eq!(build(Some(""), "d"), build(None, "d"));
        assert!(!build(None, "d").contains("Additional focus"));
    }
}
