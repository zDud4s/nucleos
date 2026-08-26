//! §10's ordering: what arrives first is what moved last.
//!
//! > *"Dentro do que chega, a ordem é por **recência de alteração do código âncora**, não por
//! > importância. Importância exigiria um juízo, e o único juízo autorizado neste desenho é o do
//! > dono. Recência é um facto do git e responde à pergunta certa: o que é que se mexeu desde a
//! > última vez que olhei?"*
//!
//! **Its own module rather than a sixth concern in `map_stamp.rs`, and that module's own header is
//! the argument.** It took the anchor digest in — against §9.3's table, which had the git call
//! living beside `git_exec.rs` — because *the form is the contract between whoever writes a digest
//! and whoever reads it back*, and a producer and a form that must agree byte for byte belong on
//! one screen. Nothing here is a contract with anything. No row stores an ordering, no stamp is
//! compared against one, and a walk taken for one request is thrown away before the next; get it
//! wrong and a list is in a worse order, not a green over changed code. What is left of the
//! resemblance is *both of them call git*, which is precisely the reason that header gives for why
//! `git_exec.rs` was the wrong home for the digest — transport is not a concern.
//!
//! The positive half is smaller and harder to argue away. `map_stamp.rs` is the verdict, the anchor
//! digest and the expiry, and its first line says so; a sort that never reads a stamp, that runs
//! over exactly the decisions no stamp exists for (§10 scopes the triager to
//! [`crate::map_stamp::Standing::Never`]), and that is spent on a route which writes no stamp at
//! all, would make that line false — and a module whose header no longer describes it is how 2 000
//! lines become 2 500 without anybody deciding to.
//!
//! **Pure, tirando a chamada ao `git`** — the same sentence §9.3 already writes about `map_stamp.rs`,
//! and here it splits cleanly in two: [`walk`] is the only thing in this file that spawns anything,
//! and [`parse`], [`Walk::age`] and [`order`] are exercisable with no repository within reach.

use crate::git_exec::run_git;
use crate::map_join::Anchored;
use serde::Serialize;
use std::cmp::Reverse;
use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::path::Path;
use std::time::Duration;

/// How many commits back the ordering can see.
///
/// **The number the panel says out loud**, because §10 presents this ordering as *um facto do git*
/// and a sort that silently stops being one is the portrait decision 1 refuses. A decision whose
/// anchors last moved a thousand commits ago and one whose anchors have never moved sort alike
/// here; that is a real approximation, so [`Recency::window`] carries the number to the screen
/// rather than letting the list imply the order is total.
///
/// **Two hundred, measured on this repository on 2026-08-26** — best of five runs of
/// `git -c core.quotepath=false log --format=%x1e%ct --name-only -n N`, against the 112 files under
/// `core/src` and `shell/src` that name a `§` (the count was 109 when `MAX_TRIAGE_BATCH` was sized
/// eight days of commits ago, which is itself the argument for the window existing):
///
/// | N | wall | output | of the 112 anchors | reaches back to |
/// |---|---|---|---|---|
/// | 20 | 59 ms | 1.4 KB | 17 | 4 hours |
/// | 50 | 77 ms | 3.2 KB | 26 | 15 hours |
/// | 100 | 102 ms | 6.6 KB | 45 | 1.5 days |
/// | 150 | 151 ms | 16 KB | 77 | 4 days |
/// | **200** | **195 ms** | **22 KB** | **83** | **5.5 days** |
/// | 300 | 294 ms | 37 KB | 89 | 7 days |
/// | 400 | 388 ms | 51 KB | 104 | 10 days |
/// | 800 | 442 ms | 85 KB | 110 | 4 weeks |
///
/// Re-timed through [`run_git`] afterwards rather than trusted from a shell: 193, 196, 209 and
/// 254 ms over four consecutive walks of this repository at N=200, so the pipes, the process group
/// and the deadline cost nothing worth naming. The FIRST walk after a rebuild took **962 ms** —
/// cold git object cache and a virus scanner meeting a new binary — which is worth knowing before
/// somebody re-measures once, reads 962, and halves the window over it.
///
/// The knee is between 150 and 200 and the marginal value falls off a cliff after it: 100→150 buys
/// 32 anchor files for 49 ms, 150→200 buys 6 for 44 ms, and 200→250 buys 3 for 49 ms. What decides
/// between the two survivors is the calendar rather than the file count — that 44 ms buys a day and
/// a half of history, and §10's question is *since the last time I looked*, which for anybody who
/// opens this weekly is not four days.
///
/// **Sized against the sum and not against itself.** This walk is sequential with the
/// `git ls-files` digest on the very same request (77 ms, measured under
/// `map_stamp::LS_FILES_TIMEOUT`), on a route the window performs every time the map opens. Two
/// hundred is the last window that keeps the walk itself under 200 ms and the pair of git calls
/// under 300; four hundred would double this half of the bill to buy the eight anchor files above.
/// Running the two concurrently would hide most of it and is deliberately not done — the triage
/// route shares this code and then spends *minutes* in a model, so the concurrency would be worth
/// 195 ms on one caller and nothing on the other, at the price of two git processes on one
/// repository and two code paths where there is now one.
///
/// **This repository is the fastest-moving one this feature will meet, so the number errs long
/// elsewhere and costs less there.** Thirty-five commits a day, 1 083 in six weeks; two hundred
/// commits is five and a half days here and months on an ordinary project. And the walk is O(N)
/// rather than O(history) — `git log -n 200` stops at two hundred commits whatever is behind them —
/// so a repository a hundred times this size does not move the measurement, only a repository with
/// much larger trees does.
///
/// **Merges are walked and left in, having been measured rather than assumed.** `--name-only`
/// prints nothing for a merge commit, so 135 of this repository's 1 083 commits spend a slot of the
/// window without contributing a path. `--no-merges` was tried: at N=200 it resolves 86 of the 112
/// instead of 83 and reaches sixteen hours further back. Three files is not enough to make the
/// panel say *"the last 200 commits that touched a file"* instead of *"the last 200 commits"*, and
/// nothing is lost by keeping them — the commits a merge merged are in the walk under their own
/// dates.
pub const WINDOW: usize = 200;

/// How long the walk is given before the ordering is given up on.
///
/// Five seconds, the same ceiling `map_stamp::LS_FILES_TIMEOUT` argues for, on the same request and
/// for the same reason: not a bound on a slow answer but on a hang, and a hang is what whoever is
/// looking at the window cannot tell from an application that has broken. It is 25× the measured
/// 195 ms and 11× the 442 ms this repository's whole history costs, and the walk is O(N) rather
/// than O(history), so the headroom is for a project with much bigger trees rather than a much
/// longer past.
///
/// A shorter deadline would be defensible here in a way it is not for the digest — losing this call
/// costs the sort and nothing else, where losing that one costs every stamp's expiry. It is not
/// shorter because the two run back to back on one request: a route that has already waited five
/// seconds for `ls-files` is not one anybody is still watching, so a second ceiling half the size
/// would buy nothing it could spend.
const WALK_TIMEOUT: Duration = Duration::from_secs(5);

/// What marks a commit's line in the walk's output.
///
/// **A byte git cannot print unquoted at the start of a path**, which is what makes the parse
/// unambiguous rather than heuristic. `git log --format=%ct --name-only` prints a bare timestamp
/// line and then bare path lines, and nothing in that says which is which: a file called
/// `1787757225` at the repository root reads exactly like a commit header, and so does the blank
/// line rule once a merge commit — which prints no paths at all — is in the window. With
/// `--format=%x1e%ct` a header is the only line starting with U+001E, because `core.quotepath`
/// still C-quotes a control character inside a path even when it has been told to leave bytes above
/// 0x7F alone, so such a path arrives as `"a\036b.rs"` and starts with a quote.
///
/// That is also why the call carries `-c core.quotepath=false` and not `-z`. `-z` would give up
/// git's quoting altogether and with it the guarantee above — the NUL-separated form marks only the
/// FIRST path of each commit, so the record after the last path of one commit and the header of the
/// next are indistinguishable, which is the same ambiguity one layer down. `quotepath=false` keeps
/// `café.rs` spelled the way [`crate::map_join::Anchored::modules`] spells it, which is the case
/// `map_stamp::digest` spends `-z` on, and leaves the exotic paths quoted, where they match no
/// anchor and are read as *not in the window*. That is the honest failure for a sort: one decision
/// lower in a list than it deserves, announced by nothing, versus a whole walk mis-parsed.
const RECORD_MARK: char = '\u{1e}';

/// The fixed argv of the walk.
///
/// **One walk and never one per anchor file, which is the entire cost argument of this module.**
/// `git log -1 --format=%ct -- <path>` per anchor is ~700 process spawns on this repository, on a
/// route the map performs every time it opens; this is one, and the answer is sliced per decision
/// afterwards by [`Walk::age`]. `one_walk_and_not_one_per_decision` asserts the process count
/// rather than the ordering, because a later refactor to the obvious loop passes every other test
/// in this file.
///
/// **No pathspecs, which is what makes it one call rather than several.** `git log -n 200 -- <the
/// anchor paths>` would answer better — two hundred commits that each touched an anchor, rather
/// than two hundred commits of which some did — and it would cost the two things this call does not
/// pay: this repository's anchor paths measure 24 224 characters of argv against Windows' 32 767,
/// so `map_stamp::chunked`'s split would apply here too and turn one spawn into several, and the
/// walk would stop being bounded by N — two hundred commits touching an anchor can be a thousand
/// commits of searching on a repository where the map's decisions cover a corner of the tree.
///
/// `--literal-pathspecs` is absent for the same reason and is not an oversight: with nothing after
/// a `--` there is no pathspec to be taken as a glob, so the flag `map_stamp::LS_FILES_ARGV` needs
/// would guard nothing here.
const WALK_ARGV: [&str; 5] = [
    "-c",
    "core.quotepath=false",
    "log",
    "--format=%x1e%ct",
    "--name-only",
];

/// When each path last moved, as far back as the window reaches.
///
/// **`commits` is an `Option` and that is the whole of what this type says about failure.** §11's
/// project added from outside has no repository and never will; a daemon whose git is momentarily
/// unhappy has one and will. `map_stamp::Anchors` keeps those two apart at some cost — one extra
/// spawn on the failure path — because collapsing them told a repository-less project to *retry* a
/// thing that could never succeed. Here they have one consequence and one sentence: there is no
/// window, the list falls back to the order it arrived in, and the panel must not claim a number it
/// does not have. Neither of them is retryable by anybody looking at a sort. The distinction is not
/// lost either — `MapAnswer::git_would_not_answer` carries it for the same repository at the same
/// instant, from the call that actually needs it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Walk {
    commits: Option<usize>,
    moved: BTreeMap<String, i64>,
}

/// How long ago one decision's anchor code last moved, in the four states that are not each other.
///
/// **Four, and the three that are not [`Age::Moved`] are the reason this is an enum rather than an
/// `Option<i64>`.** They sort in the same region and mean entirely different things, and §10's
/// ordering is presented to its reader as a fact about git — a screen that renders three different
/// silences identically is the false confidence §1 describes, arriving through the rendering door
/// §6.1 watches.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Age {
    /// Inside the window: when the most recent of this decision's anchor files last moved, as a
    /// committer date in Unix seconds.
    ///
    /// **The committer date and not the author date**, because the question is *what moved since I
    /// last looked* and a commit rebased onto today's branch moved today whatever its author wrote
    /// on it a month ago. It is the same choice `map_stamp::digest` makes one granularity down when
    /// it reads the index rather than `HEAD`: err towards asking.
    Moved { at: i64 },
    /// Anchored, and none of its anchor files moved inside the window.
    ///
    /// Says nothing about how much older — a decision last touched two hundred and one commits ago
    /// and one last touched a thousand are both this, which is the approximation
    /// [`Recency::window`] exists to announce.
    Older,
    /// Nothing to move.
    ///
    /// **A fact about the decision and not about git**, which is why it survives a walk that never
    /// happened: `anchor_digests` gives such a decision `Computed("")` whatever became of the git
    /// call, for the same reason, and a decision that flapped between *nothing to watch* and *could
    /// not look* every time git hiccuped would be nagging about a fact that cannot change.
    ///
    /// **It sorts last, and the sentence it earns is not [`Age::Older`]'s.** Nothing moved because
    /// there is nothing to move, which is a different fact from nothing moved lately; §5.1 calls the
    /// first *declarado, sem código* and puts it in front of somebody precisely so it is looked at,
    /// and a list that spelled the two alike would bury it among rows that are merely quiet.
    ///
    /// **The one flattening in here, named rather than discovered:** a decision named only by a Go
    /// sidecar has an empty `modules` and a non-empty `foreign`, so it lands here although there is
    /// code that could have moved. That is `map_stamp::Watch::NoAnchor`'s flattening, taken
    /// deliberately so the two axes do not disagree about one decision — and the payload carries
    /// `Anchored::foreign` beside this, so a panel saying *no code names this* is choosing to,
    /// rather than being told to.
    Unanchored,
    /// Anchored, and git would not say when anything moved.
    ///
    /// **Never written when there is a window**, so it can only ever mean what it says. Reporting
    /// [`Age::Older`] for a project with no repository would be a claim that the walk looked and
    /// found nothing, which is the shape of wrong this feature may not be: *"uma resposta
    /// visivelmente aproximada é aceitável; uma silenciosamente errada não é."*
    Unknown,
}

/// §10's ordering, and how far it can see.
///
/// Beside `junction.decisions` on the wire rather than inside it, because the order the list is in
/// is only half the answer: the other half is which part of it the order is a fact about, and a
/// list carries that nowhere.
///
/// **What this ordering actually produces on this repository today, measured rather than assumed,
/// and it is not what §10 pictured.** Run over the real anchor sets on 2026-08-26 — one decision per
/// distinct cited section, which is exactly what `map_join::join` produces while §8 is unfixed and
/// every anchor is [`crate::map_join::Anchor::Ambiguous`]: **71 of 80 decisions land inside the
/// 200-commit window and carry only 19 distinct timestamps between them, and the top eighteen share
/// ONE — the head commit.** The head of the list is therefore a twenty-row tie broken by
/// `spec_slug, ordinal, id`, which is the alphabetical order §10 set out to escape.
///
/// **The cause is §8 and not this sort.** A decision's anchor set today is every module citing that
/// section NUMBER across every document, because nothing in a file says which of forty specs its
/// `§5.2` belongs to: median four modules, up to 67, and `§5.2` alone collects 42. The most recent
/// of forty-two files in a repository committing thirty-five times a day is *this morning*, for
/// almost any decision. The rule being applied is §10's own — *a recência do código âncora*, and the
/// most recent of the set — and it will start separating rows the moment the sets narrow to one
/// document's modules. Fifteen decisions are already anchored to a single file, and those sort
/// exactly as intended; so does the tail, where a three-file anchor set that has not moved lands in
/// [`Age::Older`] and the unanchored rows land last.
///
/// It is written down here rather than left for the panel to discover because the honest sentence
/// on screen depends on it: *ordered by what moved in the last 200 commits* is true and, today,
/// says less than a reader will assume — most of the head moved in the same commit. Nothing here
/// may repair it by weighing the anchor set, taking a median, or preferring narrow sets, because
/// every one of those is a judgement about which decision matters more, and §10 gives that judgement
/// to one person.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Recency {
    /// How many commits the walk asked for, or `None` when git would not say — the number the panel
    /// puts in *"ordered by what moved in the last N commits"*, and the sentence it must not write
    /// when this is absent.
    pub window: Option<usize>,
    /// Where each decision fell in that window, by `decision_id`.
    ///
    /// **A map and never a parallel array**, the rule `MapAnswer::standings` already keeps: two
    /// arrays that must stay index-aligned is a bug waiting for the first re-sort, and this field
    /// exists because of a re-sort.
    pub ages: BTreeMap<i64, Age>,
}

impl Walk {
    /// How long ago the most recent of these anchor files moved.
    ///
    /// **The most recent and not the oldest, the average, or the first.** A decision is as fresh as
    /// the freshest thing under it: §10 asks what moved since the owner last looked, and one file
    /// of six having moved this morning is a yes to that question whatever the other five did.
    pub fn age(&self, paths: &[String]) -> Age {
        if paths.is_empty() {
            return Age::Unanchored;
        }
        if self.commits.is_none() {
            return Age::Unknown;
        }
        match paths
            .iter()
            .filter_map(|path| self.moved.get(path.as_str()).copied())
            .max()
        {
            Some(at) => Age::Moved { at },
            None => Age::Older,
        }
    }
}

/// One walk of the last [`WINDOW`] commits.
pub async fn walk(root: &Path) -> Walk {
    walk_within(root, WINDOW).await
}

/// The walk, with the window as a parameter so the tests can have one small enough to fall outside
/// of.
///
/// Private, and [`walk`] is the only thing that names [`WINDOW`], so the two callers cannot come to
/// disagree about the size of the window they are both meant to be reporting.
async fn walk_within(root: &Path, commits: usize) -> Walk {
    let limit = format!("--max-count={commits}");
    let mut argv: Vec<&OsStr> = WALK_ARGV.iter().map(|arg| OsStr::new(*arg)).collect();
    argv.push(OsStr::new(limit.as_str()));

    let answer = match run_git(root, &argv, WALK_TIMEOUT).await {
        Ok(answer) if answer.succeeded() => answer,
        // Both failures are one answer here, and the `warn!` is what keeps that from being silent.
        // A folder that is not a repository exits 128 and so does a repository with no commits yet;
        // `map_stamp::why_not`'s second spawn would tell them apart and there is nothing for a sort
        // to do with the difference. See [`Walk`].
        Ok(answer) => {
            tracing::warn!(
                root = %root.display(),
                commits,
                exit = ?answer.exit_code,
                tail = %answer.output_tail.trim(),
                "git would not walk the recent commits, so the map keeps the order it read the \
                 decisions in"
            );
            return Walk {
                commits: None,
                moved: BTreeMap::new(),
            };
        }
        Err(reason) => {
            tracing::warn!(
                root = %root.display(),
                commits,
                %reason,
                "could not run git to walk the recent commits, so the map keeps the order it read \
                 the decisions in"
            );
            return Walk {
                commits: None,
                moved: BTreeMap::new(),
            };
        }
    };

    Walk {
        commits: Some(commits),
        moved: parse(&answer.stdout),
    }
}

/// A `path → last moved` map out of one walk's output.
///
/// **The maximum and not the first record seen**, although `git log` answers newest first and the
/// two agree today. The order git walks in is a fact about the graph and about which of
/// `--date-order`, `--author-date-order` and `--topo-order` is in force; the answer wanted here is
/// *the most recent commit in the window that touched this path*, and a `max` says exactly that
/// without depending on any of them. It costs one comparison per record.
///
/// **A line this cannot read is skipped and never fails the walk**, which is the opposite of
/// `map_stamp::digest`'s rule about the same shape of problem, and the difference is what the two
/// answers are for. A dropped anchor there reads as `Lapse::Moved { gone }` — a file that never
/// went anywhere — so *I could not look* is the only true answer. A dropped line here puts one
/// decision lower in a list than it deserved, and refusing the whole walk over it would put every
/// decision in the project there instead.
fn parse(text: &str) -> BTreeMap<String, i64> {
    let mut moved: BTreeMap<String, i64> = BTreeMap::new();
    let mut at: Option<i64> = None;

    for line in text.lines() {
        if let Some(stamp) = line.strip_prefix(RECORD_MARK) {
            // A header this cannot read takes its own commit's paths with it rather than filing
            // them under the previous commit's date, which would report a file as having moved at a
            // time nothing happened to it.
            at = stamp.trim().parse::<i64>().ok();
            continue;
        }
        if line.is_empty() {
            continue;
        }
        // Everything before the first header, and every path of a commit whose header would not
        // parse. Neither can be attributed to anything.
        let Some(when) = at else {
            continue;
        };
        // C-quoted — a path with a control character or a quote in it, which `core.quotepath=false`
        // does not unquote. It matches no anchor path in the spelling the map holds, so entering it
        // under its quoted name would only put a row in this map that nothing can ever look up.
        if line.starts_with('"') {
            continue;
        }
        moved
            .entry(line.to_owned())
            .and_modify(|held| *held = (*held).max(when))
            .or_insert(when);
    }

    moved
}

/// Puts the decisions in §10's order and says what that order is a fact about.
///
/// **In place, and both callers of the map share it.** `GET /map` sorts the list the panel reads
/// and `POST /map/triage` sorts the sweep that spends a model call per decision under
/// `MAX_TRIAGE_BATCH`; two orderings would be two answers to §10, and the one deciding what money
/// is spent on would be the one nobody looked at.
///
/// **Takes `Anchored` rather than a generic `(id, paths)` pair**, which would keep this module
/// clear of `map_join` and would also let a caller order by a path set that is not the anchor set.
/// §10 names the anchor code and nothing else, and the one place that could go wrong is a caller
/// helpfully folding in [`Anchored::foreign`] — the Go files, which `map_stamp::digest` does not
/// watch, so a decision would rise to the top of the list because of a file no other part of this
/// feature considers an anchor. `anchor_digests` argues the same point from the other side.
///
/// **A stable sort, and `sort_unstable_by_key` is the wrong function here rather than a faster
/// one.** Two readings of a repository nothing has happened to must not disagree: everything
/// outside the window ties, everything with no anchor ties, and on this repository that is most of
/// the list. A map that changes shape between two readings for no visible reason is exactly the
/// portrait decision 1 refuses. The order fallen back to is `map_store::approved`'s —
/// `spec_slug, ordinal, id` — because that is the order the decisions arrive in.
pub fn order(decisions: &mut [Anchored], walk: &Walk) -> Recency {
    let ages: BTreeMap<i64, Age> = decisions
        .iter()
        .map(|anchored| (anchored.decision_id, walk.age(&anchored.modules)))
        .collect();

    decisions.sort_by_key(|anchored| {
        // Unreachable: `ages` was built from this very slice one statement ago. `Unknown` is the
        // value that is safe to be wrong with — it claims nothing about git and sorts among the
        // rows the ordering already cannot speak for.
        rank(
            ages.get(&anchored.decision_id)
                .copied()
                .unwrap_or(Age::Unknown),
        )
    });

    Recency {
        window: walk.commits,
        ages,
    }
}

/// The sort key, newest first.
///
/// [`Reverse`] on the timestamp rather than reversing the whole comparison, because only the
/// timestamp runs backwards: the three silences behind it keep their own order, and that order is
/// an argument. [`Age::Older`] is *anchored, and quiet*; [`Age::Unknown`] is *anchored, and nobody
/// could look*, which is a row that might deserve attention and cannot be shown to; and
/// [`Age::Unanchored`] is last because there is nothing under it that could ever move.
/// `Older` and `Unknown` never occur in one answer — the second only exists when there is no window
/// and the first only when there is — so their order relative to each other is unobservable and is
/// written down anyway, since a `match` that has to be exhaustive is where the next reader looks
/// for the rule.
fn rank(age: Age) -> (u8, Reverse<i64>) {
    match age {
        Age::Moved { at } => (0, Reverse(at)),
        Age::Older => (1, Reverse(0)),
        Age::Unknown => (2, Reverse(0)),
        Age::Unanchored => (3, Reverse(0)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::map_intent::Kind;
    use crate::map_join::Anchor;
    use std::process::Command;

    /// A directory in the **system** temp folder, deleted when it drops.
    ///
    /// A `TempDir` and not a `remove_dir_all` at the bottom of the body, for the reason
    /// `map_stamp`'s identical helper gives: `Drop` runs while a panic unwinds and a line at the
    /// bottom of the body does not, and this repository already pays for that difference in
    /// stranded `%TEMP%` directories — each of these fixtures being a git repository, `.git` and
    /// all. Not `git_exec::space_free_tempdir`, which builds its directory under this checkout and
    /// would leave `a_folder_that_is_not_a_repository_orders_without_failing` walking nucleos' own
    /// history while claiming to walk nothing.
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

    fn repository(prefix: &str) -> tempfile::TempDir {
        let dir = scratch(prefix);
        git_in(dir.path(), &["init", "-q"]);
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

    /// One commit at a committer date this test chose.
    ///
    /// **Forged rather than taken from the clock, because the subject of half the tests below is an
    /// ORDER between timestamps** and several commits made in a row land in the same second — which
    /// would leave the assertions passing on the tie-break they exist to tell apart from the sort.
    /// `GIT_COMMITTER_DATE` is what `%ct` reports; `GIT_AUTHOR_DATE` goes with it so the two never
    /// disagree and mislead whoever reads the fixture with `git log` in hand.
    fn commit_at(root: &Path, message: &str, when: i64) {
        git_in(root, &["add", "-A"]);
        let stamp = format!("{when} +0000");
        let status = Command::new("git")
            .arg("-C")
            .arg(root)
            .args(["commit", "-q", "-m", message])
            .env("GIT_COMMITTER_DATE", &stamp)
            .env("GIT_AUTHOR_DATE", &stamp)
            .status()
            .expect("git should start");
        assert!(status.success(), "commit {message} failed");
    }

    /// A decision naming these anchor files, and nothing else this module reads.
    ///
    /// `ordinal` and `decision_id` are the same number so that a failure prints ids whose position
    /// in `map_store::approved`'s order is readable straight off them — which is the tie-break
    /// every assertion here is measured against.
    fn decision(id: i64, modules: &[&str]) -> Anchored {
        Anchored {
            decision_id: id,
            ordinal: id,
            spec_slug: "a-spec-design".to_owned(),
            section: "## 1 Uma secção".to_owned(),
            text: format!("decision {id}"),
            kind: Kind::Character,
            anchor: if modules.is_empty() {
                Anchor::Silent
            } else {
                Anchor::Ambiguous
            },
            modules: modules.iter().map(|path| (*path).to_owned()).collect(),
            foreign: Vec::new(),
        }
    }

    fn ids(decisions: &[Anchored]) -> Vec<i64> {
        decisions.iter().map(|one| one.decision_id).collect()
    }

    /// Three files, each last touched in its own commit. A day apart, so a window that leaves one
    /// outside does so with no argument about clock resolution.
    const OLD: i64 = 1_700_000_000;
    const MIDDLE: i64 = 1_700_086_400;
    const RECENT: i64 = 1_700_172_800;

    fn three_files(prefix: &str) -> tempfile::TempDir {
        let repo = repository(prefix);
        let root = repo.path();
        write(root, "core/src/slow.rs", "//! §1 barely touched\n");
        commit_at(root, "slow", OLD);
        write(root, "core/src/middle.rs", "//! §1 sometimes\n");
        commit_at(root, "middle", MIDDLE);
        write(root, "core/src/quick.rs", "//! §1 often\n");
        commit_at(root, "quick", RECENT);
        repo
    }

    #[tokio::test]
    async fn the_order_is_by_the_most_recent_of_a_decision_s_anchors() {
        let repo = three_files("nucleos-recency-order-");
        let walked = walk_within(repo.path(), 10).await;

        // Handed over in the worst order for the assertion on purpose: `map_store::approved` sorts
        // by `spec_slug, ordinal, id`, so 1, 2, 3 is exactly what a route that had not been wired to
        // this would leave on the screen, and 3, 2, 1 is what §10 asks for.
        let mut decisions = vec![
            decision(1, &["core/src/slow.rs"]),
            // The one that matters: its newest anchor is the newest file in the repository and its
            // oldest is the oldest, so a `min` — or a `first` over an unsorted anchor list — puts it
            // last instead of first.
            decision(2, &["core/src/slow.rs", "core/src/quick.rs"]),
            decision(3, &["core/src/middle.rs"]),
        ];
        let recency = order(&mut decisions, &walked);

        assert_eq!(ids(&decisions), vec![2, 3, 1], "{recency:?}");
        assert_eq!(recency.window, Some(10));
        assert_eq!(recency.ages.get(&2), Some(&Age::Moved { at: RECENT }));
        assert_eq!(recency.ages.get(&3), Some(&Age::Moved { at: MIDDLE }));
        assert_eq!(recency.ages.get(&1), Some(&Age::Moved { at: OLD }));
    }

    #[tokio::test]
    async fn a_decision_with_no_anchors_sorts_last_rather_than_as_if_it_never_changed() {
        let repo = three_files("nucleos-recency-unanchored-");
        let walked = walk_within(repo.path(), 10).await;

        let mut decisions = vec![
            // First in `approved`'s order, so nothing but the rule below can move it.
            decision(1, &[]),
            decision(2, &["core/src/slow.rs"]),
            decision(3, &["core/src/quick.rs"]),
        ];
        let recency = order(&mut decisions, &walked);

        assert_eq!(ids(&decisions), vec![3, 2, 1], "{recency:?}");

        // **The two facts must not read alike**, which is the whole of this test. *Nothing moved
        // because there is nothing to move* is §5.1's *declarado, sem código*, a pile that exists to
        // be looked at; *nothing moved lately* is a quiet file. One label over both would bury the
        // first among the second.
        assert_eq!(recency.ages.get(&1), Some(&Age::Unanchored));
        assert_ne!(recency.ages.get(&1), recency.ages.get(&2));
    }

    #[tokio::test]
    async fn a_path_older_than_the_window_sorts_with_the_others_older_than_the_window() {
        let repo = three_files("nucleos-recency-window-");

        // A window of one commit: `quick.rs` is inside it, `middle.rs` and `slow.rs` are both
        // outside — a day apart in the repository, one age here.
        let walked = walk_within(repo.path(), 1).await;

        let mut decisions = vec![
            decision(1, &["core/src/middle.rs"]),
            decision(2, &["core/src/slow.rs"]),
            decision(3, &["core/src/quick.rs"]),
            decision(4, &[]),
        ];
        let recency = order(&mut decisions, &walked);

        // Inside the window first; then the two outside it, in the order they arrived rather than
        // in an order git was never asked for; then the one with nothing to move.
        assert_eq!(ids(&decisions), vec![3, 1, 2, 4], "{recency:?}");
        assert_eq!(recency.ages.get(&1), Some(&Age::Older));
        assert_eq!(recency.ages.get(&2), Some(&Age::Older));
        assert_eq!(recency.ages.get(&3), Some(&Age::Moved { at: RECENT }));

        // And how far the walk could see reaches the caller, so the panel can say *ordered by what
        // moved in the last N commits* rather than implying the sort is total. Two of these four
        // rows are ordered by nothing at all.
        assert_eq!(recency.window, Some(1));
    }

    #[tokio::test]
    async fn two_readings_of_an_unchanged_repository_agree() {
        let repo = three_files("nucleos-recency-stable-");
        let root = repo.path();

        // Six decisions of which four tie: two outside a one-commit window and two with no anchor
        // at all. Ties are most of a real map — 83 of this repository's 112 anchor files fall inside
        // a 200-commit window and every decision behind the other 29 ties — so a sort that shuffled
        // them would change the shape of the screen between two reads of a repository nothing had
        // happened to.
        let build = || {
            vec![
                decision(1, &["core/src/slow.rs"]),
                decision(2, &[]),
                decision(3, &["core/src/middle.rs"]),
                decision(4, &["core/src/quick.rs"]),
                decision(5, &[]),
                decision(6, &["core/src/slow.rs"]),
            ]
        };

        let first_walk = walk_within(root, 1).await;
        let mut first = build();
        let first_recency = order(&mut first, &first_walk);

        let second_walk = walk_within(root, 1).await;
        let mut second = build();
        let second_recency = order(&mut second, &second_walk);

        assert_eq!(ids(&first), ids(&second), "{first_recency:?}");
        assert_eq!(first_recency, second_recency);

        // Not merely equal to each other — equal to `map_store::approved`'s order within each
        // group, which is the order the decisions arrived in. An unstable sort can be perfectly
        // deterministic and be neither.
        assert_eq!(ids(&first), vec![4, 1, 3, 6, 2, 5], "{first_recency:?}");
    }

    #[tokio::test]
    async fn one_walk_and_not_one_per_decision() {
        let repo = repository("nucleos-recency-one-walk-");
        let root = repo.path();

        // Twenty anchor files over four commits, and twenty-five decisions naming them. The obvious
        // implementation — `git log -1 --format=%ct -- <path>` per anchor — costs one spawn per
        // FILE, which is ~700 on this repository; the loop-per-decision variant costs one per
        // decision. Both pass every other test in this module, which is why this one asserts a cost
        // and not an answer.
        let files: Vec<String> = (0..20)
            .map(|which| format!("core/src/anchor-{which:02}.rs"))
            .collect();
        for (which, path) in files.iter().enumerate() {
            write(root, path, &format!("//! §1 anchor {which}\n"));
            if which % 5 == 4 {
                commit_at(root, &format!("batch {which}"), OLD + which as i64 * 100);
            }
        }

        let mut decisions: Vec<Anchored> = (0..25)
            .map(|id| {
                let named: Vec<&str> = files
                    .iter()
                    .skip(id as usize % 5)
                    .step_by(3)
                    .map(String::as_str)
                    .collect();
                decision(id, &named)
            })
            .collect();

        // Read before and after rather than against zero: this fixture builds its repository with
        // `std::process::Command`, which the counter does not see, and nothing says a later helper
        // will not go through `run_git` instead.
        let before = crate::git_exec::spawns::against(root);
        let walked = walk_within(root, 50).await;
        let recency = order(&mut decisions, &walked);

        assert_eq!(
            crate::git_exec::spawns::against(root) - before,
            1,
            "twenty-five decisions over twenty anchor files must cost ONE git process, and the \
             ordering itself must cost none"
        );

        // And that one walk answered, so the count above is not one spawn that failed and a list
        // left in the order it arrived — which would satisfy the assertion above perfectly.
        assert_eq!(recency.window, Some(50));
        assert!(
            recency
                .ages
                .values()
                .any(|age| matches!(age, Age::Moved { .. })),
            "{recency:?}"
        );
    }

    #[tokio::test]
    async fn a_folder_that_is_not_a_repository_orders_without_failing() {
        // §11's project added from outside: no repository, no citations, and a map that has to draw
        // itself anyway. `%TEMP%` is not inside a repository on this machine — measured, and
        // `map_stamp`'s `scratch` records where.
        let outside = scratch("nucleos-recency-no-repo-");
        let walked = walk_within(outside.path(), 10).await;

        let mut decisions = vec![
            decision(1, &["core/src/slow.rs"]),
            decision(2, &[]),
            decision(3, &["core/src/quick.rs"]),
        ];
        let recency = order(&mut decisions, &walked);

        // Falling back to `map_store::approved`'s order is fine; failing the map is not.
        assert_eq!(ids(&decisions), vec![1, 3, 2], "{recency:?}");

        // **`None` and not `Some(10)`**, because the panel's sentence would be a claim about a walk
        // that never happened: *ordered by what moved in the last 10 commits*, over a list ordered
        // by nothing of the sort.
        assert_eq!(recency.window, None);

        // And **`Unknown` rather than `Older`**, which is the same rule one level down: `Older` says
        // the walk looked and found nothing, and nothing looked. The decision with no anchors keeps
        // `Unanchored` regardless — that is a fact about the decision, exactly as `anchor_digests`
        // gives it `Computed("")` whatever became of the git call.
        assert_eq!(recency.ages.get(&1), Some(&Age::Unknown));
        assert_eq!(recency.ages.get(&2), Some(&Age::Unanchored));
    }

    /// The ambiguity [`RECORD_MARK`] exists for, on the two inputs that produce it.
    ///
    /// A file called `1700000000` at the repository root is a path that reads exactly like a commit
    /// header, and a merge commit prints no paths at all — which defeats the other obvious rule,
    /// *a line followed by a blank one is a header*. Both are cheap to assert here and expensive to
    /// meet on a live map, where the symptom is a handful of decisions sorted as though a file
    /// nobody touched had just moved.
    #[test]
    fn a_file_named_like_a_timestamp_is_a_path_and_not_a_commit() {
        let text = concat!(
            "\u{1e}1700172800\n",
            "\n",
            "1700000000\n",
            "core/src/quick.rs\n",
            "\u{1e}1700086400\n",
            "\n",
            "\u{1e}1700000000\n",
            "\n",
            "core/src/slow.rs\n",
        );
        let read = parse(text);

        assert_eq!(read.get("1700000000"), Some(&1_700_172_800));
        assert_eq!(read.get("core/src/quick.rs"), Some(&1_700_172_800));
        assert_eq!(read.get("core/src/slow.rs"), Some(&1_700_000_000));
        assert_eq!(read.len(), 3, "{read:?}");
    }

    /// A path in two commits is dated by the newer of them, whichever order the walk hands them
    /// over in.
    #[test]
    fn a_path_touched_twice_carries_the_more_recent_of_the_two() {
        let newest_first = parse(concat!(
            "\u{1e}1700172800\n\ncore/src/quick.rs\n",
            "\u{1e}1700000000\n\ncore/src/quick.rs\n",
        ));
        let oldest_first = parse(concat!(
            "\u{1e}1700000000\n\ncore/src/quick.rs\n",
            "\u{1e}1700172800\n\ncore/src/quick.rs\n",
        ));

        assert_eq!(newest_first.get("core/src/quick.rs"), Some(&1_700_172_800));
        assert_eq!(newest_first, oldest_first);
    }
}
