//! The independent reviewer of a spec's criteria: what it is shown, how it is asked, and how its
//! answer is read back.
//!
//! The reviewer is a fresh run with no tools, so everything it may judge from is embedded in its
//! prompt: the files the criteria name and the files the feature branch changed, numbered by line
//! and held to a size cap. When a cap cut something the prompt says so, and the reviewer is told
//! to answer `doubt` rather than assume.

use std::path::Path;

use super::audit::{self, CriterionReview, ReviewerAnswer, ReviewerRunner};
use super::daemon::{no_tools_run_request, run_no_tools};
use super::model::{Criterion, CriterionStatus, Spec};
use super::verdict::ReviewerRun;
use crate::state::AppState;

/// Bytes of one file the reviewer is shown.
pub(super) const REVIEW_FILE_CAP: usize = 24 * 1024;
/// Bytes of all files together.
const REVIEW_TOTAL_CAP: usize = 160 * 1024;
/// A file larger than this is not even read.
const REVIEW_READ_LIMIT: u64 = 4 * 1024 * 1024;

/// One file as the reviewer sees it: numbered lines, so `path:line` evidence is exact.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ReviewFile {
    pub(super) path: String,
    pub(super) text: String,
}

/// What the reviewer is given to read, and whether anything was left out.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct ReviewMaterial {
    pub(super) files: Vec<ReviewFile>,
    /// A cap cut something, a file was binary or too large, or the changed-file list could not be
    /// read: the reviewer may not have seen everything.
    pub(super) cut: bool,
}

/// File-looking tokens (`dir/name.ext`, `name.ext`, with an optional `:line`) in the criteria and
/// their verify hints, in order, once each. Whether a file exists is decided at read time.
pub(super) fn paths_named_by(criteria: &[Criterion]) -> Vec<String> {
    let mut found: Vec<String> = Vec::new();
    for criterion in criteria {
        let hint = criterion.verify_hint.as_deref().unwrap_or("");
        for token in criterion
            .text
            .split_whitespace()
            .chain(hint.split_whitespace())
        {
            let token = token
                .trim_matches(|c: char| {
                    matches!(
                        c,
                        '`' | '\'' | '"' | '(' | ')' | ',' | ';' | '[' | ']' | '<' | '>'
                    )
                })
                .trim_end_matches(['.', ':']);
            let token = match token.rsplit_once(':') {
                Some((path, line))
                    if !line.is_empty() && line.chars().all(|c| c.is_ascii_digit()) =>
                {
                    path
                }
                _ => token,
            };
            let token = token.replace('\\', "/");
            let token = token.strip_prefix("./").unwrap_or(&token).to_owned();
            let name = token.rsplit('/').next().unwrap_or("");
            let looks_like_file = name.rsplit_once('.').is_some_and(|(stem, ext)| {
                !stem.is_empty()
                    && (1..=5).contains(&ext.len())
                    && ext.chars().all(|c| c.is_ascii_alphanumeric())
            });
            if looks_like_file && !token.contains("://") && !found.contains(&token) {
                found.push(token);
            }
        }
    }
    found
}

/// `text` as numbered lines, whole lines only, at most `cap` bytes; the flag says a line was left out.
pub(super) fn numbered_prefix(text: &str, cap: usize) -> (String, bool) {
    let mut out = String::new();
    for (index, line) in text.lines().enumerate() {
        let row = format!("{}: {line}\n", index + 1);
        if out.len() + row.len() > cap {
            return (out, true);
        }
        out.push_str(&row);
    }
    (out, false)
}

/// The files the feature branch changed against the integration branch, or `None` when git could
/// not say.
async fn changed_files(worktree: &Path, integration: &str) -> Option<Vec<String>> {
    let output = crate::execution::worktree::git()
        .arg("-c")
        .arg("core.quotepath=false")
        .arg("-C")
        .arg(worktree)
        .arg("diff")
        .arg("--name-only")
        .arg(format!("{integration}...HEAD"))
        .output()
        .await
        .ok()?;
    if !output.status.success() {
        return None;
    }
    Some(
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(str::to_owned)
            .collect(),
    )
}

/// Reads, within the caps, the files the criteria name and then the files changed on the branch.
/// Paths that escape the worktree or do not exist are skipped.
pub(super) async fn gather_material(
    worktree: &Path,
    integration: &str,
    criteria: &[Criterion],
) -> ReviewMaterial {
    let mut cut = false;
    let mut wanted = paths_named_by(criteria);
    match changed_files(worktree, integration).await {
        Some(changed) => {
            for path in changed {
                if !wanted.contains(&path) {
                    wanted.push(path);
                }
            }
        }
        None => cut = true,
    }
    let mut files = Vec::new();
    let mut used = 0usize;
    for path in wanted {
        let Some(full) = audit::safe_relative_path(worktree, &path) else {
            continue;
        };
        let Ok(meta) = tokio::fs::metadata(&full).await else {
            continue;
        };
        if !meta.is_file() {
            continue;
        }
        if meta.len() > REVIEW_READ_LIMIT {
            cut = true;
            continue;
        }
        let Ok(bytes) = tokio::fs::read(&full).await else {
            continue;
        };
        if bytes.contains(&0) {
            cut = true;
            continue;
        }
        let remaining = REVIEW_TOTAL_CAP.saturating_sub(used);
        if remaining == 0 {
            cut = true;
            break;
        }
        let (text, file_cut) = numbered_prefix(
            &String::from_utf8_lossy(&bytes),
            REVIEW_FILE_CAP.min(remaining),
        );
        cut |= file_cut;
        used += text.len();
        files.push(ReviewFile { path, text });
    }
    ReviewMaterial { files, cut }
}

pub(super) fn review_prompt(
    spec: &Spec,
    criteria: &[Criterion],
    material: &ReviewMaterial,
) -> String {
    let mut prompt = format!(
        "You are an independent reviewer of the spec `{}` ({}). You have no tools and cannot read \
         the repository: judge only from the file contents below (lines are numbered), and answer \
         `doubt` (the unknown answer) for any criterion this material does not prove.\n",
        spec.path, spec.title
    );
    if material.cut {
        prompt.push_str(
            "\nNOT EVERYTHING IS SHOWN: a size cap, a binary or oversized file, or an unreadable \
             change list left material out. For every criterion you cannot see the proof of, answer \
             `doubt`; never assume what you were not shown.\n",
        );
    }
    prompt.push_str(
        "\nThe daemon runs the project's test suite separately, so judge only what the files \
         show. For a criterion that also asks that tests pass, answer from the static part: \
         the test exists, with its file:line as evidence. Never answer `doubt` merely because \
         a test run is not visible to you.\n",
    );
    prompt.push_str("\nCriteria:\n");
    for criterion in criteria {
        prompt.push_str(&format!("{}. {}", criterion.ordinal, criterion.text));
        if let Some(hint) = &criterion.verify_hint {
            prompt.push_str(&format!(" (verify: {hint})"));
        }
        prompt.push('\n');
    }
    prompt.push_str("\nFiles (changed on the feature branch, or named by a criterion):\n");
    if material.files.is_empty() {
        prompt.push_str("(none could be shown)\n");
    }
    for file in &material.files {
        prompt.push_str(&format!("\n=== {} ===\n{}", file.path, file.text));
    }
    prompt.push_str(
        "\nAnswer with one JSON array and nothing else, one object per criterion: \
         {\"ordinal\": <number>, \"status\": \"met\" | \"unmet\" | \"doubt\", \
         \"evidence\": \"<path>:<line>\" or null}. `met` needs evidence naming a file and line.\n",
    );
    prompt
}

/// Reads the reviewer's JSON array out of its text, tolerating a code fence around it.
fn parse_reviews(text: &str) -> Result<Vec<CriterionReview>, String> {
    let (Some(start), Some(end)) = (text.find('['), text.rfind(']')) else {
        return Err("the reviewer did not answer with a JSON array".to_owned());
    };
    if end < start {
        return Err("the reviewer did not answer with a JSON array".to_owned());
    }
    let items: Vec<serde_json::Value> = serde_json::from_str(&text[start..=end])
        .map_err(|error| format!("the reviewer's answer is not valid JSON: {error}"))?;
    Ok(items
        .iter()
        .filter_map(|item| {
            let ordinal = item.get("ordinal")?.as_i64()?;
            let status = item
                .get("status")
                .and_then(|status| status.as_str())
                .and_then(CriterionStatus::parse)
                .unwrap_or(CriterionStatus::Doubt);
            let evidence = item
                .get("evidence")
                .and_then(|e| e.as_str())
                .map(str::to_owned);
            Some(CriterionReview {
                ordinal,
                status,
                evidence,
            })
        })
        .collect())
}

/// The independent reviewer: a fresh no-tools run, so it cannot write, and `audit` refuses it if
/// it turns out to belong to the spec's own lineage.
pub(super) struct NoToolsReviewer<'a> {
    pub(super) state: &'a AppState,
    pub(super) cwd: &'a str,
    /// The checkout of the spec's feature branch, whose files are embedded in the prompt.
    pub(super) worktree: &'a Path,
    pub(super) integration_branch: &'a str,
}

#[async_trait::async_trait]
impl ReviewerRunner for NoToolsReviewer<'_> {
    async fn review(&self, spec: &Spec, criteria: &[Criterion]) -> Result<ReviewerAnswer, String> {
        let material = gather_material(self.worktree, self.integration_branch, criteria).await;
        let prompt = review_prompt(spec, criteria, &material);
        let request = no_tools_run_request(&prompt, spec.project_id.as_deref(), Some(self.cwd));
        let (run_id, text) = run_no_tools(self.state, request).await?;
        let reviews = parse_reviews(&text)?;
        let numeric: i64 = run_id.parse().map_err(|_| format!("bad run id {run_id}"))?;
        let root =
            sqlx::query_scalar::<_, Option<i64>>("SELECT lineage_root_id FROM runs WHERE id = ?")
                .bind(numeric)
                .fetch_optional(&self.state.pool)
                .await
                .map_err(|error| error.to_string())?
                .flatten();
        let lineage_root_id = Some(root.unwrap_or(numeric).to_string());
        Ok(ReviewerAnswer {
            run: ReviewerRun {
                run_id,
                lineage_root_id,
            },
            reviews,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spec_backlog::model::{CriterionKind, SpecKind, SpecStatus};

    fn spec() -> Spec {
        Spec {
            id: "spec-1".to_owned(),
            project_id: None,
            path: ".ai/specs/example.md".to_owned(),
            title: "Example".to_owned(),
            content_sha: "sha-1".to_owned(),
            kind: SpecKind::Spec,
            status: SpecStatus::Extracted,
            status_reason: None,
            tree_id: None,
            last_audited_sha: None,
            created_at: "2026-10-31T00:00:00+00:00".to_owned(),
            updated_at: "2026-10-31T00:00:00+00:00".to_owned(),
        }
    }

    #[test]
    fn the_prompt_rests_a_suite_criterion_on_the_daemons_run() {
        let criteria = [Criterion {
            id: 1,
            spec_id: "spec-1".to_owned(),
            ordinal: 1,
            text: "The test suite passes".to_owned(),
            kind: CriterionKind::Test,
            verify_hint: Some("python -m unittest discover -s tests -t .".to_owned()),
            status: CriterionStatus::Open,
            evidence: None,
            reviewer_run_id: None,
            spec_sha: "sha-1".to_owned(),
            checked_at: None,
        }];
        let material = ReviewMaterial {
            files: vec![ReviewFile {
                path: "tests/test_example.py".to_owned(),
                text: "1: def test_example():\n".to_owned(),
            }],
            cut: false,
        };

        let prompt = review_prompt(&spec(), &criteria, &material);

        assert!(prompt.contains("answer `met`"), "{prompt}");
        assert!(
            prompt.contains("refuses to finish the spec unless its own run is green"),
            "{prompt}"
        );
        assert!(prompt.contains("doubt"), "{prompt}");
    }
}
