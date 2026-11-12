//! The `renumber-sequence` factory step (spec 2026-11-09 passos de integracao §4): files that carry a
//! sequential number in their name (migrations, say) that a branch added with a number already taken
//! are renumbered after the target's highest one, and the references to their old names are
//! rewritten.
//!
//! It knows nothing about SQL. It runs in the integration worktree on the merged tree, between the
//! merge and the gate, and leaves its changes in the worktree for the engine (`steps.rs`) to commit.
//!
//! Idempotent: the candidates come from `base..merge` and so carry the names the branch gave them,
//! but a second run finds the worktree already renamed, so a candidate that no longer exists is
//! dropped and nothing is left to do.

use super::*;
use regex::Regex;
use std::collections::BTreeSet;
use std::ops::Range;

/// The step's `config`: where the numbered files live, how their names carry the number, and which
/// files may mention them.
#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RenumberConfig {
    /// Repo-relative directory holding the numbered files.
    pub dir: String,
    /// A file-name glob with exactly one `{N:<width>}`, e.g. `{N:4}_*.sql`.
    pub pattern: String,
    /// Repo-relative globs of the files searched for references to a renamed file's stem.
    #[serde(default)]
    pub references: Vec<String>,
}

impl RenumberConfig {
    pub(super) fn from_value(value: &serde_json::Value) -> Result<Self, String> {
        serde_json::from_value(value.clone())
            .map_err(|error| format!("invalid renumber-sequence config: {error}"))
    }
}

/// `{N:4}_*.sql` read once: a regex that matches a file name and captures the number.
pub(super) struct NumberPattern {
    regex: Regex,
    pub width: usize,
}

impl NumberPattern {
    pub(super) fn parse(pattern: &str) -> Result<Self, String> {
        let start = pattern
            .find("{N:")
            .ok_or_else(|| format!("pattern `{pattern}` has no `{{N:<width>}}` number"))?;
        let rest = &pattern[start + 3..];
        let close = rest
            .find('}')
            .ok_or_else(|| format!("pattern `{pattern}` has an unterminated `{{N:`"))?;
        let width: usize = rest[..close]
            .parse()
            .map_err(|_| format!("pattern `{pattern}`: the width after `N:` is not a number"))?;
        if width == 0 || width > 18 {
            return Err(format!("pattern `{pattern}`: the width must be 1 to 18"));
        }
        let before = &pattern[..start];
        let after = &rest[close + 1..];
        if after.contains("{N:") {
            return Err(format!("pattern `{pattern}` has more than one number"));
        }
        let source = format!(
            "^{}(?P<n>[0-9]{{{width}}}){}$",
            glob_regex(before),
            glob_regex(after)
        );
        let regex = Regex::new(&source).map_err(|error| format!("pattern `{pattern}`: {error}"))?;
        Ok(Self { regex, width })
    }

    /// The number a file name carries and where its digits sit in the name.
    pub(super) fn parts(&self, name: &str) -> Option<(u64, Range<usize>)> {
        let captured = self.regex.captures(name)?.name("n")?;
        let number = captured.as_str().parse().ok()?;
        Some((number, captured.range()))
    }
}

/// The regex source of a glob: `*` stays inside a path segment, `**` crosses segments (`**/` may
/// also match nothing), `?` is one character; everything else is literal.
fn glob_regex(glob: &str) -> String {
    let mut out = String::new();
    let mut chars = glob.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '*' => {
                let mut double = false;
                while chars.peek() == Some(&'*') {
                    chars.next();
                    double = true;
                }
                if !double {
                    out.push_str("[^/]*");
                } else if chars.peek() == Some(&'/') {
                    chars.next();
                    out.push_str("(?:.*/)?");
                } else {
                    out.push_str(".*");
                }
            }
            '?' => out.push_str("[^/]"),
            other => out.push_str(&regex::escape(&other.to_string())),
        }
    }
    out
}

/// Whether `text` (a `/`-separated path) matches `glob`.
pub(super) fn glob_match(glob: &str, text: &str) -> bool {
    Regex::new(&format!("^{}$", glob_regex(glob))).is_ok_and(|regex| regex.is_match(text))
}

/// Which candidates are renumbered, and to what. Empty when no candidate collides with an occupied
/// number or with another candidate; otherwise every candidate, by `(number, name)`, takes
/// `max(occupied) + 1`, `+ 2`, ... so the order the branch's author chose survives.
pub(super) fn plan_renumber(occupied: &[u64], candidates: &[(u64, String)]) -> Vec<(String, u64)> {
    let taken: BTreeSet<u64> = occupied.iter().copied().collect();
    let mut seen = BTreeSet::new();
    let collides = candidates
        .iter()
        .any(|(number, _)| taken.contains(number) || !seen.insert(*number));
    if !collides {
        return Vec::new();
    }
    let mut ordered: Vec<&(u64, String)> = candidates.iter().collect();
    ordered.sort();
    let first = occupied.iter().copied().max().unwrap_or(0) + 1;
    ordered
        .into_iter()
        .zip(first..)
        .map(|((_, name), number)| (name.clone(), number))
        .collect()
}

fn is_word(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

/// True when `text[start..end]` stands alone: no word character touches either side.
fn stands_alone(text: &str, start: usize, end: usize) -> bool {
    let before = text[..start].chars().next_back();
    let after = text[end..].chars().next();
    !before.is_some_and(is_word) && !after.is_some_and(is_word)
}

/// Replaces each old stem by its new one in one pass (so a result is never rescanned), only where
/// no word character touches the match: `0042_add` inside `0042_add_policy` is left alone.
/// Returns the new text and how many replacements were made.
pub(super) fn rewrite_stems(text: &str, stems: &[(String, String)]) -> (String, usize) {
    if stems.is_empty() {
        return (text.to_owned(), 0);
    }
    let mut longest_first: Vec<&(String, String)> = stems.iter().collect();
    longest_first.sort_by_key(|(old, _)| std::cmp::Reverse(old.len()));
    let alternatives: Vec<String> = longest_first
        .iter()
        .map(|(old, _)| regex::escape(old))
        .collect();
    let Ok(regex) = Regex::new(&alternatives.join("|")) else {
        return (text.to_owned(), 0);
    };
    let mut out = String::with_capacity(text.len());
    let mut last = 0;
    let mut count = 0;
    for found in regex.find_iter(text) {
        if !stands_alone(text, found.start(), found.end()) {
            continue;
        }
        let Some((_, new)) = stems.iter().find(|(old, _)| old == found.as_str()) else {
            continue;
        };
        out.push_str(&text[last..found.start()]);
        out.push_str(new);
        last = found.end();
        count += 1;
    }
    out.push_str(&text[last..]);
    (out, count)
}

/// The numbers (as written) that still appear on their own in `text`.
fn bare_numbers<'a>(text: &str, numbers: &'a [String]) -> Vec<&'a str> {
    numbers
        .iter()
        .filter(|number| {
            text.match_indices(number.as_str())
                .any(|(at, found)| stands_alone(text, at, at + found.len()))
        })
        .map(String::as_str)
        .collect()
}

/// The file name without its last extension.
fn stem_of(name: &str) -> &str {
    match name.rfind('.') {
        Some(dot) if dot > 0 => &name[..dot],
        _ => name,
    }
}

/// One git command whose failure is a plain message, not a queue outcome.
async fn git_text(repo: &Path, args: &[&str], budget: Duration) -> Result<String, String> {
    let deadline = std::time::Instant::now() + budget;
    match git(repo, args, deadline).await {
        Ok(done) if done.succeeded() => Ok(done.stdout),
        Ok(done) => Err(format!(
            "git {} failed: {}",
            args.join(" "),
            done.output_tail
        )),
        Err(Outcome::Failed { reason, .. }) => Err(reason),
        Err(_) => Err(format!("git {} could not run", args.join(" "))),
    }
}

fn paths_of(listing: &str) -> impl Iterator<Item = &str> {
    listing.split('\0').filter(|path| !path.is_empty())
}

/// Runs the step on the integration worktree. Returns what it did, one line per action.
pub(super) async fn run(
    run: &StepRun<'_>,
    config: &serde_json::Value,
    git_timeout: Duration,
) -> Result<String, String> {
    let config = RenumberConfig::from_value(config)?;
    let pattern = NumberPattern::parse(&config.pattern)?;
    let dir = config.dir.replace('\\', "/").trim_matches('/').to_owned();
    if dir.is_empty() || dir.split('/').any(|part| part == "..") {
        return Err(format!(
            "dir `{}` must be a path inside the repository",
            config.dir
        ));
    }
    let prefix = format!("{dir}/");
    let repo = run.integration;

    // The numbers the target already uses.
    let listed = git_text(
        repo,
        &["ls-tree", "--name-only", "-z", run.base, "--", &prefix],
        git_timeout,
    )
    .await?;
    let occupied: Vec<u64> = paths_of(&listed)
        .filter_map(|path| path.strip_prefix(&prefix))
        .filter_map(|name| pattern.parts(name))
        .map(|(number, _)| number)
        .collect();

    // The files the branch added, under the names it gave them; only those still in the worktree,
    // which is what makes a second run a no-op.
    let added = git_text(
        repo,
        &[
            "diff",
            "--name-only",
            "-z",
            "--diff-filter=A",
            "--no-renames",
            run.base,
            run.merge,
            "--",
            &dir,
        ],
        git_timeout,
    )
    .await?;
    let candidates: Vec<(u64, String)> = paths_of(&added)
        .filter_map(|path| path.strip_prefix(&prefix))
        .filter(|name| !name.contains('/'))
        .filter_map(|name| Some((pattern.parts(name)?.0, name.to_owned())))
        .filter(|(_, name)| repo.join(&dir).join(name).is_file())
        .collect();

    let plan = plan_renumber(&occupied, &candidates);
    if plan.is_empty() {
        return Ok("no numbering collision; nothing renumbered".to_owned());
    }

    // (old name, new name) for the files that actually move.
    let mut renames: Vec<(String, String)> = Vec::new();
    // (old number as written) for the warning about bare mentions.
    let mut old_numbers: Vec<String> = Vec::new();
    for (old, number) in &plan {
        let Some((_, digits)) = pattern.parts(old) else {
            continue;
        };
        let padded = format!("{number:0width$}", width = pattern.width);
        if padded.len() > pattern.width {
            return Err(format!(
                "number {number} does not fit the {} digits of the pattern",
                pattern.width
            ));
        }
        let new = format!("{}{padded}{}", &old[..digits.start], &old[digits.end..]);
        if &new != old {
            old_numbers.push(old[digits].to_owned());
            renames.push((old.clone(), new));
        }
    }

    // Two phases through temporary names, so a new name equal to another file's old name cannot
    // collide.
    let mut lines = Vec::new();
    for (index, (old, _)) in renames.iter().enumerate() {
        let from = format!("{prefix}{old}");
        let temp = format!("{prefix}.renumber-{index}.tmp");
        git_text(repo, &["mv", "--", &from, &temp], git_timeout).await?;
    }
    for (index, (old, new)) in renames.iter().enumerate() {
        let temp = format!("{prefix}.renumber-{index}.tmp");
        let to = format!("{prefix}{new}");
        git_text(repo, &["mv", "--", &temp, &to], git_timeout).await?;
        lines.push(format!("renamed {prefix}{old} -> {to}"));
    }

    // References: files the branch added or changed, under the name they have now.
    let stems: Vec<(String, String)> = renames
        .iter()
        .map(|(old, new)| (stem_of(old).to_owned(), stem_of(new).to_owned()))
        .collect();
    old_numbers.sort();
    old_numbers.dedup();
    let touched = git_text(
        repo,
        &[
            "diff",
            "--name-only",
            "-z",
            "--diff-filter=AM",
            "--no-renames",
            run.base,
            run.merge,
        ],
        git_timeout,
    )
    .await?;
    for path in paths_of(&touched) {
        if !config.references.iter().any(|glob| glob_match(glob, path)) {
            continue;
        }
        let current = match path
            .strip_prefix(&prefix)
            .and_then(|name| renames.iter().find(|(old, _)| old == name))
        {
            Some((_, new)) => format!("{prefix}{new}"),
            None => path.to_owned(),
        };
        let file = repo.join(&current);
        let Ok(bytes) = std::fs::read(&file) else {
            continue;
        };
        let Ok(text) = String::from_utf8(bytes) else {
            continue;
        };
        let (rewritten, count) = rewrite_stems(&text, &stems);
        if count > 0 {
            std::fs::write(&file, &rewritten)
                .map_err(|error| format!("could not write {current}: {error}"))?;
            lines.push(format!("rewrote {count} reference(s) in {current}"));
        }
        for number in bare_numbers(&rewritten, &old_numbers) {
            lines.push(format!(
                "warning: {current} mentions {number} on its own; not rewritten"
            ));
        }
    }
    Ok(lines.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::execution::git_exec::testkit::init_contained_repo;
    use std::path::PathBuf;
    use std::process::Command;

    const GIT_TIMEOUT: Duration = Duration::from_secs(60);

    fn sh(repo: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(args)
            .output()
            .expect("git should start");
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout)
            .expect("utf-8")
            .trim()
            .to_owned()
    }

    fn write_all(repo: &Path, files: &[(&str, &str)]) {
        for (path, content) in files {
            let full = repo.join(path);
            std::fs::create_dir_all(full.parent().expect("parent")).expect("mkdir");
            std::fs::write(full, content).expect("write");
        }
    }

    /// `master` with `db/0001_a.sql`, `db/0002_b.sql`, and `src/old.rs` mentioning `0002_x`.
    fn base_repo() -> (tempfile::TempDir, PathBuf) {
        let (container, repo) = init_contained_repo("nucleos-renumber-");
        sh(&repo, &["branch", "-M", "master"]);
        write_all(
            &repo,
            &[
                ("db/0001_a.sql", "-- a\n"),
                ("db/0002_b.sql", "-- b\n"),
                ("src/old.rs", "// 0002_x\n"),
            ],
        );
        sh(&repo, &["add", "-A"]);
        sh(&repo, &["commit", "-m", "base"]);
        (container, repo)
    }

    /// A branch off `master` with `files` written, and back on `master`.
    fn branch(repo: &Path, name: &str, files: &[(&str, &str)]) {
        sh(repo, &["checkout", "-b", name, "master"]);
        write_all(repo, files);
        sh(repo, &["add", "-A"]);
        sh(repo, &["commit", "-m", name]);
        sh(repo, &["checkout", "master"]);
    }

    /// Merges `name` into `master` (a real merge commit); returns `(base, merge)`.
    fn land(repo: &Path, name: &str) -> (String, String) {
        let base = sh(repo, &["rev-parse", "HEAD"]);
        sh(repo, &["merge", "--no-ff", "-m", "land", name]);
        (base, sh(repo, &["rev-parse", "HEAD"]))
    }

    const CONFIG: &str =
        r#"{"dir":"db","pattern":"{N:4}_*.sql","references":["src/**/*.rs","db/*.sql"]}"#;

    async fn renumber(repo: &Path, base: &str, merge: &str) -> Result<String, String> {
        let step = StepRun {
            integration: repo,
            base,
            merge,
            source: "feat/x",
            target: "master",
        };
        let config: serde_json::Value = serde_json::from_str(CONFIG).expect("json");
        super::run(&step, &config, GIT_TIMEOUT).await
    }

    fn text(repo: &Path, path: &str) -> String {
        std::fs::read_to_string(repo.join(path)).expect("read")
    }

    #[test]
    fn the_glob_matches_double_star_and_single_star() {
        assert!(glob_match("core/src/**/*.rs", "core/src/a.rs"));
        assert!(glob_match("core/src/**/*.rs", "core/src/x/y/a.rs"));
        assert!(!glob_match("core/src/**/*.rs", "core/src/a.sql"));
        assert!(glob_match(
            "core/migrations/*.sql",
            "core/migrations/0001_a.sql"
        ));
        assert!(!glob_match(
            "core/migrations/*.sql",
            "core/migrations/sub/0001_a.sql"
        ));
        assert!(glob_match("a/?.rs", "a/b.rs"));
        assert!(!glob_match("a/?.rs", "a/bc.rs"));
        assert!(glob_match("a/**", "a/b/c.txt"));
        assert!(!glob_match("a.rs", "axrs"));
    }

    #[test]
    fn the_pattern_reads_the_number_and_its_width() {
        let pattern = NumberPattern::parse("{N:4}_*.sql").expect("pattern");
        assert_eq!(pattern.width, 4);
        assert_eq!(pattern.parts("0042_add.sql"), Some((42, 0..4)));
        assert_eq!(pattern.parts("42_add.sql"), None);
        assert_eq!(pattern.parts("0042_add.txt"), None);
        let prefixed = NumberPattern::parse("v{N:2}-*.md").expect("pattern");
        assert_eq!(prefixed.parts("v07-notes.md"), Some((7, 1..3)));
    }

    #[test]
    fn a_pattern_without_a_number_is_refused() {
        assert!(NumberPattern::parse("*.sql").is_err());
        assert!(NumberPattern::parse("{N:x}_*.sql").is_err());
        assert!(NumberPattern::parse("{N:2}_{N:2}.sql").is_err());
        assert!(RenumberConfig::from_value(&serde_json::json!({"dir": "db"})).is_err());
    }

    #[test]
    fn no_collision_renumbers_nothing() {
        let candidates = [(3, "0003_a.sql".to_owned()), (4, "0004_b.sql".to_owned())];
        assert!(plan_renumber(&[1, 2], &candidates).is_empty());
    }

    #[test]
    fn two_candidates_with_one_number_are_renumbered() {
        let candidates = [(5, "0005_b.sql".to_owned()), (5, "0005_a.sql".to_owned())];
        assert_eq!(
            plan_renumber(&[1, 2], &candidates),
            vec![("0005_a.sql".to_owned(), 3), ("0005_b.sql".to_owned(), 4)]
        );
    }

    #[tokio::test]
    async fn a_collision_with_the_base_renumbers_the_whole_block_in_order() {
        let (_c, repo) = base_repo();
        branch(
            &repo,
            "feat/x",
            &[("db/0002_x.sql", "-- x\n"), ("db/0003_y.sql", "-- y\n")],
        );
        let (base, merge) = land(&repo, "feat/x");
        let output = renumber(&repo, &base, &merge).await.expect("runs");

        assert!(
            output.contains("renamed db/0002_x.sql -> db/0003_x.sql"),
            "{output}"
        );
        assert!(
            output.contains("renamed db/0003_y.sql -> db/0004_y.sql"),
            "{output}"
        );
        assert!(repo.join("db/0003_x.sql").is_file());
        assert!(repo.join("db/0004_y.sql").is_file());
        assert!(!repo.join("db/0002_x.sql").exists());
        assert!(!repo.join("db/0003_y.sql").exists());
        assert!(
            repo.join("db/0002_b.sql").is_file(),
            "the base's own file stays"
        );
    }

    #[tokio::test]
    async fn files_already_in_the_base_are_never_renumbered() {
        let (_c, repo) = base_repo();
        branch(
            &repo,
            "feat/x",
            &[
                ("db/0002_b.sql", "-- b edited\n"),
                ("db/0002_x.sql", "-- x\n"),
            ],
        );
        let (base, merge) = land(&repo, "feat/x");
        renumber(&repo, &base, &merge).await.expect("runs");

        assert_eq!(text(&repo, "db/0002_b.sql"), "-- b edited\n");
        assert!(repo.join("db/0003_x.sql").is_file());
        assert!(!repo.join("db/0002_x.sql").exists());
    }

    #[tokio::test]
    async fn the_references_to_the_full_stem_are_rewritten_in_changed_files_only() {
        let (_c, repo) = base_repo();
        branch(
            &repo,
            "feat/x",
            &[
                ("db/0002_x.sql", "-- x\n"),
                ("db/0003_ref.sql", "-- depends on 0002_x\n"),
                ("src/uses.rs", "const A: &str = \"0002_x\";\n"),
            ],
        );
        let (base, merge) = land(&repo, "feat/x");
        let output = renumber(&repo, &base, &merge).await.expect("runs");

        assert_eq!(text(&repo, "src/uses.rs"), "const A: &str = \"0003_x\";\n");
        // The reference file is itself a candidate: it is rewritten under its new name.
        assert_eq!(text(&repo, "db/0004_ref.sql"), "-- depends on 0003_x\n");
        assert!(
            output.contains("rewrote 1 reference(s) in src/uses.rs"),
            "{output}"
        );
        // A file the branch did not touch is never read.
        assert_eq!(text(&repo, "src/old.rs"), "// 0002_x\n");
    }

    #[tokio::test]
    async fn a_stem_that_is_a_prefix_of_another_is_left_alone() {
        let (_c, repo) = base_repo();
        branch(
            &repo,
            "feat/x",
            &[
                ("db/0002_add.sql", "-- add\n"),
                ("src/uses.rs", "0002_add_policy 0002_add 0002_add2\n"),
            ],
        );
        let (base, merge) = land(&repo, "feat/x");
        renumber(&repo, &base, &merge).await.expect("runs");

        assert_eq!(
            text(&repo, "src/uses.rs"),
            "0002_add_policy 0003_add 0002_add2\n"
        );
    }

    #[tokio::test]
    async fn a_bare_number_is_warned_about_and_not_rewritten() {
        let (_c, repo) = base_repo();
        branch(
            &repo,
            "feat/x",
            &[
                ("db/0002_x.sql", "-- x\n"),
                (
                    "src/uses.rs",
                    "// see migration 0002 for details\nconst A: &str = \"0002_x\";\n",
                ),
            ],
        );
        let (base, merge) = land(&repo, "feat/x");
        let output = renumber(&repo, &base, &merge).await.expect("runs");

        assert!(
            output.contains("warning: src/uses.rs mentions 0002 on its own; not rewritten"),
            "{output}"
        );
        let content = text(&repo, "src/uses.rs");
        assert!(content.contains("migration 0002 for details"), "{content}");
        assert!(content.contains("\"0003_x\""), "{content}");
    }

    #[tokio::test]
    async fn the_second_run_changes_nothing() {
        let (_c, repo) = base_repo();
        branch(
            &repo,
            "feat/x",
            &[
                ("db/0002_x.sql", "-- x\n"),
                ("src/uses.rs", "const A: &str = \"0002_x\";\n"),
            ],
        );
        let (base, merge) = land(&repo, "feat/x");
        let first = renumber(&repo, &base, &merge).await.expect("first run");
        assert!(first.contains("renamed"), "{first}");
        let status = sh(&repo, &["status", "--porcelain"]);

        let second = renumber(&repo, &base, &merge).await.expect("second run");
        assert!(!second.contains("renamed"), "{second}");
        assert_eq!(sh(&repo, &["status", "--porcelain"]), status);
    }

    #[tokio::test]
    async fn chained_branches_get_consecutive_numbers() {
        let (_c, repo) = base_repo();
        branch(&repo, "feat/a", &[("db/0003_a.sql", "-- a\n")]);
        branch(&repo, "feat/b", &[("db/0003_b.sql", "-- b\n")]);

        let (base, merge) = land(&repo, "feat/a");
        renumber(&repo, &base, &merge).await.expect("first land");
        assert!(repo.join("db/0003_a.sql").is_file());

        let (base, merge) = land(&repo, "feat/b");
        renumber(&repo, &base, &merge).await.expect("second land");
        assert!(repo.join("db/0003_a.sql").is_file());
        assert!(repo.join("db/0004_b.sql").is_file());
        assert!(!repo.join("db/0003_b.sql").exists());
    }
}
