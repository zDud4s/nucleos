//! Shell-command recognition and queueability classification.

use super::*;

/// PURE: the merge a shell command asks for, in the queue's own terms, or `None`.
///
/// `git merge X`, in a worktree whose HEAD is on `B`, means "bring X into B" — which is
/// `Merge { source: X, target: B }`, the shape the queue already executes. The queue performs it in
/// the integration worktree and moves the holder's worktree afterwards when the holder is the one
/// standing on `B`, which is the case this whole pillar was written around.
///
/// **The strictest reading that still admits what the queue performs: `git merge <ref>`, with an
/// optional `--no-ff` on either side of the ref, and nothing else.** Not because more could not be
/// parsed, but because everything else is a DIFFERENT operation: `--squash` does not merge at all;
/// `--abort` unwinds one; a second ref is an octopus merge; `--ff` and `--ff-only` both ask for a
/// fast-forward where this queue always writes a merge commit. A caller whose spelling is not one of
/// these keeps exactly the behaviour it has always had, rather than having the queue perform
/// something adjacent to what it wrote.
///
/// **`--no-ff` was in that rejected list, and it was there on a false premise** — worth recording,
/// because the sentence read true and the mistake cost the pillar its whole point for that spelling.
/// It said `--no-ff` wants a merge commit while `publish` is `--ff-only`, which conflates two
/// different things: `compute_merge` runs `git merge --no-ff` unconditionally, so the commit this
/// queue publishes is ALWAYS a merge commit, and `publish`'s `--ff-only` fast-forwards the holder's
/// checkout ONTO that already-computed commit. `--no-ff` is therefore not adjacent to what the queue
/// does — it is a literal spelling of it. Refusing it sent the merge back to be performed by the
/// agent's own hand, which is the one outcome this pillar exists to abolish.
///
/// **This is not the shell parsing `classifier.rs` exists to keep closed, and the difference is
/// where the output goes.** Nothing here reaches an argv: both names pass through `Branch` — the
/// argv guard — and `git_exec` builds its own command line from the typed value. The worst a
/// misreading can do is refuse, or name a branch git will not resolve; it cannot inject. That is
/// also why the verb is folded for comparison and the REF is not: git is case-sensitive about
/// branch names and this must not quietly rename one.
///
/// A `target` of `HEAD` is refused rather than passed on. It is what `rev-parse --abbrev-ref` says
/// for a detached HEAD, and it is meaningless as a merge target besides — the integration worktree's
/// own HEAD is always detached, so publishing "into HEAD" names nothing.
/// PURE: the command segments a shell string holds, in order, with any leading environment
/// assignments stripped.
///
/// **Every parser below matches the WHOLE token list as an exact shape, which is right for what
/// they are and wrong for what they were being handed.** `merge_from_command` accepts
/// `[program, subcommand, source]` and nothing longer — a deliberate strictness, argued at length
/// in its own comment, so that a spelling the queue cannot perform keeps working directly instead
/// of becoming impossible. But `cd repo && git merge feature` is not a different spelling of merge.
/// It is the same operation with a shell in front of it, and the exact-shape match cannot see it:
/// five tokens, no match, `None` — and `session_git_decision` reads `None` as "not mine", which is
/// an ALLOW. Every guarantee the queue makes was one `cd` away from being optional.
///
/// Splitting here rather than loosening the parsers keeps that strictness intact: each segment is
/// still matched as a whole shape, there are just more of them. A segment that is not a git command
/// matches nothing, exactly as an unrecognised command does today.
///
/// **Quoted text and heredoc bodies are data, not commands, and this used to split inside both.**
/// The paragraph that stood here said the opposite and defended it: *"Quoting is ignored, so
/// `echo "a; git merge x"` yields a segment that parses as a merge and is refused. That direction is
/// the safe one — the refusal is a sentence a person can reword, and the caller cannot approve
/// anything with it."*
///
/// **The second half of that sentence was never true, and 2026-08-27 is what it cost.**
/// `hooks::session_git_decision` does not merely refuse: it **submits the operation to the queue**
/// and *then* answers `deny` with the ticket number. So a caller could not approve anything with the
/// refusal, and did not need to — the queue already had the request. A session writing a file with
/// `python - <<'EOF' … EOF`, whose body happened to contain the words of a merge command as
/// documentation for another agent, had that merge performed against the branch its shell was
/// standing on. It was `master`, and 1016 lines of unreviewed work were published into it seconds
/// after the heredoc was typed. Nothing was executed by the shell; the text was never a command.
///
/// So the scan below tracks two things and nothing else, and both are shell facts rather than
/// heuristics:
///
/// - **Quotes.** A `;` inside `'…'` or `"…"` does not separate commands, so it is not a split point.
///   Skipping quoted spans cannot open a bypass, because quoted text is not run: a caller wanting a
///   merge out of a string still has to spell `sh -c "…"`, whose own segment is `sh` and matches no
///   parser here today either.
/// - **Heredoc bodies.** Everything between `<<WORD` and the terminator line is stdin for the
///   command, not shell input, so no line of it is ever a segment. `<<-WORD` strips leading tabs
///   from the terminator; `<<<` is a here-string and opens no body at all.
///
/// **Still not a shell parser, and it must not become one.** It does not expand anything, does not
/// understand backslash escapes, and does not follow `$(…)`. What it now refuses to do is invent a
/// command out of prose — which is a smaller claim than parsing shell, and the only one this needed.
/// **The grammar moved.** Everything the comment above describes — the quote tracking, the heredoc
/// bodies, the here-string that must not be read as an opener — now lives in `command_reader`,
/// which is the single reader this daemon has for a shell line. It moved because there were TWO,
/// with opposite policies: this one followed quotes and the classifier's did not, and the blind one
/// decided whether an autonomous run could continue. On 2026-08-29 it stopped four of them.
///
/// The queue keeps the reading that never refuses, and that is a deliberate asymmetry rather than a
/// leftover: a caller asking whether a line CONTAINS a merge most needs to keep scanning the line it
/// cannot fully read, because handing back nothing there would read as "no merge here".
pub fn shell_segments(command: &str) -> Vec<&str> {
    crate::command_reader::segments(command, crate::command_reader::Shell::Posix)
}
/// Where a segment's git command runs, as the line itself says it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SegmentDir {
    /// Nothing earlier in the line moved it: the session's own working directory.
    Session,
    /// Directories to apply in order, starting from the session's working directory: every
    /// `cd <dir>` before the segment, then the segment's own `git -C <dir>`.
    Steps(Vec<String>),
    /// The line moves the directory in a way this reader does not follow; holds the spelling.
    Unfollowable(String),
}
/// A shell segment with its `git -C <dir>` lifted out, and the directory it runs in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectedSegment {
    pub dir: SegmentDir,
    /// The segment as the parsers read it: `git -C x merge y` arrives here as `git merge y`.
    pub command: String,
}
/// PURE: `shell_segments`, plus the directory each one runs in.
///
/// **The queue's parsers name the branch a merge moves from where the command RUNS, and that used
/// to be read from the session's working directory alone.** `cd <worktree> && git merge master`,
/// typed from a session rooted in the main checkout (on `master`), queued `master into master` — a
/// no-op the hook then reported as performed — instead of merging into the worktree's branch.
/// `git -C <worktree> merge master` was refused outright, because no parser reads `-C`. So an agent
/// could not merge master into its own branch from a session rooted anywhere else.
///
/// Both spellings are now followed: every `cd`/`pushd`/`chdir`/`Set-Location` with exactly one
/// plain target, and a run of `-C <dir>` right after `git`. Anything else that moves the directory
/// (bare `cd`, `cd -`, `popd`, a flag, a variable, `~`, a glob, a subshell) is `Unfollowable`, and
/// the caller refuses a queue operation behind it rather than guess — guessing wrong performs the
/// operation on a branch nobody named.
pub fn directed_segments(command: &str) -> Vec<DirectedSegment> {
    let mut steps: Vec<String> = Vec::new();
    let mut lost: Option<String> = None;
    let mut out = Vec::new();
    for segment in shell_segments(command) {
        let tokens: Vec<&str> = segment.split_whitespace().collect();
        let Some(first) = tokens.first() else {
            continue;
        };
        let moves = |token: &str| {
            matches!(
                token.to_ascii_lowercase().as_str(),
                "cd" | "chdir" | "pushd" | "popd" | "set-location" | "sl"
            )
        };
        let bare = first.to_ascii_lowercase();
        // A directory change that is not the segment's own program — `{ cd x`, `if cd x`,
        // `then cd x` — still moves the shell, and this reader does not follow shell grammar.
        if !moves(first)
            && !first.eq_ignore_ascii_case("git")
            && tokens.iter().any(|token| moves(token))
        {
            lost = Some(format!("`{}`", segment.trim()));
            continue;
        }
        if moves(first) {
            // A subshell's `cd` does not outlive it, and `shell_segments` splits on the parens
            // without saying where they were — so in a line holding any paren at all, no `cd` is
            // followed. Refusing a line that merely quotes one is the cheap direction to be wrong.
            let target = match tokens.as_slice() {
                [_, target] if bare != "popd" && !command.contains(['(', ')']) => {
                    followable_dir(target)
                }
                _ => None,
            };
            match target {
                Some(target) => steps.push(target),
                None => lost = Some(format!("`{}`", segment.trim())),
            }
            continue;
        }

        let mut own_steps = Vec::new();
        let mut parsed = segment.trim().to_owned();
        let mut own_lost = None;
        if first.eq_ignore_ascii_case("git") && tokens.get(1) == Some(&"-C") {
            let mut index = 1;
            while tokens.get(index) == Some(&"-C") {
                match tokens.get(index + 1).and_then(|dir| followable_dir(dir)) {
                    Some(dir) => own_steps.push(dir),
                    None => own_lost = Some(format!("`{}`", segment.trim())),
                }
                index += 2;
            }
            parsed = std::iter::once(*first)
                .chain(tokens.get(index..).unwrap_or(&[]).iter().copied())
                .collect::<Vec<_>>()
                .join(" ");
        }

        let dir = match lost.clone().or(own_lost) {
            Some(how) => SegmentDir::Unfollowable(how),
            None if steps.is_empty() && own_steps.is_empty() => SegmentDir::Session,
            None => SegmentDir::Steps(steps.iter().cloned().chain(own_steps).collect()),
        };
        out.push(DirectedSegment {
            dir,
            command: parsed,
        });
    }
    out
}
/// PURE: a `cd`/`-C` target this reader can follow without a shell, unquoted, or `None`.
pub(super) fn followable_dir(raw: &str) -> Option<String> {
    let unquoted = match raw.as_bytes() {
        [b'"', .., b'"'] | [b'\'', .., b'\''] if raw.len() >= 2 => &raw[1..raw.len() - 1],
        _ => raw,
    };
    let refused = unquoted.is_empty()
        || unquoted.starts_with(['-', '~'])
        || unquoted.contains([
            '$', '`', '*', '?', '(', ')', '{', '}', '<', '>', '"', '\'', '%', '!',
        ]);
    (!refused).then(|| unquoted.to_owned())
}
/// PURE: `/c/Projects/x` (Git Bash's spelling of a drive) as `C:/Projects/x`, or `None` when the
/// path is not one. Applied on Windows only, where a native `Path` reads the MSYS spelling as a
/// drive-relative path and lands somewhere else.
pub fn msys_drive_path(raw: &str) -> Option<String> {
    let rest = raw.strip_prefix('/')?;
    let mut chars = rest.chars();
    let drive = chars.next().filter(char::is_ascii_alphabetic)?;
    let tail = chars.as_str();
    if !(tail.is_empty() || tail.starts_with('/')) {
        return None;
    }
    Some(format!(
        "{}:{}",
        drive.to_ascii_uppercase(),
        if tail.is_empty() { "/" } else { tail }
    ))
}
/// PURE: the directory `steps` lead to from `session`, each one absolute or relative to the last.
pub fn resolve_segment_dir(session: &std::path::Path, steps: &[String]) -> std::path::PathBuf {
    steps.iter().fold(session.to_path_buf(), |current, step| {
        let native = if cfg!(windows) {
            msys_drive_path(step).unwrap_or_else(|| step.clone())
        } else {
            step.clone()
        };
        current.join(native)
    })
}
pub fn merge_from_command(command: &str, current_branch: &str) -> Option<Op> {
    let tokens: Vec<&str> = command.split_whitespace().collect();
    // Matched as whole shapes rather than by filtering the flag out of the token list, and the
    // difference is what a caller can smuggle: a filter would turn `git --no-ff merge feature` —
    // which git itself refuses — into a queued merge, performing something nobody could have run.
    // Here the flag is only ever recognised in argument position, where it is the only thing it can
    // be. A `source` left holding `--no-ff` (`git merge --no-ff`) falls through to `Branch`, which
    // refuses a leading dash; that guard is load-bearing here and not merely nearby.
    let [program, subcommand, source] = match tokens.as_slice() {
        [program, subcommand, source]
        | [program, subcommand, "--no-ff", source]
        | [program, subcommand, source, "--no-ff"] => [program, subcommand, source],
        _ => return None,
    };
    if !program.eq_ignore_ascii_case("git") || !subcommand.eq_ignore_ascii_case("merge") {
        return None;
    }
    if current_branch.trim() == "HEAD" {
        return None;
    }
    Some(Op::Merge {
        source: Branch::new(source).ok()?,
        target: Branch::new(current_branch).ok()?,
    })
}
/// PURE: the push a shell command asks for, in the queue's own terms, or `None`.
///
/// **`git push <remote>` and `git push <remote> <branch>`, and nothing else** — the same strictness
/// posture `merge_from_command` argues at length, applied to an operation where being wrong is worse
/// because it is public. What that list leaves out is the interesting part, and each exclusion is a
/// different KIND of thing rather than a longer list of the same one:
///
/// - `git push` alone is refused, and it is the one that looks safest. It has no argv of its own —
///   what it does is read out of `push.default`, `branch.<name>.remote` and the upstream, in a
///   repository the daemon does not control. The queue would have to guess a destination, and the
///   spelling that means "the usual place" to the person who typed it means whatever their config
///   says to us.
/// - `-u` / `--set-upstream` is refused because the queue's argv would not do it. The push would
///   succeed, the upstream would not be set, and the row would say `succeeded` — a silent partial
///   execution, which is worse than a refusal that hands the command back.
/// - `--force`, `--force-with-lease`, `--delete`, `--tags`, `--all`, `--mirror` are refused because
///   each is a different operation, in the sense `merge_from_command` uses the word: they destroy or
///   move things this one only adds to. There is no `--no-ff`-shaped case here — no flag that is
///   merely a literal spelling of what the executor already does — so nothing is admitted beside the
///   bare shapes.
///
/// The branch is the command's own second word when it has one, and otherwise the branch the
/// worktree is standing on. `git push origin feature` from a worktree on `master` is a perfectly
/// ordinary thing to write and does not touch any worktree, so there is no reason to require the two
/// to agree — but `HEAD` is refused in both routes, whether it arrived from a detached worktree or
/// was typed, because a queued row naming `HEAD` names nothing by the time it runs.
pub fn push_from_command(command: &str, current_branch: &str) -> Option<Op> {
    let tokens: Vec<&str> = command.split_whitespace().collect();
    let (program, subcommand, remote, branch) = match tokens.as_slice() {
        [program, subcommand, remote] => (program, subcommand, remote, current_branch.trim()),
        [program, subcommand, remote, branch] => (program, subcommand, remote, *branch),
        _ => return None,
    };
    if !program.eq_ignore_ascii_case("git") || !subcommand.eq_ignore_ascii_case("push") {
        return None;
    }
    // Both routes, deliberately: `rev-parse --abbrev-ref` says `HEAD` for a detached worktree, and a
    // caller may equally have typed it. Neither is a branch this queue can push a week later.
    if branch == "HEAD" {
        return None;
    }
    Some(Op::Push {
        remote: Remote::new(remote).ok()?,
        branch: Branch::new(branch).ok()?,
    })
}
/// PURE: the tag a shell command asks for, in the queue's own terms, or `None`.
///
/// **`git tag <name>` and `git tag <name> <branch>`, and nothing else** — the third application of
/// the posture `merge_from_command` argues. The exclusions are worth naming individually because
/// they are not one kind of thing:
///
/// - **`git tag` alone is a READ.** It lists the repository's tags, and refusing it is not a
///   restriction: the fallback hands the run a grant and it lists them itself, which is the right
///   outcome for a command that changes nothing. This is the only one of the three parsers where the
///   bare two-token form means something entirely different from the operation, rather than meaning
///   it with the arguments left to config.
/// - **`-a`, `-s`, `-m` are refused because the queue would have to parse a quoted message**, and
///   not parsing quoted shell is the whole of `classifier.rs`'s reason to exist. A lightweight tag is
///   what this executes and an annotated one is a different object, not a decoration on the same one.
/// - **`-d`, `-f` are the destructive spellings.** One removes a tag and one moves an existing tag
///   to a new commit, which is the tag equivalent of a force push: it invalidates what anybody who
///   already fetched believes. The queue creates; it does not overwrite.
/// - **`-l`, `--list`, `--contains`, `-n` are reads wearing the write's name**, and they are stopped
///   by `TagName` rather than by this list — a leading dash cannot be a tag name. Named here anyway
///   because a reader checking whether they are handled should not have to derive it.
///
/// The branch is the command's own second word when it has one, and otherwise the branch the
/// worktree stands on — the same rule `push_from_command` uses, and `HEAD` is refused on both routes
/// for the same reason: it names nothing by the time a queued row runs.
pub fn tag_from_command(command: &str, current_branch: &str) -> Option<Op> {
    let tokens: Vec<&str> = command.split_whitespace().collect();
    let (program, subcommand, name, at) = match tokens.as_slice() {
        [program, subcommand, name] => (program, subcommand, name, current_branch.trim()),
        [program, subcommand, name, at] => (program, subcommand, name, *at),
        _ => return None,
    };
    if !program.eq_ignore_ascii_case("git") || !subcommand.eq_ignore_ascii_case("tag") {
        return None;
    }
    if at == "HEAD" {
        return None;
    }
    Some(Op::Tag {
        name: TagName::new(name).ok()?,
        at: Branch::new(at).ok()?,
    })
}
/// PURE: the rebase a shell command asks for, in the queue's own terms, or `None`.
///
/// `git rebase X`, in a worktree whose HEAD is on `B`, means "replay B onto X" — which is
/// `Rebase { branch: B, onto: X }`, the mirror of how `merge_from_command` reads its own two halves.
///
/// **Everything with a flag is refused, and unlike the other parsers there is no accepted one.**
/// `-i` opens an editor, which is a human sitting at a terminal that does not exist here.
/// `--continue`, `--abort` and `--skip` operate on a rebase already in progress — a state this queue
/// never leaves behind, since a conflicted compute aborts before the row is written. `--onto` takes a
/// third ref and means something the two-field shape cannot hold. `--exec` runs an arbitrary command
/// per commit, which is a shell by another name.
///
/// Bare `git rebase` is refused for `git push`'s reason: it replays onto the configured upstream, in
/// a repository the daemon does not control.
///
/// **Every rebase that arrives through this parser blocks, and that is structural rather than
/// incidental.** The branch is the worktree's own — this function has no other one to name — and a
/// run's worktree is precisely what holds it, so `GitExecutor`'s holder check finds the caller
/// itself and refuses. Measured end to end: an approved `git rebase master` in a run's worktree
/// wrote `blocked`, naming that worktree, and the takeover grant left the run unable to perform it
/// by hand either. That is the correct answer and not a gap — the branch the queue would rewrite is
/// the one the paused run resumes onto, and rewriting it underneath is what would corrupt the run.
/// The publishing half is reached from `POST /vcs/requests`, where a caller names a branch nobody
/// has open; that half is proven too, and moves the ref through `publish_by_update_ref`'s
/// compare-and-swap rather than through the rebase.
pub fn rebase_from_command(command: &str, current_branch: &str) -> Option<Op> {
    let tokens: Vec<&str> = command.split_whitespace().collect();
    let [program, subcommand, onto] = tokens.as_slice() else {
        return None;
    };
    if !program.eq_ignore_ascii_case("git") || !subcommand.eq_ignore_ascii_case("rebase") {
        return None;
    }
    if current_branch.trim() == "HEAD" {
        return None;
    }
    Some(Op::Rebase {
        branch: Branch::new(current_branch).ok()?,
        onto: Branch::new(onto).ok()?,
    })
}
/// PURE: the fetch a shell command asks for, in the queue's own terms, or `None`.
///
/// **`git fetch <remote>` exactly.** Bare `git fetch` is refused for `git push`'s reason and not for
/// `git tag`'s: it is not a read wearing the write's name, it is the operation with its destination
/// left to `branch.<name>.remote` and `remote.pushDefault` in a repository the daemon does not
/// control. `--all` fetches from remotes nobody named, `--prune` deletes tracking refs, and `--tags`
/// pulls in a namespace the refspec deliberately leaves out — three different operations, and the
/// argv guard stops none of them, so the shape has to.
pub fn fetch_from_command(command: &str) -> Option<Op> {
    let tokens: Vec<&str> = command.split_whitespace().collect();
    let [program, subcommand, remote] = tokens.as_slice() else {
        return None;
    };
    if !program.eq_ignore_ascii_case("git") || !subcommand.eq_ignore_ascii_case("fetch") {
        return None;
    }
    Some(Op::Fetch {
        remote: Remote::new(remote).ok()?,
    })
}
/// PURE: the branch deletion a shell command asks for, in the queue's own terms, or `None`.
///
/// **`-d` and `--delete`, never `-D` and never `--delete --force`**, and this is the one parser
/// where the accepted flag is mandatory rather than optional. `git branch <name>` CREATES a branch —
/// `classifier.rs` pins that exact ambiguity as the reason `git branch` is on its
/// `SAFE_EXACT_COMMANDS` list in its listing spellings only — so a shape that read the flag as
/// optional would queue a deletion for a command that asked for a creation.
///
/// The distinction between the two spellings is not stylistic: `-d` refuses when the branch holds
/// commits no other ref reaches, and `-D` deletes anyway. That refusal is git's, computed from the
/// commit graph, and it is the entire reason this operation can be offered at all — the queue never
/// has to decide what is safe to lose. `-D` asks git to stop answering that question, so it falls
/// back to the grant like every other unqueueable spelling.
pub fn branch_delete_from_command(command: &str) -> Option<Op> {
    let tokens: Vec<&str> = command.split_whitespace().collect();
    let [program, subcommand, flag, branch] = tokens.as_slice() else {
        return None;
    };
    if !program.eq_ignore_ascii_case("git") || !subcommand.eq_ignore_ascii_case("branch") {
        return None;
    }
    // Case-sensitive on purpose, and it is the only place in these parsers where that is load-
    // bearing rather than incidental: `-d` and `-D` differ by case alone and mean the safe and the
    // unsafe thing. Folding here would turn every `-D` into the spelling this queue accepts.
    if *flag != "-d" && !flag.eq_ignore_ascii_case("--delete") {
        return None;
    }
    Some(Op::BranchDelete {
        branch: Branch::new(branch).ok()?,
    })
}
/// Global git flags that swallow the token after them, so a verb scan does not read their VALUE as
/// the subcommand. `ask_daemon.py` has carried this list since it was written; the daemon did not,
/// and the asymmetry was a hole rather than a tidiness problem — `git -C <path> merge <branch>` put
/// `<path>` where the verb scan looks, matched no arm, and was ALLOWED by a function whose whole
/// job is to refuse that merge.
pub const GIT_FLAGS_WITH_VALUES: &[&str] = &[
    "-C",
    "-c",
    "--git-dir",
    "--work-tree",
    "--namespace",
    "--exec-path",
];
/// `git merge` spellings that write nothing another session can see, and so stay this route's
/// business to leave alone.
///
/// `--squash` stages instead of merging and `--no-commit` stops before the ref moves; `--abort`,
/// `--continue` and `--quit` operate on a merge already in progress, which is state local to the
/// caller's own worktree. `--ff-only` is the one that moves a shared ref and is listed anyway,
/// because leaving it passing was a decision taken deliberately and recorded — it is the spelling
/// that refuses rather than destroys when it cannot fast-forward. Revisiting it is a separate
/// question from closing the default, and folding the two would hide the second inside the first.
pub(super) const MERGE_CALLER_LOCAL: &[&str] = &[
    "--squash",
    "--no-commit",
    "--abort",
    "--continue",
    "--quit",
    "--ff-only",
];
/// `git rebase` spellings that only touch a rebase already in progress, or only read it.
pub(super) const REBASE_CALLER_LOCAL: &[&str] = &[
    "--abort",
    "--skip",
    "--quit",
    "--continue",
    "--edit-todo",
    "--show-current-patch",
];
/// `git tag` flags that create or destroy a ref under `refs/tags/`.
pub(super) const TAG_WRITE_FLAGS: &[&str] = &[
    "-a",
    "-s",
    "-d",
    "-f",
    "-m",
    "-F",
    "-u",
    "--annotate",
    "--sign",
    "--delete",
    "--force",
    "--message",
    "--file",
    "--local-user",
];
/// `git tag` flags that only ever list. `git tag` with none of either is the listing too.
pub(super) const TAG_LIST_FLAGS: &[&str] = &[
    "-l",
    "-n",
    "-i",
    "--list",
    "--contains",
    "--no-contains",
    "--points-at",
    "--merged",
    "--no-merged",
    "--sort",
    "--format",
    "--column",
    "--ignore-case",
];
/// PURE: whether any of `flags` appears in `tokens`, as a whole token or as `--flag=value`.
///
/// Exact token equality rather than a prefix test, and case-sensitive, for the reason
/// `branch_delete_from_command` spells out: the flags that decide these questions differ from their
/// dangerous twins by case alone (`-d` and `-D`) or by a suffix. The `--flag=value` arm is there
/// because `--sort=x` and `--message=x` are the same flags written the other way, and a reader that
/// saw only the spaced form would call a tag creation a listing.
pub(super) fn carries(tokens: &[&str], flags: &[&str]) -> bool {
    tokens.iter().any(|token| {
        flags.iter().any(|flag| {
            *token == *flag
                || (flag.starts_with("--")
                    && token.starts_with(flag)
                    && token.as_bytes().get(flag.len()) == Some(&b'='))
        })
    })
}
/// PURE: whether a `git tag` spelling the parser declined would still write a ref.
///
/// The listing spellings were never this queue's business and must keep working — bare `git tag`,
/// and every `-l`/`--list` form. What is left names a tag to create or delete, and `refs/tags/` is
/// fetched by every other session.
pub(super) fn tag_command_writes(args: &[&str]) -> bool {
    if carries(args, TAG_WRITE_FLAGS) {
        return true;
    }
    if carries(args, TAG_LIST_FLAGS) {
        return false;
    }
    // A name with no flag is the lightweight tag `tag_from_command` declined on shape alone
    // (`git tag v1 <sha>`, whose `at` must be a branch). No name at all is the listing.
    args.iter().any(|token| !token.starts_with('-'))
}
/// PURE: whether a `git branch` argument list asks for a deletion, in any spelling git accepts.
///
/// Not `carries`, whose whole-token match is right where it is used and would be the hole here.
/// git's option parser takes short flags in clusters and a long option by any unambiguous prefix,
/// so `-df` is `-d -f` — a forced delete — and `--del` is `--delete`, and a whole-token test reads
/// neither as a deletion. The `-D` arm in `unqueueable_but_shared` matches `-D` alone, so until
/// this existed `-df` fell through to `_ => None` with every other spelling nobody listed.
///
/// A cluster counts only when every letter in it is one of `git branch`'s own short flags. That is
/// what keeps a value handed over after a space from reading as flags: `--sort -committerdate` is
/// a listing, and `-committerdate` holds a `d`.
pub(super) fn asks_to_delete(args: &[&str]) -> bool {
    const SHORT_FLAGS: &str = "acCdDfilmMqrtuv";
    args.iter().any(|token| {
        if let Some(long) = token.strip_prefix("--") {
            let name = long.split_once('=').map_or(long, |(name, _)| name);
            !name.is_empty() && "delete".starts_with(name)
        } else if let Some(cluster) = token.strip_prefix('-') {
            !cluster.is_empty()
                && cluster.chars().all(|letter| SHORT_FLAGS.contains(letter))
                && cluster.contains(['d', 'D'])
        } else {
            false
        }
    })
}
/// PURE: why a command the parsers declined must still not be run by hand, or `None`.
///
/// **"Declined by the queue" and "fine to run directly" are not the same sentence, and treating
/// them as one left the worst spelling of the worst verb wide open.** The session gate's rule was
/// that anything the six parsers refuse passes through untouched, and that rule is right for
/// `git merge --squash`: squash is a DIFFERENT operation, it stages instead of merging, and
/// refusing it would make it impossible rather than governed. It is wrong for `git push --force`,
/// which is the SAME operation in a worse spelling. Measured against the live gate before this
/// existed: `git push --force origin master`, bare `git push`, and `git pull origin master` all
/// came back `allow`.
///
/// The line drawn here is **does it write something other sessions share**. A push writes the
/// remote; a pull writes the remote-tracking refs and then moves the local branch; `git branch -D`
/// destroys a branch after asking git to stop answering whether that is safe. None of those can be
/// left to race with the queue that exists to order exactly them. What stays silent is what touches
/// only the caller's own worktree or index — `--squash`, `--abort`, `--continue` — and read-only
/// spellings, which were never this queue's business.
///
/// This is a refusal and NOT an admission: there is nothing to queue, because the queue does not
/// know how to perform the spelling either. The caller is told which spelling it does know.
pub fn unqueueable_but_shared(command: &str) -> Option<String> {
    let tokens: Vec<&str> = command.split_whitespace().collect();
    // **The segment must BEGIN with git, and that is what separates a command from a mention.**
    // Scanning for `git` anywhere inside it is what the first version did, and it refused this
    // feature's own commit message: segments split on newlines, so that `cd repo` and `git push` on
    // two lines are judged apart — which also turns a line of prose reading "git push --force was
    // allowed" into something shaped exactly like a command. Measured, not imagined: the commit
    // could not be written, and neither could the probe that found it.
    //
    // Requiring the program first costs nothing real. `cd x && git push` has already become two
    // segments by the time it arrives here, and the second begins with git. What it stops is every
    // quoted and narrated occurrence — most of them, in a repository whose commit messages argue
    // about git commands for a living.
    let program = tokens
        .first()?
        .replace('\\', "/")
        .rsplit('/')
        .next()?
        .to_ascii_lowercase();
    if program != "git" && program != "git.exe" {
        return None;
    }
    // Skipping the VALUE of a global flag, not merely the flag. `find(|t| !t.starts_with('-'))`
    // reads `git -C <path> merge <branch>` as the verb `<path>`, which matches no arm below and
    // was therefore allowed — the same fail-open this function exists to close, one token earlier.
    let mut rest = 1;
    let verb = loop {
        let candidate = tokens.get(rest)?;
        if GIT_FLAGS_WITH_VALUES.contains(candidate) {
            rest += 2;
            continue;
        }
        if candidate.starts_with('-') {
            rest += 1;
            continue;
        }
        break candidate.to_ascii_lowercase();
    };
    // Everything after the verb. Anchored here rather than at a fixed index so the flag checks
    // below read `git -C <path> merge --squash` the same way they read `git merge --squash`.
    let args = tokens.get(rest + 1..).unwrap_or(&[]);

    match verb.as_str() {
        "push" => Some(
            // The pointer to `--land` lives here because this is where a finishing agent arrives.
            // "Deliver this work" and "push it somewhere" are the same thought to most callers, and
            // a refusal that only says what is wrong sends them looking for another way to push.
            "a push writes the remote, which is the shared thing this queue orders — and this \
             spelling is not one it can perform. Use `git push <remote>` or \
             `git push <remote> <branch>`, which it queues. If what you actually want is to \
             deliver this branch, that is `nucleos-core --land`: it asks the queue to merge your \
             branch into the branch the project is on, in order, and reports conflicts rather than \
             leaving you to resolve them."
                .to_owned(),
        ),
        "pull" => Some(
            "a pull is a fetch and then a merge, and it moves your branch on the way. It is two \
             queue operations, not one: run `git fetch <remote>`, then `git merge <remote>/<branch>`."
                .to_owned(),
        ),
        // `-D` first, for the sentence it earns. The difference between it and `-d` is the whole
        // reason deletion is offerable at all: `-d` asks git to refuse when the branch holds commits
        // nothing else reaches, and `-D` asks git to stop answering that.
        // `-d -f` and `--delete --force` are `-D` written the long way, and git treats them as the
        // same request. Listing only the short spelling refused the careless form and admitted the
        // deliberate one.
        "branch"
            if args.contains(&"-D")
                || (carries(args, &["-d", "--delete"]) && carries(args, &["-f", "--force"])) =>
        {
            Some(
            "`-D` deletes a branch whose commits may be reachable from nowhere else, and it is the \
             spelling that asks git not to check. The queue performs `git branch -d <branch>`, where \
             git's own refusal is the safety."
                .to_owned(),
            )
        }
        // **Every other deletion, and this arm's absence was a hole.** The one above refused the
        // forced spellings and left the rest to `_ => None`, which `session_git_decision` reads as
        // "not mine" and ALLOWS. Observed 2026-09-14, from an editor session in the main checkout:
        // `git worktree remove <path> && echo "worktree removed" && git branch -d <branch> 2>&1 |
        // tail -6`. The line split as it should, and the deletion arrived as
        // `git branch -d <branch> 2>&1` — five tokens, where `branch_delete_from_command` reads
        // exactly four. No parser took it, this function had nothing to say, and the branch was
        // deleted by hand with no row in `vcs_requests`.
        //
        // Refused rather than read more generously, for the reason `merge_from_command` gives for
        // its own strictness: the parser's exact shape is what keeps `git branch <name>` — a
        // creation — from ever being queued as a deletion, and a looser second reader is a second
        // place for that to go wrong. The caller is handed the one spelling the queue does read.
        //
        // It answers for that spelling too, the way the arms below answer for theirs. The session
        // gate asks the parsers first and never brings a segment one of them read down here; the
        // classifier's confinement rule asks this function whether a segment is the queue's business
        // at all, and a readable deletion is.
        "branch" if asks_to_delete(args) => Some(
            "a branch deletion goes through the queue, and the queue reads one spelling of it: \
             `git branch -d <branch>`, one local branch, as a command of its own. This one carries \
             something that spelling does not — a redirection or a pipe after it, a `-C`, a second \
             name, a combined or abbreviated flag. Run `git branch -d <branch>` alone, as its own \
             command, and the queue will take it."
                .to_owned(),
        ),
        // **What the queue's parsers could not read is refused by default from here down.**
        //
        // The opposite default is what let this route's worst case through, and it was measured
        // after the fact rather than imagined: `merge_from_command` reads three exact token shapes,
        // the merge a person actually writes carries `-m "..."`, and a shape no parser recognised
        // left this function as `None` and was ALLOWED. An editor session ran
        // `git merge --no-ff <branch> -m "..."`; the hook fired, the daemon was asked, and the
        // daemon said yes. Nothing was broken — the guard answered the question it was asked, and
        // the question had a hole in it.
        //
        // So the arms below invert it for the four remaining queue verbs: the listed spellings are
        // the exceptions, and anything unlisted is refused. A spelling nobody anticipated now fails
        // closed, which is the direction this route already argues for everywhere else.
        "merge" if !carries(args, MERGE_CALLER_LOCAL) => Some(
            "a merge moves the branch you are standing on, and other sessions hold it — but this \
             spelling is not one the queue can perform. It queues `git merge <branch>` and `git \
             merge --no-ff <branch>`, and nothing carrying a message: the queue writes its own \
             (`merge <source> into <target>`), so accepting `-m` would drop yours in silence and \
             still report success. To deliver this branch, `nucleos-core --land` asks the queue to \
             merge it into the branch the project is on, in order."
                .to_owned(),
        ),
        "rebase" if !carries(args, REBASE_CALLER_LOCAL) => Some(
            "a rebase rewrites the branch you are standing on, which other sessions may already \
             hold, and the queue performs `git rebase <onto>` only. `-i` wants an editor nobody is \
             sitting at, `--onto` names a third ref the queue's two-field row cannot hold, and \
             bare `git rebase` replays onto an upstream configured in a repository the daemon does \
             not control."
                .to_owned(),
        ),
        "fetch" if !carries(args, &["--dry-run"]) => Some(
            "a fetch writes the remote-tracking refs every other session reads, and this spelling \
             is not one the queue can perform. It queues `git fetch <remote>` exactly — `--all` \
             fetches from remotes nobody named, `--prune` deletes tracking refs, and `--tags` \
             pulls in a namespace the refspec deliberately leaves out."
                .to_owned(),
        ),
        "tag" if tag_command_writes(args) => Some(
            "a tag writes a ref under `refs/tags/`, which every other session fetches, and this \
             spelling is not one the queue can perform. It queues the lightweight `git tag <name>` \
             and `git tag <name> <branch>`; `-a` and `-s` need a message, and a message is a \
             quoted shell argument these parsers exist to never read. Listing spellings are \
             untouched."
                .to_owned(),
        ),
        _ => None,
    }
}
