//! §spec mapa-do-projeto
//!
//! Whether a decision has no code, or lost the comment that said it had.
//!
//! §5.1's *declarado, sem código* pile is the product of this map, and every row in it rests on the
//! absence of a `§N` in a comment. There is nothing under that — no static analysis, no naming
//! convention, no heuristic. Delete the comment and the anchor stops existing, and the decision
//! joins the pile indistinguishable from one nobody ever implemented.
//!
//! The failure mode is not somebody deleting it on purpose. It is a thousand-line plan accepted
//! without being read, in which a model rewrites a module and drops the comment on the way — which
//! is §1 of the design document happening **to the instrument built against §1**.
//!
//! **Half of that hole is already covered and this module is the other half.** A stamp keeps the
//! digest of the anchor files, and `map_stamp`'s `moved` compares it against the present: a file
//! that loses its citation leaves the anchor set and arrives in
//! [`crate::map_stamp::Lapse::Moved`]'s `gone`, by name. A decision **nobody ever stamped** has no
//! record of before at all — the junction is derived state, recomputed on every read, with no
//! memory — and those are the majority on day one (§10).
//!
//! **On demand, per decision, and never in the background.** The question is not *did any file lose
//! a citation?* in the abstract; it is *was this never built, or did it lose the comment?*, and it
//! only means anything at the instant somebody is looking at one row. So there is no table here, no
//! migration and no periodic work. The alternative — storing each read's anchor set to diff against
//! the next — costs permanent state, fires on every legitimate removal, and still cannot tell
//! *deliberate* from *accidental*, which is the only distinction that matters and the only one a
//! person can make.
//!
//! **Two passes, and the second is what makes it honest.** Git's pickaxe is a cheap filter over the
//! history; the confirmation reads the actual blobs and runs them through
//! [`crate::map_join::citations`] and [`crate::map_join::evidence`] — the same two functions that
//! answer about the present. A regex written here to read the past would answer a slightly
//! different question from the one the junction answers, and the two would drift apart the first
//! time the reader changed. That rule is not a stylistic preference: the sweep that was supposed to
//! justify this module was written with a private regex, reported nine losses, and every one of
//! them was an artefact — a `.` left as a wildcard, `§3` swallowing `§3.5`, `§6.1` swallowing
//! `§6.1a`, and an addition read as a removal. The conclusion sat in the design document as fact
//! until somebody asked for a review. Each of those four defects is a test below.
//!
//! **It answers a narrow question and never the big one.** *This text left this file, in this
//! commit* is a fact about git. *This was implemented* is not, and the parser cannot even tell the
//! two apart: [`crate::map_join::citations`] reads `§7.1` in *this implements §7.1* and in *as §7.1
//! explains* exactly alike, which is why [`crate::map_join::Anchor::Declared`] says **names** and
//! never **implements**. This hands over a fact and a hash; the owner judges, as §5 requires of
//! everything else here.

use crate::git_exec::run_git;
use crate::map_join::{Citation, Evidence, citations, evidence};
use crate::project_map::scanned;
use serde::Serialize;
use std::collections::BTreeSet;
use std::ffi::{OsStr, OsString};
use std::path::Path;
use std::time::Duration;

/// How far back the history is searched.
///
/// **Five times [`crate::map_recency::WINDOW`], and the two windows are deliberately different
/// sizes because they answer different questions.** Recency asks *what moved since I last looked*,
/// whose honest horizon is days — two hundred commits is five and a half of them on this
/// repository. This asks *when did this citation disappear*, and the answer is allowed to be months
/// old: a decision extracted from a document written in June is still worth an answer in September.
/// A window sized for the first question would report [`Orphan::NotInWindow`] for almost every real
/// loss, which is a sentence that says nothing.
///
/// **Measured on this repository on 2026-08-27, warm cache, through `git log -S`:** 200 commits
/// 1.11 s, 400 commits 1.95 s, the whole 1 099-commit history 2.31 s — against 0.18 s for the same
/// walk with no pickaxe, so the diffing is the entire bill at roughly 2 ms per commit. One thousand
/// is therefore ~2 s here, and the ceiling exists so that a repository with a hundred thousand
/// commits does not spend three minutes on one click.
///
/// **It does NOT reach the beginning of this repository, and the consequence is worth stating
/// rather than discovering.** There are 1 099 commits here and the window is 1 000, so a search
/// that finds nothing comes back [`Orphan::NotInWindow`] and never [`Orphan::NeverNamed`] — and as
/// the history grows the gap only widens. That is the correct answer and not a shortfall: 99
/// commits went unread, so *nothing ever named this* is a claim the search did not earn. What it
/// costs is real all the same — the definite answer, the one that says *this decision genuinely
/// has no code*, is available only on a project younger than the window. Raising the constant
/// until it happened to cover this repository would be fitting it to one tree on one day, at 35
/// commits a day; the honest fix, if the sentence on screen ever stops being good enough, is to
/// widen the window with an argument about cost rather than about this checkout's size.
///
/// **A pathspec was measured and refused.** Restricting the pickaxe to the extensions [`scanned`]
/// admits is the obvious saving and it is not one: 2.58 s against 2.19 s over the full history,
/// because matching the pathspec costs more than the diffs it skips. It would also invite git's
/// history simplification, which prunes commits when a path limit is in force — the risk §14.10 of
/// the design document listed as unmeasured. With no pathspec there is no path-based simplification
/// to worry about, and `--full-history` was confirmed to change nothing: identical commit lists
/// over seven sections and 181 flagged commits.
pub const WINDOW: usize = 1_000;

/// How long the pickaxe is given before the answer becomes *I could not look*.
///
/// **A bound on a hang and not on a slow answer**, the same argument `map_stamp`'s
/// `LS_FILES_TIMEOUT` makes, and the number is larger for a measured reason. The walk costs ~2 s at
/// [`WINDOW`] on this repository warm, and [`crate::map_recency::WINDOW`] records that the first git
/// call after a rebuild on this machine ran **5×** slower — cold object cache meeting a virus
/// scanner. A ten-second ceiling would therefore fire on the first click after a build and report
/// [`Orphan::Unreadable`] about a repository that was merely cold. Twenty seconds clears that with
/// room and is still short enough that whoever clicked gets a sentence rather than a window that
/// never finishes.
const PICKAXE_TIMEOUT: Duration = Duration::from_secs(20);

/// How long one blob read or one working-tree search is given.
///
/// Five seconds, `map_stamp`'s ceiling for the same shape of call. Ten `git show` invocations of
/// this repository's largest module measured 0.71 s in total, so this is ~70× the cost of the call
/// it bounds; what it is really guarding is a git that has stopped answering, and by then the
/// pickaxe's own deadline is the one doing the work.
const READ_TIMEOUT: Duration = Duration::from_secs(5);

/// What marks a commit's line in the pickaxe's output.
///
/// The same U+001E `map_recency`'s `RECORD_MARK` uses and for the same reason: git C-quotes a
/// control character inside a path even with `core.quotepath=false`, so such a path arrives
/// starting with a quote and a header is the only line that can begin with this byte. Without it a
/// file called `1787757225` at the repository root is indistinguishable from a commit header.
const RECORD_MARK: char = '\u{1e}';

/// What separates the fields inside one header.
///
/// U+001F, and the subject goes **last** so the split can be bounded: a subject is one line by
/// construction (`%s`), but nothing stops it containing this byte, and taking the remainder as the
/// subject means such a commit is reported with a strange title rather than dropped.
const FIELD_MARK: char = '\u{1f}';

/// Whether a decision's section is genuinely unimplemented, or lost the comment that anchored it.
///
/// **[`Orphan::NotInWindow`] exists for the reason everything in this feature exists.** *I did not
/// find it* and *I did not search all of it* are different sentences, and collapsing the second into
/// [`Orphan::NeverNamed`] would assert *this was never built* about a history nobody read to the
/// end. A visibly approximate answer is acceptable; a silently wrong one is not.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Orphan {
    /// No commit in the window ever named this section, and nothing names it now. The decision has
    /// no code, and that is the truth.
    NeverNamed,
    /// Something names it today, so it is not an orphan and the question should not have been
    /// asked.
    ///
    /// Carries the files, because *you are looking at the wrong row* is only useful to somebody who
    /// can go and look at the right one. Reached before the pickaxe runs — which is also what keeps
    /// the cost of this module proportional to how orphaned a section actually is: a section forty
    /// files name today is answered by one `git grep` and never diffs a commit.
    StillNamed { paths: Vec<String> },
    /// Files carried this citation and stopped carrying it.
    Lost { losses: Vec<Loss> },
    /// The window ran out before the beginning of the repository.
    ///
    /// Carries the window so the sentence on screen can name it, exactly as `map_recency`'s
    /// `Recency` carries its own.
    NotInWindow { window: usize },
    /// Git is there and would not answer.
    ///
    /// **A repository with no commits arrives here, and that is the right direction rather than an
    /// oversight.** There is no history to search, so [`Orphan::NeverNamed`] — which asserts *this
    /// was never built* — would be the report of a search that never ran. It is
    /// [`crate::map_join::Anchor::Unnumbered`]'s argument one layer down: *nothing claims this* is
    /// the result of looking, and where nothing was looked at, the only honest word is that nothing
    /// was looked at.
    Unreadable,
    /// There is no repository.
    ///
    /// Distinct from [`Orphan::Unreadable`] for the reason [`crate::map_stamp::Watch::NoRepository`]
    /// is distinct from [`crate::map_stamp::Lapse::Unreadable`] (§11): the only advice `Unreadable`
    /// carries is *try again*, and a project added from outside a repository will never succeed at
    /// trying again.
    NoRepository,
}

/// One file that carried the citation and stopped.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Loss {
    /// The path as it was **before** the commit — the file that was carrying the citation.
    pub path: String,
    /// The full object id, not an abbreviation. Whoever reads this sentence is going to paste it
    /// into `git show`, and an abbreviation is a hash that stops working when the repository grows.
    pub commit: String,
    /// The committer date, in Unix seconds. Committer and not author, for
    /// [`crate::map_recency::Age::Moved`]'s reason: the question is when this arrived in the branch
    /// somebody is reading, not when it was first typed.
    pub at: i64,
    pub subject: String,
    /// Where the file went, when that commit renamed it.
    pub renamed_to: Option<String>,
    /// Whether the citation that left **named this decision's document**.
    ///
    /// `false` for every citation in this repository today, because §8 is unfixed and not one `§`
    /// here carries a slug — so today this field is honestly reporting *a file named this section
    /// number, under some document, and this is a guess about which*. That is the same ambiguity
    /// [`crate::map_join::Anchor::Ambiguous`] reports about the present, said in the same words, and
    /// the guard does not get to be more certain about the past than the junction is about the now.
    pub declared: bool,
}

/// Whether anything names this section in the working tree right now, or why that is unknown.
enum Present {
    /// The files that do, confirmed by the parser. Empty means nothing does.
    Named(Vec<String>),
    Unreadable,
    NoRepository,
}

/// One commit the pickaxe flagged.
#[derive(Clone)]
struct Flagged {
    commit: String,
    at: i64,
    /// **Every parent, and the loop over them is not dead code even though it has exactly one
    /// iteration on this repository.** `git log -S` runs the pickaxe over a diff, and a merge has
    /// none by default, so a merge is never flagged: measured on 2026-08-27 over seven sections and
    /// 181 flagged commits here, of which zero were merges. The sweep this module replaces got that
    /// backwards — it believed most flagged commits were merges and that `<commit>^` was therefore
    /// reading the wrong side — and the belief was an artefact of its own broken regex. The loop
    /// stays because `log.diffMerges` is a configuration setting somebody's machine may carry, and
    /// under it a merge can be flagged with no side-branch commit to find the removal in. Reading
    /// only the first parent there would report a loss that never happened, or miss one that did.
    parents: Vec<String>,
    subject: String,
}

/// What one commit did to one path.
struct Change {
    /// The path as it was before, or `None` when the commit created it. **A change with no before
    /// can never be a loss**, which is where the third of the four defects dies structurally rather
    /// than by a rule somebody has to remember: the pickaxe flags any change in the count of the
    /// text, in both directions, and the sweep that read `before=0 after=2` as a removal is exactly
    /// this case.
    before: Option<String>,
    /// The path as it is after, or `None` when the commit deleted it.
    after: Option<String>,
    renamed_to: Option<String>,
}

/// What one blob read found.
enum Blob {
    Read(String),
    /// That path does not exist at that revision. Git answered, and the answer was no.
    Absent,
    /// Git could not be asked. Never confused with [`Blob::Absent`], because a git that will not run
    /// would otherwise turn every path into *there was nothing there* and every loss into silence.
    Failed,
}

/// Whether a decision's section was ever named by a file that no longer names it.
///
/// `section` is the number a citation is written as — `6.4`, `5.3a` — and not the heading the
/// decision was copied from. [`crate::map_join::section_number`] is what turns one into the other,
/// and it stays at the route so that this and the junction cannot come to disagree about what the
/// number of a heading is.
pub async fn orphan(root: &Path, section: &str, spec_slug: &str, spec_slugs: &[String]) -> Orphan {
    orphan_within(root, section, spec_slug, spec_slugs, WINDOW).await
}

/// The guard, with the window as a parameter so the tests can have one small enough to fall outside
/// of.
///
/// Private, and [`orphan`] is the only thing that names [`WINDOW`], following `map_recency`'s
/// `walk_within` so that the size of the window and the size reported in [`Orphan::NotInWindow`]
/// cannot come apart.
async fn orphan_within(
    root: &Path,
    section: &str,
    spec_slug: &str,
    spec_slugs: &[String],
    window: usize,
) -> Orphan {
    // The one string every pass searches for. Built once, and never turned into a pattern: `-S`
    // takes a literal, so there is nothing to escape and `--pickaxe-regex` is never passed. The
    // first of the four defects — an escape that replaced a dot with a dot, leaving `§5.1` matching
    // `§5x1` — cannot be written here, because there is no regex to get wrong.
    let needle = format!("§{section}");

    match named_today(root, &needle, section, spec_slug, spec_slugs).await {
        Present::Named(paths) if !paths.is_empty() => return Orphan::StillNamed { paths },
        Present::Named(_) => {}
        Present::Unreadable => return Orphan::Unreadable,
        Present::NoRepository => return Orphan::NoRepository,
    }

    let boundary = match boundary(root, window).await {
        Ok(edge) => edge,
        Err(answer) => return answer,
    };

    let flagged = match pickaxe(root, &needle, boundary.as_deref()).await {
        Ok(found) => found,
        Err(answer) => return answer,
    };

    let mut losses = Vec::new();
    // The paths a loss has already been recorded for. Git answers newest first, so the first
    // confirmed transition for a path is the most recent one — *where it stopped* — and the older
    // commits that also touched it have nothing left to add.
    let mut answered: BTreeSet<String> = BTreeSet::new();

    for (commit, change) in flagged {
        let Some(before_path) = change.before else {
            continue;
        };
        // A path outside the map's own universe is not a loss this map may report. The rule is
        // `project_map`'s, borrowed rather than restated, for the same reason the parser is.
        if !scanned(&before_path) || answered.contains(&before_path) {
            continue;
        }

        // Did the citation survive this commit? A rename whose destination still names the section
        // dies here, which is §14.6's mitigation and not a case of its own.
        if let Some(after_path) = change.after.as_deref() {
            match names(
                root,
                &commit.commit,
                after_path,
                section,
                spec_slug,
                spec_slugs,
            )
            .await
            {
                Ok(Some(_)) => continue,
                Ok(None) => {}
                Err(()) => return Orphan::Unreadable,
            }
        }

        // Was it there before? Reported as *declared* only if some parent had it under this
        // decision's own document, which is [`Evidence`]'s ordering and not a second opinion about
        // it.
        let mut carried: Option<bool> = None;
        for parent in &commit.parents {
            match names(root, parent, &before_path, section, spec_slug, spec_slugs).await {
                Ok(Some(Evidence::Declared)) => carried = Some(true),
                Ok(Some(_)) => carried = Some(carried.unwrap_or(false)),
                Ok(None) => {}
                Err(()) => return Orphan::Unreadable,
            }
        }
        let Some(declared) = carried else {
            continue;
        };

        answered.insert(before_path.clone());
        losses.push(Loss {
            path: before_path,
            commit: commit.commit,
            at: commit.at,
            subject: commit.subject,
            renamed_to: change.renamed_to,
            declared,
        });
    }

    if !losses.is_empty() {
        return Orphan::Lost { losses };
    }
    // Nothing found, and whether that is an answer depends entirely on whether the search reached
    // the beginning. A boundary exists only when the history is deeper than the window.
    if boundary.is_some() {
        Orphan::NotInWindow { window }
    } else {
        Orphan::NeverNamed
    }
}

/// The files naming this section in the working tree right now.
///
/// **`git grep` is a filter and the parser is the answer**, the same two-pass shape the history gets
/// and for the same reason: `-F '§6.1'` matches inside `§6.10` and `§6.1a`, and only [`citations`]
/// knows that the second is a different section. Asking git for a regex instead would be a third
/// spelling of *what is a citation*.
///
/// **`--untracked`, because the present is the working tree.** A file somebody has just written and
/// not yet committed names the section as much as a committed one does, and missing it would report
/// a section as orphaned on the very day it was implemented. Ignored files stay out — git's standard
/// excludes still apply — which keeps `node_modules` and build output from being searched, exactly
/// as [`crate::project_map::structure`]'s own walk skips them.
async fn named_today(
    root: &Path,
    needle: &str,
    section: &str,
    spec_slug: &str,
    spec_slugs: &[String],
) -> Present {
    let argv: Vec<OsString> = ["grep", "--untracked", "-I", "-l", "-z", "-F", "-e", needle]
        .iter()
        .map(OsString::from)
        .collect();
    let borrowed: Vec<&OsStr> = argv.iter().map(OsString::as_os_str).collect();

    let answer = match run_git(root, &borrowed, READ_TIMEOUT).await {
        // Exit 1 is `git grep`'s way of saying nothing matched, which is an answer and not a
        // failure — the one place in this module where a non-zero exit is the good news.
        Ok(answer) if answer.exit_code == Some(1) => return Present::Named(Vec::new()),
        Ok(answer) if answer.succeeded() => answer,
        Ok(answer) => {
            tracing::warn!(
                root = %root.display(),
                exit = ?answer.exit_code,
                tail = %answer.output_tail.trim(),
                "git would not search the working tree for a citation"
            );
            return why_not(root).await;
        }
        Err(reason) => {
            tracing::warn!(
                root = %root.display(),
                %reason,
                "could not run git to search the working tree for a citation"
            );
            return Present::Unreadable;
        }
    };

    let mut naming = Vec::new();
    // `-z` rather than `core.quotepath=false`: `git grep` prints raw paths under it, so there is no
    // quoting to undo and no exotic path silently dropped. The history walk cannot use the same
    // trick — see [`pickaxe`].
    for path in answer.stdout.split('\0').filter(|path| !path.is_empty()) {
        if !scanned(path) {
            continue;
        }
        let Ok(source) = std::fs::read_to_string(root.join(path)) else {
            // A file git can see and this cannot read is not evidence of anything. It reads as
            // naming nothing, which is what `project_map::structure` does with the same file.
            continue;
        };
        let cites: Vec<Citation> = citations(&source).into_iter().collect();
        if evidence(&cites, section, spec_slug, spec_slugs) != Evidence::Nothing {
            naming.push(path.to_owned());
        }
    }
    naming.sort();
    Present::Named(naming)
}

/// The commit just past the far edge of the window, or `None` when the history is shorter than it.
///
/// **One `rev-list` rather than `--max-count` on the pickaxe itself, because the two mean different
/// things.** `git log -S… --max-count=N` bounds the number of **matching** commits, not the depth of
/// the walk — so a section changed twice in the last week would stop the search a week back and call
/// everything before it unsearched, while a section nothing ever touched would diff the whole
/// history. The window this module promises is a depth, so the depth is what is measured, and the
/// same call answers the other half of the question for free: a boundary exists exactly when the
/// history is deeper than the window, which is what [`Orphan::NotInWindow`] reports.
///
/// `rev-list` walks commits without diffing them — 0.18 s over this repository's whole history — so
/// this costs nothing beside the pass it bounds.
async fn boundary(root: &Path, window: usize) -> Result<Option<String>, Orphan> {
    let skip = format!("--skip={window}");
    let argv: Vec<&OsStr> = ["rev-list", skip.as_str(), "--max-count=1", "HEAD"]
        .iter()
        .map(|arg| OsStr::new(*arg))
        .collect();

    match run_git(root, &argv, READ_TIMEOUT).await {
        Ok(answer) if answer.succeeded() => {
            let edge = answer.stdout.trim();
            Ok(if edge.is_empty() {
                None
            } else {
                Some(edge.to_owned())
            })
        }
        Ok(answer) => {
            tracing::warn!(
                root = %root.display(),
                window,
                exit = ?answer.exit_code,
                tail = %answer.output_tail.trim(),
                "git would not say how deep the history is, so the window cannot be placed"
            );
            Err(why_not(root).await.into())
        }
        Err(reason) => {
            tracing::warn!(
                root = %root.display(),
                window,
                %reason,
                "could not run git to place the window"
            );
            Err(Orphan::Unreadable)
        }
    }
}

/// The commits where the count of this literal changed, and what each did to which path.
///
/// **`-S` and never `-G` or `--pickaxe-regex`.** `-S` counts occurrences of a literal string, which
/// is both cheaper and the only form with nothing to escape. `-G` matches a regex against the diff
/// text and would flag a commit that merely moved a line containing the citation — noise the
/// confirmation pass would then have to pay two blob reads to discard.
///
/// **No pathspec, deliberately.** [`WINDOW`] records the measurement that made it a refusal rather
/// than an omission; the part worth repeating here is that a path limit turns git's history
/// simplification on, and with it the possibility of a commit being pruned. Filtering by [`scanned`]
/// afterwards costs nothing and prunes nothing.
///
/// **`core.quotepath=false` rather than `-z`, which is the trade `map_recency`'s `WALK_ARGV` argues
/// at length.** `-z` would give up git's quoting and with it the guarantee that a header is the only
/// line starting with [`RECORD_MARK`]. A path exotic enough to still arrive quoted matches no path
/// this map holds and is skipped, which costs one under-report on a file nothing here could have
/// named anyway.
async fn pickaxe(
    root: &Path,
    needle: &str,
    boundary: Option<&str>,
) -> Result<Vec<(Flagged, Change)>, Orphan> {
    let pickaxe = format!("-S{needle}");
    let range = boundary.map(|edge| format!("{edge}..HEAD"));
    let mut argv: Vec<&OsStr> = [
        "-c",
        "core.quotepath=false",
        "log",
        // The subject last, so the split can be bounded — see [`FIELD_MARK`].
        "--format=%x1e%H%x1f%ct%x1f%P%x1f%s",
        "--name-status",
        "--find-renames",
        pickaxe.as_str(),
    ]
    .iter()
    .map(|arg| OsStr::new(*arg))
    .collect();
    if let Some(range) = range.as_deref() {
        argv.push(OsStr::new(range));
    }

    match run_git(root, &argv, PICKAXE_TIMEOUT).await {
        Ok(answer) if answer.succeeded() => Ok(parse(&answer.stdout)),
        Ok(answer) => {
            tracing::warn!(
                root = %root.display(),
                exit = ?answer.exit_code,
                tail = %answer.output_tail.trim(),
                "git would not walk the history for a citation"
            );
            Err(why_not(root).await.into())
        }
        Err(reason) => {
            tracing::warn!(
                root = %root.display(),
                %reason,
                "could not run git to walk the history for a citation"
            );
            Err(Orphan::Unreadable)
        }
    }
}

/// One `(commit, change)` pair per name-status line, in git's order — newest first.
///
/// A pair per line and not a commit carrying a list, because every consumer here works one path at a
/// time and a commit that touched four files is four independent questions.
///
/// **A line this cannot read is skipped rather than failing the walk**, which is `map_recency`'s
/// rule for the same shape of problem and not `map_stamp`'s. A dropped line costs one file's
/// history; refusing the whole answer over it costs every file's.
fn parse(text: &str) -> Vec<(Flagged, Change)> {
    let mut found = Vec::new();
    let mut at: Option<Flagged> = None;

    for line in text.lines() {
        if let Some(header) = line.strip_prefix(RECORD_MARK) {
            at = read_header(header);
            continue;
        }
        if line.is_empty() {
            continue;
        }
        // Everything before the first header, and every path of a commit whose header would not
        // parse. Neither can be attributed to a commit, and a loss with no commit on it is a
        // sentence with the useful half missing.
        let Some(flagged) = at.as_ref() else {
            continue;
        };
        let Some(change) = read_change(line) else {
            continue;
        };
        found.push((flagged.clone(), change));
    }

    found
}

fn read_header(header: &str) -> Option<Flagged> {
    let mut fields = header.splitn(4, FIELD_MARK);
    let commit = fields.next()?.trim().to_owned();
    let at = fields.next()?.trim().parse::<i64>().ok()?;
    let parents: Vec<String> = fields
        .next()?
        .split_whitespace()
        .map(str::to_owned)
        .collect();
    // A subject is the only field allowed to be missing: `git commit --allow-empty-message` makes
    // one, and a commit with no title is still a commit somebody can be pointed at.
    let subject = fields.next().unwrap_or_default().to_owned();
    if commit.is_empty() {
        return None;
    }
    Some(Flagged {
        commit,
        at,
        parents,
        subject,
    })
}

/// One `M`/`A`/`D`/`R`/`C` line, as the before-and-after pair the confirmation needs.
///
/// **A copy has no before**, which is not a technicality: `C` says this content arrived from
/// somewhere that still has it, so the source lost nothing and the destination gained. Reading it as
/// a rename would invent a loss in a file that is still sitting there naming the section.
fn read_change(line: &str) -> Option<Change> {
    let mut fields = line.split('\t');
    let status = fields.next()?;
    let first = fields.next()?;
    let second = fields.next();

    // C-quoted, which `core.quotepath=false` does not undo. It matches no path this map holds, so
    // entering it would only put a row in an answer nobody can look up.
    if first.starts_with('"') || second.is_some_and(|path| path.starts_with('"')) {
        return None;
    }

    match status.chars().next()? {
        'A' => Some(Change {
            before: None,
            after: Some(first.to_owned()),
            renamed_to: None,
        }),
        'D' => Some(Change {
            before: Some(first.to_owned()),
            after: None,
            renamed_to: None,
        }),
        'R' => second.map(|to| Change {
            before: Some(first.to_owned()),
            after: Some(to.to_owned()),
            renamed_to: Some(to.to_owned()),
        }),
        'C' => second.map(|to| Change {
            before: None,
            after: Some(to.to_owned()),
            renamed_to: None,
        }),
        // `M`, and `T` for a file that changed type. Both are the same question.
        'M' | 'T' => Some(Change {
            before: Some(first.to_owned()),
            after: Some(first.to_owned()),
            renamed_to: None,
        }),
        _ => None,
    }
}

/// What one file said about one section at one revision, read by the project's own parser.
///
/// `Ok(None)` is *that file did not name it there*, which covers both a blob that named nothing and
/// a path that did not exist at all. `Err` is *git could not be asked*, and the caller turns it into
/// [`Orphan::Unreadable`] for the whole answer rather than reporting the losses it happened to
/// confirm — a partial list of losses is an under-report with nothing on it saying so.
async fn names(
    root: &Path,
    rev: &str,
    path: &str,
    section: &str,
    spec_slug: &str,
    spec_slugs: &[String],
) -> Result<Option<Evidence>, ()> {
    match blob(root, rev, path).await {
        Blob::Read(source) => {
            let cites: Vec<Citation> = citations(&source).into_iter().collect();
            match evidence(&cites, section, spec_slug, spec_slugs) {
                Evidence::Nothing => Ok(None),
                found => Ok(Some(found)),
            }
        }
        Blob::Absent => Ok(None),
        Blob::Failed => Err(()),
    }
}

/// One file, as it stood at one revision.
///
/// A non-zero exit is [`Blob::Absent`]: git ran, git looked, and the path was not there — which is
/// the ordinary answer for the parent of the commit that created a file, and for the root commit's
/// own non-existent parent.
async fn blob(root: &Path, rev: &str, path: &str) -> Blob {
    let object = format!("{rev}:{path}");
    let argv: Vec<&OsStr> = ["show", object.as_str()]
        .iter()
        .map(|arg| OsStr::new(*arg))
        .collect();

    match run_git(root, &argv, READ_TIMEOUT).await {
        Ok(answer) if answer.succeeded() => Blob::Read(answer.stdout),
        Ok(_) => Blob::Absent,
        Err(reason) => {
            tracing::warn!(
                root = %root.display(),
                %object,
                %reason,
                "could not run git to read a file out of the history"
            );
            Blob::Failed
        }
    }
}

/// Whether a git that would not answer is a git with nothing to answer about.
///
/// One extra spawn on the failure path only, exactly as `map_stamp`'s function of the same name —
/// and the asymmetry that function argues for holds here too: a machine with no git at all produces
/// `Err` for both calls, and reading that as *no repository* would tell somebody with a perfectly
/// good repository that their project has none.
async fn why_not(root: &Path) -> Present {
    let argv: Vec<&OsStr> = ["rev-parse", "--git-dir"].iter().map(OsStr::new).collect();
    match run_git(root, &argv, READ_TIMEOUT).await {
        Ok(answer) if answer.succeeded() => Present::Unreadable,
        Ok(_) => Present::NoRepository,
        Err(reason) => {
            tracing::warn!(
                root = %root.display(),
                %reason,
                "git would not run at all, so whether this folder is a repository is unknown"
            );
            Present::Unreadable
        }
    }
}

impl From<Present> for Orphan {
    fn from(present: Present) -> Self {
        match present {
            Present::NoRepository => Orphan::NoRepository,
            // `Named` never reaches here: [`why_not`] returns only the two failures, and it is the
            // only producer this conversion has.
            Present::Unreadable | Present::Named(_) => Orphan::Unreadable,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    /// The document every fixture's decision belongs to.
    const SPEC: &str = "a-spec-design";

    /// A directory in the **system** temp folder, deleted when it drops.
    ///
    /// A `TempDir` and not a `remove_dir_all` at the bottom of the body, for the reason
    /// `map_recency`'s identical helper gives: `Drop` runs while a panic unwinds and a line at the
    /// bottom of the body does not, and this repository already pays for that difference in
    /// stranded `%TEMP%` directories — each of these fixtures being a git repository, `.git` and
    /// all. Not `git_exec::space_free_tempdir`, which builds its directory under this checkout and
    /// would leave `a_folder_that_is_not_a_repository_says_so` walking nucleos' own history while
    /// claiming to walk nothing.
    fn scratch(prefix: &str) -> tempfile::TempDir {
        tempfile::Builder::new()
            .prefix(prefix)
            .tempdir()
            .expect("create a temporary directory")
    }

    fn git_in(dir: &Path, args: &[&str]) {
        let status = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .status()
            .expect("git should start");
        assert!(status.success(), "git {args:?} failed in {}", dir.display());
    }

    fn git_says(dir: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .expect("git should start");
        assert!(
            out.status.success(),
            "git {args:?} failed in {}",
            dir.display()
        );
        String::from_utf8_lossy(&out.stdout).trim().to_owned()
    }

    /// A repository whose branch is named rather than inherited.
    ///
    /// `init.defaultBranch` is a per-machine setting and two of the fixtures below check out a
    /// branch by name, so the name is forced here instead of being whatever the developer's global
    /// config happens to say.
    fn repository(prefix: &str) -> tempfile::TempDir {
        let dir = scratch(prefix);
        git_in(dir.path(), &["init", "-q", "--initial-branch=master"]);
        git_in(dir.path(), &["config", "user.email", "test@x"]);
        git_in(dir.path(), &["config", "user.name", "test"]);
        git_in(dir.path(), &["config", "core.autocrlf", "false"]);
        dir
    }

    fn write(root: &Path, path: &str, contents: &str) {
        let file = root.join(path);
        if let Some(parent) = file.parent() {
            std::fs::create_dir_all(parent).expect("create the file's directory");
        }
        std::fs::write(file, contents).expect("write the file");
    }

    fn remove(root: &Path, path: &str) {
        std::fs::remove_file(root.join(path)).expect("delete the file");
    }

    /// One commit at a committer date this test chose.
    ///
    /// Forged rather than taken from the clock, following `map_recency`'s helper: several commits
    /// made in a row land in the same second, and half of what is asserted below is *which* commit
    /// a loss is attributed to.
    fn commit_at(root: &Path, message: &str, when: i64) -> String {
        git_in(root, &["add", "-A"]);
        run_committing(root, &["commit", "-q", "-m", message], when);
        git_says(root, &["rev-parse", "HEAD"])
    }

    fn merge_at(root: &Path, branch: &str, message: &str, when: i64) -> String {
        run_committing(
            root,
            &["merge", "--no-ff", "-q", "-m", message, branch],
            when,
        );
        git_says(root, &["rev-parse", "HEAD"])
    }

    fn run_committing(root: &Path, args: &[&str], when: i64) {
        let stamp = format!("{when} +0000");
        let status = Command::new("git")
            .arg("-C")
            .arg(root)
            .args(args)
            .env("GIT_COMMITTER_DATE", &stamp)
            .env("GIT_AUTHOR_DATE", &stamp)
            .status()
            .expect("git should start");
        assert!(
            status.success(),
            "git {args:?} failed in {}",
            root.display()
        );
    }

    /// Enough body for `--find-renames` to recognise a moved file.
    ///
    /// Git's similarity index is 50% by default, so a two-line fixture renamed and edited reads as
    /// a delete and an add — which would make the two rename tests below pass for the wrong reason.
    fn filler() -> String {
        (0..40)
            .map(|line| format!("// a line of body, number {line}\n"))
            .collect()
    }

    fn module(citation: &str) -> String {
        format!("//! A module. {citation}\n{}", filler())
    }

    async fn answer(root: &Path, section: &str) -> Orphan {
        orphan(root, section, SPEC, &[SPEC.to_owned()]).await
    }

    fn losses(answer: &Orphan) -> &[Loss] {
        match answer {
            Orphan::Lost { losses } => losses,
            other => panic!("expected losses, got {other:?}"),
        }
    }

    const FIRST: i64 = 1_700_000_000;
    const SECOND: i64 = 1_700_086_400;
    const THIRD: i64 = 1_700_172_800;
    const FOURTH: i64 = 1_700_259_200;

    #[tokio::test]
    async fn a_file_that_still_names_it_is_not_an_orphan() {
        let repo = repository("nucleos-orphan-still-");
        let root = repo.path();
        write(root, "core/src/a.rs", &module("§7.1"));
        commit_at(root, "one", FIRST);

        assert_eq!(
            answer(root, "7.1").await,
            Orphan::StillNamed {
                paths: vec!["core/src/a.rs".to_owned()]
            }
        );
    }

    #[tokio::test]
    async fn a_section_nothing_ever_named_is_never_named() {
        let repo = repository("nucleos-orphan-never-");
        let root = repo.path();
        write(root, "core/src/a.rs", &module("§7.1"));
        commit_at(root, "one", FIRST);

        assert_eq!(answer(root, "9.2").await, Orphan::NeverNamed);
    }

    #[tokio::test]
    async fn a_citation_that_left_says_which_file_and_which_commit() {
        let repo = repository("nucleos-orphan-lost-");
        let root = repo.path();
        write(root, "core/src/a.rs", &module("§7.1"));
        commit_at(root, "the citation arrives", FIRST);
        write(root, "core/src/a.rs", &module("no citation here"));
        let removal = commit_at(root, "refactor: rewrite the module header", SECOND);

        let found = answer(root, "7.1").await;
        assert_eq!(
            losses(&found),
            [Loss {
                path: "core/src/a.rs".to_owned(),
                commit: removal,
                at: SECOND,
                subject: "refactor: rewrite the module header".to_owned(),
                renamed_to: None,
                // §8 is unfixed in this fixture exactly as it is in the repository: the citation
                // named a section and no document.
                declared: false,
            }]
        );
    }

    /// The first of the four defects §14.9 left as requirements.
    ///
    /// The sweep escaped its section number with `sed 's/\./\./g'` — a dot replaced by a dot — so
    /// the dot stayed a metacharacter and `§5.1` matched `§5x1`. Here that text is the only thing
    /// the file ever carried, and the honest answer about §5.1 is that nothing ever named it.
    ///
    /// **This test does not discriminate, and saying so is the point of writing it down.** The
    /// three mutations the rest of this suite was falsified against — the parser replaced by a
    /// substring test, *flagged means lost*, and `--first-parent` — leave it green, because the
    /// defect it names cannot be reintroduced by any of them: `-S` searches for a literal,
    /// `--pickaxe-regex` is never passed, and `map_join::leading_number` reads `§5x1` as section
    /// `5x` whatever the filter did. It is a regression guard against a future author reaching for
    /// a pattern in both passes at once, and a green here on its own is worth nothing. §14.9's
    /// whole lesson is that a test which cannot fail licenses the code it is pointed at, so this
    /// one says out loud that it is not licensing anything.
    #[tokio::test]
    async fn a_dot_in_a_section_number_is_not_a_wildcard() {
        let repo = repository("nucleos-orphan-dot-");
        let root = repo.path();
        write(root, "core/src/a.rs", &module("§5x1"));
        commit_at(root, "one", FIRST);
        write(root, "core/src/a.rs", &module("nothing"));
        commit_at(root, "two", SECOND);

        assert_eq!(answer(root, "5.1").await, Orphan::NeverNamed);
    }

    /// The second defect, in the direction that loses a whole subsection tree into its parent.
    ///
    /// `§3([^0-9]|$)` matched `§3.5`, so a top-level number absorbed every subsection under it —
    /// which is how the sweep reported the same removed line twice, once as §3 and once as §3.5.
    ///
    /// **The filter here really does match**, which is the point: `-S'§3'` counts the substring
    /// inside `§3.5` and flags the commit. It is the confirmation pass that refuses it, because
    /// `citations` reads the section as `3.5` and `evidence` compares section labels rather than
    /// prefixes. This is the two-pass shape doing the exact job §14.4 gives it.
    #[tokio::test]
    async fn a_top_level_number_does_not_swallow_its_subsections() {
        let repo = repository("nucleos-orphan-subsection-");
        let root = repo.path();
        write(root, "core/src/a.rs", &module("§3.5"));
        commit_at(root, "one", FIRST);
        write(root, "core/src/a.rs", &module("nothing"));
        let removal = commit_at(root, "two", SECOND);

        assert_eq!(answer(root, "3").await, Orphan::NeverNamed);
        // And the section that really left is found, so the refusal above is a refusal and not a
        // guard that silences everything.
        assert_eq!(losses(&answer(root, "3.5").await)[0].commit, removal);
    }

    /// The second defect in the other direction: `§6.1` is not `§6.1a`.
    ///
    /// These documents use letter suffixes — `6c`, `7a`, `4.4a` — which `map_join::leading_number`
    /// already knows about and a hand-written boundary did not.
    #[tokio::test]
    async fn a_letter_suffix_is_a_different_section() {
        let repo = repository("nucleos-orphan-suffix-");
        let root = repo.path();
        write(root, "core/src/a.rs", &module("§6.1a"));
        commit_at(root, "one", FIRST);
        write(root, "core/src/a.rs", &module("nothing"));
        let removal = commit_at(root, "two", SECOND);

        assert_eq!(answer(root, "6.1").await, Orphan::NeverNamed);
        assert_eq!(losses(&answer(root, "6.1a").await)[0].commit, removal);
    }

    /// The third defect: the pickaxe flags a count that CHANGED, in either direction.
    ///
    /// The sweep took the newest flagged commit for a file and called it the removal.
    /// `ModeMap.tsx`'s only flagged commit went from zero occurrences to two — an addition — and
    /// was reported as having lost the citation it had just gained.
    ///
    /// **The obvious fixture for this does not discriminate, and the first version of this test was
    /// it.** Two files that each gain and then lose the citation gives the right answer under a
    /// *flagged means lost* implementation too, because for a path that no longer names the section
    /// the newest flagged commit really is its removal — any later change to the count would itself
    /// have been flagged. Written that way, the test passed under the mutation it exists to catch,
    /// which is §14.9's own lesson arriving inside the test that cites it.
    ///
    /// So the fixture puts an addition where the newest flagged commit is: `b.rs` arrives carrying
    /// `§5.10`, which contains the searched text as a substring and is a different section. The
    /// filter flags it, the confirmation refuses it, and one loss comes back — `a.rs`, one commit
    /// earlier. An implementation that reads *flagged* as *removed* reports `b.rs` as having lost a
    /// citation it never carried.
    #[tokio::test]
    async fn an_addition_is_never_read_as_a_removal() {
        let repo = repository("nucleos-orphan-addition-");
        let root = repo.path();
        write(root, "core/src/a.rs", &module("§5.1"));
        commit_at(root, "a gains it", FIRST);
        write(root, "core/src/a.rs", &module("nothing"));
        let a_lost = commit_at(root, "a loses it", SECOND);
        write(root, "core/src/b.rs", &module("§5.10"));
        commit_at(root, "b arrives naming a different section", THIRD);

        let found = answer(root, "5.1").await;
        assert_eq!(
            losses(&found)
                .iter()
                .map(|loss| (loss.path.as_str(), loss.commit.as_str()))
                .collect::<Vec<_>>(),
            [("core/src/a.rs", a_lost.as_str())]
        );
    }

    /// The fourth defect, and the measurement that replaced its premise.
    ///
    /// §14.9 said `<commit>^` on a merge reads the first parent and that most flagged commits here
    /// were merges. The second half was itself an artefact: `git log -S` runs the pickaxe over a
    /// diff and a merge has none by default, so a merge is never flagged — 181 flagged commits over
    /// seven sections of this repository on 2026-08-27, of which zero were merges.
    ///
    /// What that leaves is the property worth pinning, and this is it: a removal made on a side
    /// branch and merged is found, attributed to the commit on the branch and never to the merge.
    /// It is also the answer to the row §14.10 marked *by medir* — the walk carries no pathspec, so
    /// git's path-based history simplification never runs and there is nothing for `--full-history`
    /// to restore.
    #[tokio::test]
    async fn a_removal_on_a_side_branch_survives_the_merge() {
        let repo = repository("nucleos-orphan-merge-");
        let root = repo.path();
        write(root, "core/src/a.rs", &module("§5.1"));
        commit_at(root, "the citation arrives", FIRST);

        git_in(root, &["checkout", "-q", "-b", "side"]);
        write(root, "core/src/a.rs", &module("nothing"));
        let removal = commit_at(root, "the branch drops it", SECOND);

        git_in(root, &["checkout", "-q", "master"]);
        write(root, "core/src/unrelated.rs", &module("nothing at all"));
        commit_at(root, "master moves on", THIRD);
        let merge = merge_at(root, "side", "merge side into master", FOURTH);

        let found = answer(root, "5.1").await;
        let losses = losses(&found);
        assert_eq!(losses.len(), 1, "one removal happened, on the branch");
        assert_eq!(losses[0].commit, removal);
        assert_ne!(losses[0].commit, merge, "the merge did not remove anything");
    }

    /// §14.6's rename mitigation, exercised where it can actually fire.
    ///
    /// The file is renamed and drops one of its two citations — enough for the pickaxe to flag the
    /// rename — and then loses the last one a commit later. Only the second is a loss: at the
    /// rename the destination still named the section, and reporting it would be a loss in a file
    /// that had simply moved.
    #[tokio::test]
    async fn a_rename_whose_destination_still_names_it_is_not_a_loss() {
        let repo = repository("nucleos-orphan-rename-kept-");
        let root = repo.path();
        write(
            root,
            "core/src/old.rs",
            &format!("//! §7.1 twice, and again §7.1\n{}", filler()),
        );
        commit_at(root, "one", FIRST);

        remove(root, "core/src/old.rs");
        write(
            root,
            "core/src/new.rs",
            &format!("//! §7.1 once now\n{}", filler()),
        );
        commit_at(root, "moved, and one citation dropped", SECOND);

        write(root, "core/src/new.rs", &module("nothing"));
        let removal = commit_at(root, "and now none", THIRD);

        let found = answer(root, "7.1").await;
        assert_eq!(
            losses(&found)
                .iter()
                .map(|loss| (loss.path.as_str(), loss.commit.as_str()))
                .collect::<Vec<_>>(),
            [("core/src/new.rs", removal.as_str())]
        );
    }

    /// A rename that drops the citation says where the file went.
    ///
    /// The loss is reported under the path that was carrying the citation — the old one — because
    /// that is the file somebody remembers. `renamed_to` is what stops that from reading as a
    /// deletion.
    #[tokio::test]
    async fn a_rename_that_drops_it_says_where_the_file_went() {
        let repo = repository("nucleos-orphan-rename-lost-");
        let root = repo.path();
        write(root, "core/src/old.rs", &module("§7.1"));
        commit_at(root, "one", FIRST);

        remove(root, "core/src/old.rs");
        write(root, "core/src/new.rs", &module("nothing"));
        let removal = commit_at(root, "moved and rewritten", SECOND);

        let found = answer(root, "7.1").await;
        let losses = losses(&found);
        assert_eq!(losses.len(), 1);
        assert_eq!(losses[0].path, "core/src/old.rs");
        assert_eq!(losses[0].commit, removal);
        assert_eq!(losses[0].renamed_to.as_deref(), Some("core/src/new.rs"));
    }

    /// A file deleted outright is a loss, and the answer says which commit deleted it.
    #[tokio::test]
    async fn a_deleted_file_is_a_loss_like_any_other() {
        let repo = repository("nucleos-orphan-deleted-");
        let root = repo.path();
        write(root, "core/src/a.rs", &module("§7.1"));
        commit_at(root, "one", FIRST);
        remove(root, "core/src/a.rs");
        let removal = commit_at(root, "delete the module", SECOND);

        let found = answer(root, "7.1").await;
        let losses = losses(&found);
        assert_eq!(losses[0].path, "core/src/a.rs");
        assert_eq!(losses[0].commit, removal);
        assert_eq!(losses[0].renamed_to, None);
    }

    /// A citation this map never reads is not a loss this map may report.
    ///
    /// A `.md` is outside [`scanned`], so the structure layer never counted it as naming anything —
    /// and a guard that reported it would be making a claim about a file the map does not look at,
    /// under the one word that is supposed to mean something definite.
    #[tokio::test]
    async fn a_file_this_map_never_reads_is_not_a_loss() {
        let repo = repository("nucleos-orphan-unscanned-");
        let root = repo.path();
        write(root, "notes.md", "A note about §7.1.\n");
        commit_at(root, "one", FIRST);
        write(root, "notes.md", "A note about nothing.\n");
        commit_at(root, "two", SECOND);

        assert_eq!(answer(root, "7.1").await, Orphan::NeverNamed);
    }

    /// A citation belonging to another of this project's documents is not this decision's loss.
    ///
    /// The rule is `map_join::evidence`'s own — a candidate that names a DIFFERENT document of this
    /// project is evidence against and the citation is skipped, never downgraded — and it is the
    /// same call rather than a second opinion about the same question. Until 2026-08-28 nothing in
    /// this repository declared anything and this branch was inert. The map's own modules now carry
    /// a `§spec` header, so it is live for them and still inert everywhere else — which is the
    /// difference between a guard that answers about one document and one that answers about all of
    /// them at once, arriving one group of files at a time.
    #[tokio::test]
    async fn a_citation_under_another_document_is_not_this_decision_s_loss() {
        let repo = repository("nucleos-orphan-other-doc-");
        let root = repo.path();
        let others = ["other-spec-design".to_owned(), SPEC.to_owned()];
        write(
            root,
            "core/src/a.rs",
            &format!(
                "//! §spec other-spec-design\n//! It implements §7.1.\n{}",
                filler()
            ),
        );
        commit_at(root, "one", FIRST);
        write(root, "core/src/a.rs", &module("nothing"));
        commit_at(root, "two", SECOND);

        assert_eq!(
            orphan(root, "7.1", SPEC, &others).await,
            Orphan::NeverNamed,
            "the file was declared under another document all along"
        );
        assert!(
            matches!(
                orphan(root, "7.1", "other-spec-design", &others).await,
                Orphan::Lost { .. }
            ),
            "and it is a loss for the document that really lost it"
        );
    }

    /// A history deeper than the window says so, rather than saying nothing was ever found.
    #[tokio::test]
    async fn a_history_deeper_than_the_window_is_not_reported_as_never_named() {
        let repo = repository("nucleos-orphan-window-");
        let root = repo.path();
        write(root, "core/src/a.rs", &module("§7.1"));
        commit_at(root, "the citation arrives", FIRST);
        write(root, "core/src/a.rs", &module("nothing"));
        commit_at(root, "and leaves", SECOND);
        write(root, "core/src/b.rs", &module("unrelated"));
        commit_at(root, "later", THIRD);
        write(root, "core/src/c.rs", &module("unrelated"));
        commit_at(root, "later still", FOURTH);

        assert_eq!(
            orphan_within(root, "7.1", SPEC, &[SPEC.to_owned()], 2).await,
            Orphan::NotInWindow { window: 2 },
            "the removal is three commits back and the window reaches two"
        );
        assert!(
            matches!(
                orphan_within(root, "7.1", SPEC, &[SPEC.to_owned()], 10).await,
                Orphan::Lost { .. }
            ),
            "and a window that reaches it finds it"
        );
    }

    #[tokio::test]
    async fn a_folder_that_is_not_a_repository_says_so() {
        let dir = scratch("nucleos-orphan-norepo-");
        write(dir.path(), "core/src/a.rs", &module("§7.1"));

        assert_eq!(answer(dir.path(), "7.1").await, Orphan::NoRepository);
    }

    /// Two readings of a repository nothing has happened to must agree.
    ///
    /// §14.10's claim is that no step of this consults a model and the chain is the pickaxe, a blob
    /// read, the project's parser and a set difference. Determinism is what that buys — and it is
    /// worth asserting rather than assuming, because it is also all it buys: the sweep §14.9
    /// retracts was perfectly deterministic and perfectly wrong.
    #[tokio::test]
    async fn two_readings_of_an_unchanged_repository_agree() {
        let repo = repository("nucleos-orphan-twice-");
        let root = repo.path();
        write(root, "core/src/a.rs", &module("§5.1"));
        commit_at(root, "one", FIRST);
        write(root, "core/src/b.rs", &module("§5.1"));
        commit_at(root, "two", SECOND);
        write(root, "core/src/a.rs", &module("nothing"));
        commit_at(root, "three", THIRD);
        write(root, "core/src/b.rs", &module("nothing"));
        commit_at(root, "four", FOURTH);

        assert_eq!(answer(root, "5.1").await, answer(root, "5.1").await);
    }

    #[test]
    fn a_copy_carries_no_before_and_therefore_no_loss() {
        let copied = read_change("C87\tcore/src/a.rs\tcore/src/b.rs").expect("a copy parses");
        assert_eq!(copied.before, None);
        assert_eq!(copied.after.as_deref(), Some("core/src/b.rs"));
        assert_eq!(copied.renamed_to, None);
    }

    #[test]
    fn a_rename_keeps_both_halves_of_the_move() {
        let moved = read_change("R91\tcore/src/a.rs\tcore/src/b.rs").expect("a rename parses");
        assert_eq!(moved.before.as_deref(), Some("core/src/a.rs"));
        assert_eq!(moved.after.as_deref(), Some("core/src/b.rs"));
        assert_eq!(moved.renamed_to.as_deref(), Some("core/src/b.rs"));
    }

    #[test]
    fn a_quoted_path_is_skipped_rather_than_entered_under_its_quoted_name() {
        assert!(read_change("M\t\"core/src/a\\033b.rs\"").is_none());
        assert!(read_change("R91\tcore/src/a.rs\t\"core/src/a\\033b.rs\"").is_none());
    }

    #[test]
    fn a_header_with_no_subject_still_names_a_commit() {
        let header = format!("abc123{FIELD_MARK}1700000000{FIELD_MARK}def456");
        let read = read_header(&header).expect("a subjectless header parses");
        assert_eq!(read.commit, "abc123");
        assert_eq!(read.at, 1_700_000_000);
        assert_eq!(read.parents, ["def456"]);
        assert_eq!(read.subject, "");
    }

    #[test]
    fn a_root_commit_has_no_parents_and_that_is_not_a_parse_failure() {
        let header = format!("abc123{FIELD_MARK}1700000000{FIELD_MARK}{FIELD_MARK}first");
        let read = read_header(&header).expect("a root commit's header parses");
        assert!(read.parents.is_empty());
        assert_eq!(read.subject, "first");
    }

    /// Paths seen before the first header belong to no commit and are dropped.
    #[test]
    fn a_status_line_with_no_header_above_it_is_dropped() {
        assert!(parse("M\tcore/src/a.rs\n").is_empty());
    }
}
