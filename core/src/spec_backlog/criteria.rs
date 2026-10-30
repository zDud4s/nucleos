//! Validation of what the extraction agent returns: `parse` turns the agent's raw output into
//! an `Extraction` of the kind of spec, its criteria, its dependencies and its open questions.

#[cfg(test)]
mod tests {
    use super::{parse, Extraction, ParseError};
    use crate::spec_backlog::model::{CriterionKind, CriterionStatus, SpecKind};
    use crate::spec_backlog::prompts::{extraction_prompt, split_sections, MAX_SPEC_CHARS};

    const KNOWN_DEP: &str = ".ai/specs/known.md";
    const UNKNOWN_DEP: &str = ".ai/specs/ghost.md";

    // `Extraction` pins no derives, so tests compare fields and go through these two helpers
    // rather than `assert_eq!` on the whole `Result` or `unwrap_err`.
    fn parse_ok(raw: &str, known_paths: &[String]) -> Extraction {
        match parse(raw, known_paths) {
            Ok(extraction) => extraction,
            Err(err) => panic!("expected a valid extraction, got {err:?} for: {raw}"),
        }
    }

    fn parse_err(raw: &str, known_paths: &[String]) -> ParseError {
        match parse(raw, known_paths) {
            Ok(_) => panic!("expected a parse error for: {raw}"),
            Err(err) => err,
        }
    }

    fn known() -> Vec<String> {
        vec![KNOWN_DEP.to_string()]
    }

    fn assert_invalid_shape(raw: &str) {
        let err = parse_err(raw, &known());
        assert!(
            matches!(err, ParseError::InvalidShape(_)),
            "expected InvalidShape, got {err:?} for: {raw}"
        );
    }

    fn assert_valid_block(extraction: &Extraction) {
        assert_eq!(extraction.kind, SpecKind::Spec);
        assert_eq!(extraction.criteria.len(), 4);

        let first = &extraction.criteria[0];
        assert_eq!(first.text, "The scan opens no file for writing");
        assert_eq!(first.kind, CriterionKind::Behavior);
        assert_eq!(first.verify_hint.as_deref(), Some("core/src/scan.rs"));
        assert_eq!(first.status, CriterionStatus::Open);

        let second = &extraction.criteria[1];
        assert_eq!(second.text, "A test covers the rescan");
        assert_eq!(second.kind, CriterionKind::Test);
        assert_eq!(second.verify_hint, None);
        assert_eq!(second.status, CriterionStatus::Open);

        assert_eq!(extraction.criteria[2].kind, CriterionKind::Doc);
        assert_eq!(extraction.criteria[3].kind, CriterionKind::Decision);

        assert_eq!(extraction.depends_on, vec![KNOWN_DEP.to_string()]);
        assert_eq!(
            extraction.questions,
            vec!["Which branch lands this?".to_string()]
        );
    }

    const VALID_OBJECT: &str = r#"{
        "kind": "spec",
        "criteria": [
            {"text": "The scan opens no file for writing", "kind": "behavior", "verify_hint": "core/src/scan.rs"},
            {"text": "A test covers the rescan", "kind": "test", "verify_hint": null},
            {"text": "The README names the route", "kind": "doc", "verify_hint": null},
            {"text": "The owner chose the limit", "kind": "decision", "verify_hint": null}
        ],
        "depends_on": [".ai/specs/known.md"],
        "questions": ["Which branch lands this?"]
    }"#;

    #[test]
    fn parse_valid_block() {
        let fenced =
            format!("Here is the extraction.\n\n```json\n{VALID_OBJECT}\n```\n\nThat is all.\n");
        assert_valid_block(&parse_ok(&fenced, &known()));

        // The same object is accepted bare, with or without prose around it.
        assert_valid_block(&parse_ok(VALID_OBJECT, &known()));
        let bare_with_prose = format!("Here is the extraction.\n{VALID_OBJECT}\nThat is all.\n");
        assert_valid_block(&parse_ok(&bare_with_prose, &known()));
    }

    #[test]
    fn rejects_empty_criterion() {
        let empty_first = r#"{"kind": "spec", "criteria": [
            {"text": "", "kind": "behavior", "verify_hint": null},
            {"text": "Fine", "kind": "test", "verify_hint": null}
        ], "depends_on": [], "questions": []}"#;
        assert_eq!(
            parse_err(empty_first, &[]),
            ParseError::EmptyCriterion { index: 0 }
        );

        let whitespace_third = r#"{"kind": "spec", "criteria": [
            {"text": "Fine", "kind": "behavior", "verify_hint": null},
            {"text": "Also fine", "kind": "test", "verify_hint": null},
            {"text": "  \n\t ", "kind": "doc", "verify_hint": null}
        ], "depends_on": [], "questions": []}"#;
        assert_eq!(
            parse_err(whitespace_third, &[]),
            ParseError::EmptyCriterion { index: 2 }
        );
    }

    #[test]
    fn rejects_non_json() {
        assert_eq!(
            parse_err("I could not read the spec, sorry.", &[]),
            ParseError::NoJsonBlock
        );
        assert_eq!(parse_err("", &[]), ParseError::NoJsonBlock);
    }

    #[test]
    fn unknown_depends_on_becomes_question() {
        let raw = r#"{"kind": "spec", "criteria": [
            {"text": "Fine", "kind": "behavior", "verify_hint": null}
        ], "depends_on": [".ai/specs/known.md", ".ai/specs/ghost.md"],
        "questions": ["Which branch lands this?"]}"#;

        let extraction = parse_ok(raw, &known());

        assert_eq!(extraction.depends_on, vec![KNOWN_DEP.to_string()]);
        assert_eq!(extraction.questions.len(), 2, "{:?}", extraction.questions);
        assert!(extraction
            .questions
            .contains(&"Which branch lands this?".to_string()));
        assert!(
            extraction.questions.iter().any(|q| q.contains(UNKNOWN_DEP)),
            "no question names the unknown path: {:?}",
            extraction.questions
        );
    }

    #[test]
    fn review_kind_allows_zero_criteria() {
        for (raw_kind, expected) in [("review", SpecKind::Review), ("note", SpecKind::Note)] {
            let raw = format!(
                r#"{{"kind": "{raw_kind}", "criteria": [], "depends_on": [], "questions": []}}"#
            );
            let extraction = parse_ok(&raw, &[]);
            assert_eq!(extraction.kind, expected);
            assert!(extraction.criteria.is_empty());
        }
    }

    #[test]
    fn spec_kind_with_zero_criteria_is_rejected() {
        let raw = r#"{"kind": "spec", "criteria": [], "depends_on": [], "questions": []}"#;
        assert_eq!(parse_err(raw, &[]), ParseError::NoCriteria);
    }

    #[test]
    fn ambiguous_criterion_is_doubt() {
        let raw = r#"{"kind": "spec", "criteria": [
            {"text": "Flagged", "kind": "behavior", "verify_hint": null, "ambiguous": true},
            {"text": "Cleared", "kind": "behavior", "verify_hint": null, "ambiguous": false},
            {"text": "Silent", "kind": "behavior", "verify_hint": null}
        ], "depends_on": [], "questions": []}"#;

        let extraction = parse_ok(raw, &[]);

        assert_eq!(extraction.criteria[0].status, CriterionStatus::Doubt);
        assert_eq!(extraction.criteria[1].status, CriterionStatus::Open);
        assert_eq!(extraction.criteria[2].status, CriterionStatus::Open);
    }

    // The owner's rule: only an independent reviewer confirms a criterion, so the extraction
    // agent's own claim about status carries no weight.
    #[test]
    fn extraction_cannot_mark_a_criterion_met() {
        let raw = r#"{"kind": "spec", "criteria": [
            {"text": "Claimed", "kind": "behavior", "verify_hint": null, "status": "met"},
            {"text": "Claimed and flagged", "kind": "behavior", "verify_hint": null,
             "status": "met", "ambiguous": true}
        ], "depends_on": [], "questions": []}"#;

        let extraction = parse_ok(raw, &[]);

        assert_eq!(extraction.criteria[0].status, CriterionStatus::Open);
        assert_eq!(extraction.criteria[1].status, CriterionStatus::Doubt);
    }

    #[test]
    fn unknown_criterion_kind_is_rejected() {
        assert_invalid_shape(
            r#"{"kind": "spec", "criteria": [
                {"text": "Fine", "kind": "vibes", "verify_hint": null}
            ], "depends_on": [], "questions": []}"#,
        );
    }

    #[test]
    fn missing_fields_are_rejected() {
        let criterion = r#"{"text": "Fine", "kind": "behavior", "verify_hint": null}"#;

        // Top-level fields.
        assert_invalid_shape(&format!(
            r#"{{"criteria": [{criterion}], "depends_on": [], "questions": []}}"#
        ));
        assert_invalid_shape(r#"{"kind": "spec", "depends_on": [], "questions": []}"#);
        assert_invalid_shape(&format!(
            r#"{{"kind": "spec", "criteria": [{criterion}], "questions": []}}"#
        ));
        assert_invalid_shape(&format!(
            r#"{{"kind": "spec", "criteria": [{criterion}], "depends_on": []}}"#
        ));

        // Criterion fields.
        assert_invalid_shape(
            r#"{"kind": "spec", "criteria": [{"kind": "behavior", "verify_hint": null}],
                "depends_on": [], "questions": []}"#,
        );
        assert_invalid_shape(
            r#"{"kind": "spec", "criteria": [{"text": "Fine", "verify_hint": null}],
                "depends_on": [], "questions": []}"#,
        );
    }

    #[test]
    fn splits_large_spec_by_h2_sections_over_80k_chars() {
        let filler = "x".repeat(30_000);

        // A preamble, an H3 and a mid-line `## ` that must not split, no trailing newline.
        let with_preamble = format!(
            "Preamble line\n\
             ## First\n{filler}\n### Not a split\nsee ## not a heading\n\
             ## Second\n{filler}\n\
             ## Third\n{filler}"
        );
        assert!(with_preamble.chars().count() > MAX_SPEC_CHARS);

        let sections = split_sections(&with_preamble);
        assert_eq!(sections.len(), 4);
        assert!(sections[0].starts_with("Preamble line"));
        assert!(sections[1].starts_with("## First\n"));
        assert!(sections[1].contains("### Not a split"));
        assert!(sections[2].starts_with("## Second\n"));
        assert!(sections[3].starts_with("## Third\n"));
        assert_eq!(sections.concat(), with_preamble);

        // No preamble: no empty element ahead of the first heading.
        let without_preamble = format!("## First\n{filler}\n## Second\n{filler}\n## Third\n{filler}\n");
        let sections = split_sections(&without_preamble);
        assert_eq!(sections.len(), 3);
        assert!(sections.iter().all(|section| section.starts_with("## ")));
        assert_eq!(sections.concat(), without_preamble);

        // CRLF line endings survive the round trip.
        let crlf =
            format!("## First\r\n{filler}\r\n## Second\r\n{filler}\r\n## Third\r\n{filler}\r\n");
        let sections = split_sections(&crlf);
        assert_eq!(sections.len(), 3);
        assert_eq!(sections.concat(), crlf);

        // Several headings under the limit stay one element, unchanged.
        let small = "Preamble\n## First\nbody\n## Second\nbody\n";
        assert_eq!(split_sections(small), vec![small.to_string()]);

        // The limit is inclusive: exactly at it is one element, one char over it splits.
        let head = "## First\nbody\n## Second\n";
        let at_limit = format!("{head}{}", "y".repeat(MAX_SPEC_CHARS - head.chars().count()));
        assert_eq!(at_limit.chars().count(), MAX_SPEC_CHARS);
        assert_eq!(split_sections(&at_limit), vec![at_limit.clone()]);

        let over_limit = format!("{at_limit}y");
        let sections = split_sections(&over_limit);
        assert_eq!(sections.len(), 2);
        assert_eq!(sections.concat(), over_limit);
    }

    #[test]
    fn split_counts_chars_not_bytes() {
        // Two bytes per char: 50 000 chars of body are 100 000 bytes.
        let body = "é".repeat(25_000);
        let text = format!("## First\n{body}\n## Second\n{body}\n");
        assert!(text.len() > MAX_SPEC_CHARS);
        assert!(text.chars().count() < MAX_SPEC_CHARS);

        assert_eq!(split_sections(&text), vec![text.clone()]);
    }

    #[test]
    fn prompt_embeds_spec_text_and_forbids_edits() {
        let rel_path = ".ai/specs/2026-10-30-sample.md";
        let section = "## Sample section\nThe marker-zebra behaviour must hold.\n";

        let prompt = extraction_prompt(rel_path, section, None);

        assert!(prompt.contains(section));
        assert!(prompt.contains(rel_path));
        assert!(prompt.contains("Do not edit"));
        assert!(prompt.contains("depends_on"));
        assert!(prompt.contains("ambiguous"));
    }

    #[test]
    fn prompt_appends_the_previous_error_on_retry() {
        let rel_path = ".ai/specs/2026-10-30-sample.md";
        let section = "## Sample section\nbody\n";
        let err = "previous attempt failed: marker-quokka";

        let retry = extraction_prompt(rel_path, section, Some(err));
        assert!(retry.contains(err));
        assert!(retry.contains(section));

        let first_try = extraction_prompt(rel_path, section, None);
        assert!(!first_try.contains(err));

        // What the retry really carries is a `ParseError` rendered through `Display`.
        let display = ParseError::NoCriteria.to_string();
        assert!(!display.is_empty());
        assert!(extraction_prompt(rel_path, section, Some(&display)).contains(&display));
    }
}
