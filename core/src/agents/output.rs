//! The shape an agent's answer must have, and the deterministic check that it has it.
//!
//! One file, two readers: `output.md` is sent to the model verbatim (prompt layer 6) and parsed here
//! into the validator's spec, so there is no second source for what "valid" means. The format is
//! documented in `.ai/plans/2026-10-20-agentes/b1-launch.md` section 1:
//!
//! - each `## Heading` is a required section, in this order; text before the first one is
//!   instruction only;
//! - inside a section, a line starting `one-of: a | b` (closed value set, checked against the first
//!   non-blank line) or `match: <regex>` (at least one line must match) is a directive. Directives
//!   stay in the text the model reads;
//! - an `alt-shape: A | B` line in the text before the first `##` names the headings of an
//!   alternative whole-output shape (e.g. a coder's `## Escalation` block that replaces everything).
//!   An answer that holds the first alt heading and none of the required ones is validated against
//!   the alt headings instead (each present, in order, non-empty). Lines inside ``` fences are
//!   never headings nor directives.
//!
//! Outcome hook (for the code that runs the agent, see `team/launch.rs`): validate the answer; on
//! `Err(report)` rerun ONCE on the same engine and model with [`retry_prompt`]; on a second `Err`
//! fail the launch and, when the run was routed, report
//! `route_advice::report_detached(router, decision_id, Outcome::Fail)`.

use regex::Regex;

/// One required `##` section and what it must hold.
#[derive(Debug, Clone)]
pub struct Section {
    pub heading: String,
    /// Closed value set; empty when the section declares none.
    pub one_of: Vec<String>,
    /// Line patterns, each of which at least one line of the section must match.
    pub matches: Vec<Regex>,
}

/// What `output.md` (or a caller's contract text) declares.
#[derive(Debug, Clone)]
pub struct OutputShape {
    /// The text as written: sent to the model unchanged.
    pub instruction: String,
    pub sections: Vec<Section>,
    /// Headings of the alternative whole-output shape (`alt-shape:`), if declared.
    pub alt: Option<Vec<String>>,
}

impl OutputShape {
    /// A shape that validates nothing and tells the model `text`.
    pub fn free(text: &str) -> Self {
        Self {
            instruction: text.to_owned(),
            sections: Vec::new(),
            alt: None,
        }
    }
}

/// Why an answer is not the declared shape: one line per problem, all of them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputReport {
    pub problems: Vec<String>,
}

impl std::fmt::Display for OutputReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "Your answer does not follow the required output format:")?;
        for problem in &self.problems {
            writeln!(f, "- {problem}")?;
        }
        Ok(())
    }
}

/// The retry prompt: the original one, then the validator's report and the instruction to answer
/// again in the required format.
pub fn retry_prompt(original: &str, report: &OutputReport) -> String {
    format!("{original}\n\n{report}\nAnswer again, following the required output format exactly.\n")
}

/// A `## Heading` line (exactly two hashes), with the heading text.
fn heading_of(line: &str) -> Option<&str> {
    let rest = line.strip_prefix("## ")?;
    let name = rest.trim();
    (!name.is_empty()).then_some(name)
}

/// Splits `text` into `(heading, body lines)` sections, ignoring `##` lines inside code fences.
/// Text before the first heading is dropped.
fn split_sections(text: &str) -> Vec<(String, Vec<&str>)> {
    let mut sections: Vec<(String, Vec<&str>)> = Vec::new();
    let mut fenced = false;
    for line in text.lines() {
        if line.trim_start().starts_with("```") {
            fenced = !fenced;
        } else if !fenced && let Some(name) = heading_of(line.trim_end()) {
            sections.push((name.to_owned(), Vec::new()));
            continue;
        }
        if let Some((_, body)) = sections.last_mut() {
            body.push(line);
        }
    }
    sections
}

/// The `alt-shape:` headings declared in the text before the first heading (outside fences).
fn alt_shape(text: &str) -> Result<Option<Vec<String>>, String> {
    let mut fenced = false;
    let mut alt: Option<Vec<String>> = None;
    for line in text.lines() {
        if line.trim_start().starts_with("```") {
            fenced = !fenced;
        } else if !fenced {
            if heading_of(line.trim_end()).is_some() {
                break;
            }
            if let Some(names) = line.trim().strip_prefix("alt-shape:") {
                let names: Vec<String> = names
                    .split('|')
                    .map(|name| name.trim().trim_start_matches("## ").to_owned())
                    .filter(|name| !name.is_empty())
                    .collect();
                if names.is_empty() {
                    return Err("`alt-shape:` lists no heading".to_owned());
                }
                alt.get_or_insert_with(Vec::new).extend(names);
            }
        }
    }
    Ok(alt)
}

pub fn parse_output_md(text: &str) -> Result<OutputShape, String> {
    let mut sections: Vec<Section> = Vec::new();
    for (heading, body) in split_sections(text) {
        if sections.iter().any(|section| section.heading == heading) {
            return Err(format!("duplicate section `## {heading}`"));
        }
        let mut section = Section {
            heading,
            one_of: Vec::new(),
            matches: Vec::new(),
        };
        let mut fenced = false;
        for line in body {
            let line = line.trim();
            if line.starts_with("```") {
                fenced = !fenced;
                continue;
            }
            if fenced {
                continue;
            }
            if let Some(values) = line.strip_prefix("one-of:") {
                let values: Vec<String> = values
                    .split('|')
                    .map(|value| value.trim().to_owned())
                    .filter(|value| !value.is_empty())
                    .collect();
                if values.is_empty() {
                    return Err(format!(
                        "`## {}`: `one-of:` lists no value",
                        section.heading
                    ));
                }
                section.one_of.extend(values);
            } else if let Some(pattern) = line.strip_prefix("match:") {
                let regex = Regex::new(pattern.trim()).map_err(|error| {
                    format!("`## {}`: bad `match:` pattern: {error}", section.heading)
                })?;
                section.matches.push(regex);
            }
        }
        sections.push(section);
    }
    Ok(OutputShape {
        instruction: text.to_owned(),
        sections,
        alt: alt_shape(text)?,
    })
}

/// Trims what a model wraps a bare value in.
fn bare(line: &str) -> &str {
    line.trim_matches(|c: char| c.is_whitespace() || c == '`' || c == '*')
}

/// Checks `answer` against `shape`. Deterministic; every problem is reported, not the first.
pub fn validate(shape: &OutputShape, answer: &str) -> Result<(), OutputReport> {
    let found = split_sections(answer);
    let mut problems: Vec<String> = Vec::new();
    if let Some(alt) = &shape.alt {
        let has = |name: &str| found.iter().any(|(heading, _)| heading == name);
        if has(&alt[0]) && !shape.sections.iter().any(|spec| has(&spec.heading)) {
            return validate_alt(alt, &found);
        }
    }
    // Required headings must appear in declared order: each search resumes after the previous hit.
    let mut cursor = 0;
    for spec in &shape.sections {
        let Some(offset) = found[cursor..]
            .iter()
            .position(|(heading, _)| *heading == spec.heading)
        else {
            if found.iter().any(|(heading, _)| *heading == spec.heading) {
                problems.push(format!(
                    "section `## {}` is out of order; the required order is {}",
                    spec.heading,
                    order_of(shape)
                ));
            } else {
                problems.push(format!("missing section `## {}`", spec.heading));
            }
            continue;
        };
        let body = &found[cursor + offset].1;
        cursor += offset + 1;
        let lines: Vec<&str> = body
            .iter()
            .map(|line| line.trim())
            .filter(|line| !line.is_empty())
            .collect();
        if lines.is_empty() {
            problems.push(format!("section `## {}` is empty", spec.heading));
            continue;
        }
        if !spec.one_of.is_empty() && !spec.one_of.iter().any(|value| value == bare(lines[0])) {
            problems.push(format!(
                "section `## {}` must be exactly one of: {}",
                spec.heading,
                spec.one_of.join(" | ")
            ));
        }
        for pattern in &spec.matches {
            if !lines.iter().any(|line| pattern.is_match(line)) {
                problems.push(format!(
                    "section `## {}` has no line matching `{}`",
                    spec.heading,
                    pattern.as_str()
                ));
            }
        }
    }
    if problems.is_empty() {
        Ok(())
    } else {
        Err(OutputReport { problems })
    }
}

/// Checks an answer against the alternative shape: every heading present, in order, non-empty.
fn validate_alt(alt: &[String], found: &[(String, Vec<&str>)]) -> Result<(), OutputReport> {
    let mut problems: Vec<String> = Vec::new();
    let mut cursor = 0;
    for name in alt {
        let Some(offset) = found[cursor..]
            .iter()
            .position(|(heading, _)| heading == name)
        else {
            problems.push(format!("missing section `## {name}`"));
            continue;
        };
        let body = &found[cursor + offset].1;
        cursor += offset + 1;
        if body.iter().all(|line| line.trim().is_empty()) {
            problems.push(format!("section `## {name}` is empty"));
        }
    }
    if problems.is_empty() {
        Ok(())
    } else {
        Err(OutputReport { problems })
    }
}

fn order_of(shape: &OutputShape) -> String {
    shape
        .sections
        .iter()
        .map(|section| format!("`## {}`", section.heading))
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    const REVIEWER: &str = "Reply in this format.\n\n## Findings\nOne bullet each.\nmatch: ^- (\\[(high|medium|low)\\] \\S+:\\d+ — .+|none)$\n\n## Verdict\nOne word.\none-of: approved | changes\n";

    fn shape() -> OutputShape {
        parse_output_md(REVIEWER).unwrap()
    }

    #[test]
    fn a_conforming_answer_is_valid() {
        let answer = "## Findings\n- [high] src/a.rs:12 — unchecked index\n- [low] b.rs:3 — nit\n\n## Verdict\nchanges\n";
        assert!(validate(&shape(), answer).is_ok());
        assert!(
            validate(
                &shape(),
                "preamble\n## Findings\n- none\n## Verdict\n`approved`\n"
            )
            .is_ok()
        );
    }

    #[test]
    fn the_instruction_is_the_file_verbatim() {
        assert_eq!(shape().instruction, REVIEWER);
        assert_eq!(shape().sections.len(), 2);
    }

    #[test]
    fn every_problem_is_reported() {
        let answer = "## Findings\nlooks fine to me\n## Verdict\nmaybe\n";
        let report = validate(&shape(), answer).unwrap_err();
        assert_eq!(report.problems.len(), 2, "{report}");
        assert!(report.problems[0].contains("no line matching"));
        assert!(report.problems[1].contains("approved | changes"));
        assert!(report.to_string().contains("- section `## Verdict`"));
    }

    #[test]
    fn a_missing_empty_or_misordered_section_is_named() {
        let missing = validate(&shape(), "## Findings\n- none\n").unwrap_err();
        assert_eq!(missing.problems, vec!["missing section `## Verdict`"]);
        let empty = validate(&shape(), "## Findings\n- none\n## Verdict\n\n").unwrap_err();
        assert_eq!(empty.problems, vec!["section `## Verdict` is empty"]);
        let swapped =
            validate(&shape(), "## Verdict\napproved\n## Findings\n- none\n").unwrap_err();
        assert!(swapped.problems[0].contains("out of order"), "{swapped}");
    }

    #[test]
    fn a_heading_inside_a_fence_is_not_a_section() {
        let answer = "## Findings\n```\n## Verdict\n```\n- none\n";
        let report = validate(&shape(), answer).unwrap_err();
        assert_eq!(report.problems, vec!["missing section `## Verdict`"]);
    }

    #[test]
    fn a_bad_declaration_is_refused() {
        assert!(
            parse_output_md("## A\n## A\n")
                .unwrap_err()
                .contains("duplicate")
        );
        assert!(
            parse_output_md("## A\nmatch: (\n")
                .unwrap_err()
                .contains("bad `match:`")
        );
        assert!(
            parse_output_md("## A\none-of: |\n")
                .unwrap_err()
                .contains("no value")
        );
        let free = parse_output_md("Just write prose.").unwrap();
        assert!(free.sections.is_empty());
        assert!(validate(&free, "anything").is_ok());
    }

    const CODER: &str = "Intro.\nalt-shape: Escalation\n\n## Changes\nmatch: ^- \\S.* — .+$\n\n## Risks\nmatch: ^(- .+|none)$\n```\nmatch: (\n## Ghost\n```\n";

    #[test]
    fn an_alternative_whole_output_shape_is_accepted() {
        let shape = parse_output_md(CODER).unwrap();
        assert_eq!(shape.alt, Some(vec!["Escalation".to_owned()]));
        assert_eq!(shape.sections.len(), 2, "a fenced heading is not a section");
        assert!(validate(&shape, "## Escalation\nblocked: no access\n").is_ok());
        assert!(validate(&shape, "## Changes\n- a.rs — why\n## Risks\nnone\n").is_ok());
        let empty = validate(&shape, "## Escalation\n\n").unwrap_err();
        assert_eq!(empty.problems, vec!["section `## Escalation` is empty"]);
        // Mixed with a required heading it is not the alt shape: the normal rules apply.
        let mixed = validate(&shape, "## Escalation\nx\n## Changes\n- a.rs — why\n").unwrap_err();
        assert_eq!(mixed.problems, vec!["missing section `## Risks`"]);
        assert!(
            parse_output_md("alt-shape: |\n## A\nx\n")
                .unwrap_err()
                .contains("no heading")
        );
    }

    #[test]
    fn the_retry_prompt_carries_the_original_and_the_report() {
        let report = validate(&shape(), "nope").unwrap_err();
        let retry = retry_prompt("ORIGINAL", &report);
        assert!(retry.starts_with("ORIGINAL\n"));
        assert!(retry.contains("missing section `## Findings`"));
        assert!(retry.contains("Answer again"));
    }
}
