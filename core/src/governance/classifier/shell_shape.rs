//! Shell-text shape and output-safety predicates.

use super::*;

/// PURE: whether a token is a file-descriptor duplication — `2>&1`, `1>&2`, `2>&-`.
///
/// The distinction it draws is the whole reason it exists: a redirect whose right-hand side is a
/// NUMBER points one stream at another stream, and a redirect whose right-hand side is a WORD points
/// a stream at a file. `2>&1` is the first; `>&out.txt` and `&>out.txt` are the second and must keep
/// being refused, which is why both sides are checked and why an empty left side (`>&x`) fails here.
/// PURE: whether one command redirects to or from a FILE, as opposed to joining two streams.
///
/// Read per command and not per line, because `2>&1` is not separated from what follows it: in
/// `ls x 2>&1; echo y` the whitespace token is `2>&1;`, and no line-level rule can tell the stream
/// join from the semicolon glued to it without doing the segmentation first.
///
/// Everything that is not the exact `N>&M` shape counts, whether the `>` stands alone (`> out.txt`)
/// or is glued on (`2>out.txt`, `2>>out.txt`, `>&out.txt`). `<` has no stream-joining spelling worth
/// keeping, so it counts whole. `&>out.txt` never arrives here at all — the lone `&` refuses the
/// line one level up.
/// Read outside quotes for the reason `has_shell_control` is: a commit message is the likeliest
/// place in this repository for a `>` to appear as text, and `git commit -m "a > b"` is not a
/// redirect. The mask keeps the token structure, so the `N>&M` shapes below still read as tokens.
pub(super) fn redirects_a_file(segment: &str) -> bool {
    let Some(masked) = crate::command_reader::without_quoted_text(segment) else {
        return true;
    };
    masked.contains('<')
        || masked
            .split_whitespace()
            .any(|token| token.contains('>') && !touches_no_file(token))
}

/// PURE: whether a redirect token names something that is not a file — another stream, or the bit
/// bucket.
///
/// The two exceptions this file makes to "a `>` means a file write", and both are exceptions because
/// the TARGET is not a file rather than because the redirect is harmless.
pub(super) fn touches_no_file(token: &str) -> bool {
    is_fd_duplication(token) || discards_output(token)
}

/// PURE: whether a redirect throws its output away — `2>/dev/null`, `>NUL`.
///
/// About as common an idiom as shell has, and it was costing an approval every time: the job-10
/// dogfood parked its plan node on `cat greet.py 2>/dev/null | head -50`.
///
/// Both spellings are the null device on the platform this daemon builds for. `NUL` is reserved in
/// every directory on Windows, which also makes `/dev/null` resolve to the device rather than to a
/// file — so neither can create anything, on cmd or under the bundled bash. The left side must be
/// empty or a file descriptor number, for the same reason `is_fd_duplication` checks it: `foo>nul`
/// is a token whose leading part is a program, and this is not the place to be deciding about that.
pub(super) fn discards_output(token: &str) -> bool {
    let Some(marker) = token.rfind('>') else {
        return false;
    };
    let (left, target) = token.split_at(marker);
    let target = target.trim_start_matches('>');
    let left = left.trim_end_matches('>');
    left.bytes().all(|byte| byte.is_ascii_digit())
        && (target.eq_ignore_ascii_case("/dev/null") || target.eq_ignore_ascii_case("nul"))
}

pub(super) fn is_fd_duplication(token: &str) -> bool {
    let Some((left, right)) = token.split_once(">&") else {
        return false;
    };
    !left.is_empty()
        && left.bytes().all(|byte| byte.is_ascii_digit())
        && !right.is_empty()
        && (right == "-" || right.bytes().all(|byte| byte.is_ascii_digit()))
}

/// PURE: the same line with its stream joins and discards taken out, so nothing downstream sees a
/// `>` that was never going to reach a file.
///
/// Reading it token by token is what makes this safe to do so early: a `>` glued to a filename
/// (`2>out.txt`, `>&out.txt`) sits in a token that fails `is_fd_duplication`, survives here, and
/// goes on to refuse the line exactly as before.
///
/// **The whitespace between tokens is copied through untouched**, which is the whole reason this is
/// not a `split_whitespace().join(" ")`. `\n` and `\r` are statement separators that `shell_segments`
/// cuts on, and collapsing them is precisely how a second command once rode in behind a safe leading
/// token. One `2>&1` anywhere in the line would have brought that back.
pub(super) fn strip_fd_duplications(command: &str) -> String {
    if !command.contains('>') {
        return command.to_owned();
    }
    let mut stripped = String::with_capacity(command.len());
    let mut rest = command;
    while !rest.is_empty() {
        let gap = rest
            .find(|character: char| !character.is_whitespace())
            .unwrap_or(rest.len());
        stripped.push_str(&rest[..gap]);
        rest = &rest[gap..];

        let token_end = rest.find(char::is_whitespace).unwrap_or(rest.len());
        let token = &rest[..token_end];
        if !touches_no_file(token) {
            stripped.push_str(token);
        }
        rest = &rest[token_end..];
    }
    stripped
}

/// PURE: the one form every command and every declared prefix is compared in — runs of whitespace
/// collapsed to a single ASCII space, both ends trimmed, ASCII letters folded to lower case.
///
/// It does THAT and nothing else: no quote handling, no path work, no token rewriting, no
/// tokenization. Worth writing down now that it is `pub(crate)`, because `project_policy` pushes a
/// person's declared prefix through it — and a fold with a surprise in it would be a surprise
/// applied to a refusal.
///
/// The case fold is `to_ascii_lowercase`, not `to_lowercase`, so a non-ASCII capital survives it.
/// **Identical treatment of both sides is NOT what makes that safe**, and the sentence that used to
/// stand here said it was. Identical treatment only makes the two sides agree on what the fold IS;
/// what a deny list actually needs is that two spellings the operating system treats as the same
/// command fold to the same string. Measured: `deny = "çurl"` against `ÇURL http://x` comes back
/// `("pending_approval", "unrecognized")`, with both sides through this exact function.
///
/// It stays as it is for three reasons, and the first is the one that carries the other two:
///
/// 1. **A non-ASCII program name is not in the threat model.** Every program this daemon runs and
///    every entry on the compiled lists is ASCII; `çurl` is not a spelling of a program that exists.
/// 2. **`to_lowercase` would not close it either.** On Windows the case-insensitivity that makes
///    two spellings the same command comes from NTFS's uppercase table, not from Unicode simple
///    casing, and the two are not the same map — so the wider fold buys a different set of misses
///    rather than no misses.
/// 3. **Widening it would change the COMPILED path**, which is much larger than the declared one.
///    This feeds `matches_any_phrase(DESTRUCTIVE_COMMAND_PATTERNS)`, `has_destructive_flags`,
///    `APPROVAL_COMMAND_PATTERNS`, `is_safe_command` and `shell_form_is_readable` — and
///    `to_lowercase` can change a string's byte LENGTH (`İ` becomes `i` + U+0307), which is exactly
///    the shape of change that can move a `starts_with` or a `contains` verdict.
///
/// Exposed rather than reimplemented for a reason that survives all three: a second spelling of
/// "fold" is how the two sides would come to disagree about what the fold even is.
pub fn normalize_command(command: &str) -> String {
    command
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase()
}

pub(super) fn matches_any_phrase(command: &str, patterns: &[&str]) -> bool {
    let padded = format!(" {command} ");
    patterns
        .iter()
        .any(|pattern| padded.contains(&format!(" {pattern} ")))
}

/// PURE: the program a token actually names, with its directory and `.exe` taken off.
///
/// The match was against the token whole, so `rm -rf x` was denied and `/bin/rm -rf x` — the same
/// program, spelled the way a script spells it — was not.
pub fn program_name(token: &str) -> &str {
    // A subshell `(rm`, a PowerShell `(Remove-Item`, `$(rm`, `@(ri` and a script block `{rm` all
    // name the same program as the bare spelling; left on, the opener hid it from every list here.
    let token = token.trim_start_matches(['(', '{', '$', '@']);
    let base = token.rsplit(['/', '\\']).next().unwrap_or(token);
    base.strip_suffix(".exe").unwrap_or(base)
}

/// PURE: whether the arguments after an `rm` ask for a recursive force delete.
///
/// Accumulated across tokens rather than looked for within one. The old check wanted `r` and `f` in
/// the same argument, so `rm -rf x` was caught by the phrase list and `rm -r -f x` — one space
/// apart, identical to the shell — fell through to `pending_approval`, which asks a human to
/// approve the very thing the other spelling is denied for.
pub(super) fn rm_deletes_recursively_and_forcibly(rest: &[&str]) -> bool {
    let (mut recursive, mut forced) = (false, false);
    for token in rest {
        let Some(flag) = token.strip_prefix('-') else {
            continue;
        };
        match flag.strip_prefix('-') {
            // A long option is a whole word, not a bag of letters: `--force` is not `-r -f`.
            Some(long) => {
                recursive |= long == "recursive";
                forced |= long == "force";
            }
            // A short cluster is a bag. `-R` is the real GNU spelling too, and the command has
            // already been lowercased, so both cases are the same character here.
            None => {
                recursive |= flag.contains('r');
                forced |= flag.contains('f');
            }
        }
    }
    recursive && forced
}

/// PURE: whether a token is `-Recurse` or `-Force` as PowerShell would read it.
///
/// PowerShell accepts any unambiguous prefix of a parameter name, so `Remove-Item -rec -fo` is
/// `-Recurse -Force` and an exact-name match never saw it. Matching by prefix over-matches on
/// purpose: a spelling PowerShell would itself reject as ambiguous is not one worth waving through.
pub(super) fn is_powershell_delete_switch(token: &str) -> bool {
    match token.strip_prefix('-') {
        Some(name) if !name.is_empty() => "recurse".starts_with(name) || "force".starts_with(name),
        _ => false,
    }
}

/// Whether the command asks for a destructive delete, wherever in the line it says so.
///
/// Scanned from every position rather than only the first token, because `sudo rm -rf`,
/// `busybox rm -rf` and `xargs rm -rf` all put the real command in an argument. This widens the
/// over-match — a commit message quoting `rm -r -f` is now denied — but only to where the phrase
/// blocklist already was: it matches ` rm -rf ` anywhere in the line and always has.
pub(super) fn has_destructive_flags(command: &str) -> bool {
    let tokens: Vec<_> = command.split_whitespace().collect();
    tokens.iter().enumerate().any(|(index, token)| {
        let rest = &tokens[index + 1..];
        match program_name(token) {
            "rm" => rm_deletes_recursively_and_forcibly(rest),
            // Under PowerShell `rd`, `rmdir`, `del`, `erase` and `ri` are all aliases of
            // `Remove-Item`, so each one is read with cmd's switches AND PowerShell's: `ri -r -fo`
            // is the same delete as `Remove-Item -Recurse -Force`.
            "rd" | "rmdir" => {
                rest.contains(&"/s") || rest.iter().any(|token| is_powershell_delete_switch(token))
            }
            "del" | "erase" => rest
                .iter()
                .any(|token| matches!(*token, "/s" | "/q") || is_powershell_delete_switch(token)),
            "remove-item" | "ri" => rest.iter().any(|token| is_powershell_delete_switch(token)),
            // `find . -delete` IS a recursive force delete, spelled as a search. It belongs here
            // rather than merely off the allow list, because `deny` is what the identical `rm -rf`
            // gets and the two differ only in which program walks the tree.
            "find" => rest.contains(&"-delete"),
            _ => false,
        }
    })
}

/// Metacharacters that let a command do something other than what its leading token says.
///
/// Command substitution belongs here for the same reason `;` and `|` do, and is easy to miss
/// because it hides *inside* an argument rather than chaining after one: `$(...)` and backticks run
/// a nested command first, so `ls $(rm -rf ~)` is an `rm`, not an `ls`. Neither guard upstream
/// catches it — the safe-prefix match only ever inspects the leading token, and the phrase
/// blocklist pads with spaces, so the `rm` in `$(rm -rf ~)` sits behind a `(` and never matches
/// " rm -rf ". Both PowerShell and POSIX shells read both spellings, and backtick is additionally
/// PowerShell's escape character, so neither is safe to wave through on either platform.
///
/// `$` alone is deliberately not here: bare `$VAR` expands to an argument rather than executing,
/// so refusing it would cost ordinary commit messages without closing anything.
///
/// Now a backstop rather than the front door, and the split is worth knowing when reading this file
/// top to bottom. `classify_shell_command` reaches `is_safe_command` only through the command
/// reader, which refuses `$(`, backticks and a lone `&` and cuts the line at every separator, and
/// then through `classify_segment`, which refuses `<` and every `>` that could reach a file and
/// strips the ones that could not. It stays because `is_safe_command` is a predicate about a
/// command rather than about a segment, and the day something else calls it with a whole line the
/// guard should be there.
///
/// **Read outside quotes, since 2026-08-30, and that correction is the whole point of the reader.**
/// The walk keeps a quoted `|` inside its argument because a shell does; a `contains` over the raw
/// segment then found it anyway and refused. `grep -n "^mod \|^pub mod " core/src/main.rs` is the
/// measured case — an alternation in a pattern, nine seconds into run 900391 — and there is nothing
/// to decide about it. Substitution is the exception and is still read RAW: `"$(whoami)"` and a
/// backtick both run a nested command from inside double quotes, so masking would hide them.
pub(super) fn has_shell_control(command: &str) -> bool {
    if command.contains('`') || command.contains("$(") {
        return true;
    }
    // An unterminated quote is the one line whose extent cannot be proved, and it masks to a line
    // with no metacharacters left in it. Refuse instead.
    let Some(masked) = crate::command_reader::without_quoted_text(command) else {
        return true;
    };
    const SHELL_CONTROL: &[char] = &[';', '|', '&', '>', '<', '\n', '\r'];
    masked.contains(SHELL_CONTROL)
}

pub(super) fn is_safe_command(command: &str) -> bool {
    shell_form_is_readable(command)
        && (SAFE_EXACT_COMMANDS.contains(&command)
            || matches_command_prefix(command, SAFE_COMMAND_PREFIXES)
            || checks_formatting_without_writing(command)
            || prints_lines_by_address(command))
}

/// PowerShell's output shapers — `Select-Object -First 100`, `Sort-Object CPU`, `Format-Table` —
/// which is how a pipeline is cut and sorted there, the way `head` and `sort` are here.
///
/// Spec A D13. Refused as soon as a script block, a subexpression, a variable or a splat appears:
/// `Select-Object @{e={...}}` and `Sort-Object { ... }` RUN the block for every row, which makes
/// them `Invoke-Expression` in disguise. `Where-Object` is deliberately absent for the same
/// reason — its whole grammar is a script block — and so is its alias `where`, which in cmd and
/// Git bash is a harmless lookup and in PowerShell is that same filter.
///
/// Kept out of `is_safe_command` because that function serves the shell-agnostic lists, and a
/// PowerShell-only rule there would force every caller to pass the shell. `sleep`/`Start-Sleep`
/// are absent on purpose: an unbounded pause holds the run like a `tail -f`, which the classifier
/// already refuses for the same reason.
pub(super) fn shapes_output_only(command: &str) -> bool {
    const SHAPERS: &[&str] = &[
        "select-object",
        "sort-object",
        "measure-object",
        "format-table",
        "format-list",
        "format-wide",
        "ft",
        "fl",
    ];
    shell_form_is_readable(command)
        && command
            .split_whitespace()
            .next()
            .is_some_and(|program| SHAPERS.contains(&program))
        && !command.contains(['{', '(', '$', '@'])
}

/// `sed -n` whose script only prints lines by their address — `'1,40p'`, `'5p'`, `'$p'`,
/// `'10,$p'` — which is `head` and `tail` in another spelling.
///
/// Measured on job 27, 2026-09-14: an implement node was refused
/// `cd "<worktree>/core" && sed -n '1,40p' Cargo.toml`, a read of the first forty lines of a
/// manifest. `sed` stays off the prefix list for the reasons written there — `-i` edits in place,
/// `w` writes, `e` executes — and this admits the one spelling that can do none of them. It is a
/// grammar rather than a blocklist: `-n`, one script of one or two addresses and a `p`, then files.
/// No other flag in any position (GNU takes options after operands, so `sed -n 1p f -i` edits in
/// place), so no `-e`, no `-f` and no `--in-place`; no second command; no regex address.
///
/// `$` is an address only inside single quotes. Anywhere else the shell expands `$p` before sed
/// ever sees it, and the script becomes whatever that variable holds.
///
/// Read off the normalized command, as the lists are: lowercasing folds `P` into `p`, and `P`
/// only prints as well.
pub(super) fn prints_lines_by_address(command: &str) -> bool {
    let mut tokens = command.split_whitespace();
    if tokens.next() != Some("sed") {
        return false;
    }
    let mut quiet = false;
    let mut script = None;
    for token in tokens {
        match token {
            "-n" | "--quiet" | "--silent" => quiet = true,
            _ if token.starts_with('-') => return false,
            _ if script.is_none() => script = Some(token),
            _ => {}
        }
    }
    quiet && script.is_some_and(prints_by_address)
}

/// PURE: whether one `sed` script is a single `p` by one or two line addresses.
pub(super) fn prints_by_address(script: &str) -> bool {
    let (body, dollar_is_literal) = match script
        .strip_prefix('\'')
        .and_then(|inner| inner.strip_suffix('\''))
    {
        Some(inner) => (inner, true),
        None => (
            script
                .strip_prefix('"')
                .and_then(|inner| inner.strip_suffix('"'))
                .unwrap_or(script),
            false,
        ),
    };
    let address = |value: &str| {
        (!value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()))
            || (value == "$" && dollar_is_literal)
    };
    let Some(range) = body.strip_suffix('p') else {
        return false;
    };
    match range.split_once(',') {
        Some((from, to)) => address(from) && address(to),
        None => address(range),
    }
}

/// `cargo fmt` with `--check` anywhere in it: rustfmt reports what it would change and writes
/// nothing.
///
/// A rule rather than a list entry, because the list matches on a prefix and this flag moves. The
/// gate runs `cargo fmt --all -- --check` (`scripts/gates.sh`) while the list held only
/// `cargo fmt --check`, so the one spelling the gate judges a node by was the one a node could not
/// run. Measured on job 26, 2026-09-13: refused four times across its implement, retry and review
/// nodes, and its first round went red on `core: fmt` alone with every test passing.
///
/// `--emit` is refused beside it: it is how rustfmt is told where to write, and a check has no
/// business naming a destination. Plain `cargo fmt` rewrites the tree, so it is not this rule's to
/// answer: since 2026-09-14 it is judged by where it lands (`lands_inside_the_workspace`), and
/// `mutating_siblings_remain_pending` pins it asking where there is no workspace to land in.
///
/// **It replaced the list's `cargo fmt --check` entry rather than joining it.** The lists are read
/// as an OR, so the entry let `cargo fmt --check -- --emit files` through whatever the guard here
/// said, and the test pinning that refusal went red the first time it ran. One place decides
/// `cargo fmt` now.
pub(super) fn checks_formatting_without_writing(command: &str) -> bool {
    let mut tokens = command.split_whitespace();
    tokens.next().map(program_name) == Some("cargo")
        && tokens.next() == Some("fmt")
        && command.split_whitespace().any(|token| token == "--check")
        && !command
            .split_whitespace()
            .any(|token| token.starts_with("--emit"))
}

/// The guards a command has to clear before ANY list may say yes to it, separated from the lists
/// themselves.
///
/// Split out when the GitHub policy became a second list, and the split is the point: a command
/// named in `~/.nucleos/github.yaml` clears exactly the same shape guards a compiled entry does. Had that
/// path grown its own conjunction the two would have drifted, and the one an owner edits is the one
/// that would have ended up shorter.
///
/// Every clause only ever REFUSES, so an overlap between them costs a redundant check and a gap
/// costs an allowed `-exec` - which is why `find_executes_or_writes` sits beside the two flag guards
/// rather than inside them.
///
/// Visible to the crate because `POST /projects/{id}/shell-rules` asks it of a prefix somebody is
/// about to WRITE DOWN rather than of a command about to run — and asks the same function the
/// decision asks, so the two cannot come to disagree about which prefixes are worth having.
///
/// **That route asks it of an `allow` and never of a `deny`, and this doc is the place the
/// asymmetry has to be right.** These guards sit above `rules.allows` in `classify_segment`, so an
/// `allow` of a shape refused here could never fire and storing one leaves the owner holding a
/// permission that does nothing. They sit above NOTHING on the deny side: `classify_shell_command`
/// consults `rules.denies` at the line level, before the segment loop is entered at all, so a
/// project's refusal of `tail -f`, `sort -o`, `find . -exec` or `curl … | sh` is enforced — and
/// those are exactly the shapes this function refuses, which is exactly why they are worth
/// refusing. A route that guarded the deny side would decline to store the refusals most worth
/// writing down, and would tell the owner they could never be enforced while the engine enforced
/// them. Migration `0128` puts it in one line: "Do lado `deny` não há nada a validar — uma recusa a
/// mais nunca deixou correr nada."
pub fn shell_form_is_readable(command: &str) -> bool {
    !has_shell_control(command)
        && !command.split_whitespace().any(|token| token == "--fix")
        && !writes_an_output_file(command)
        && !forces_external_diff_or_textconv(command)
        && !runs_a_helper_command(command)
        && !uses_a_flag_its_program_makes_dangerous(command)
        && !find_executes_or_writes(command)
}

/// `--output=<file>` is a *diff* option, so every history command in the safe set (`git log`,
/// `git show`, `git diff`) turns into a file write with one flag — read-local must never mean "wrote
/// a file". Rejecting the whole `--output` family also costs the display-only spellings
/// (`--output-indicator-new`); that over-reach is the cheap side of the trade.
/// `find`'s spelling of the same thing is `-fprint`, `-fprintf`, `-fprint0` and `-fls`: each takes a
/// path and writes the walk to it, so a search allowed for reading would write a file — and, given
/// an absolute path, write it outside the workspace.
pub(super) fn writes_an_output_file(command: &str) -> bool {
    command.split_whitespace().any(|token| {
        token.starts_with("--output") || token.starts_with("-fprint") || token == "-fls"
    })
}

/// Whether the command asks an otherwise-safe program to run a second program for it.
///
/// Every entry is a flag that takes a COMMAND as its value, which makes the leading token a liar:
/// `find . -exec sh -c '...' \;` is an `sh`, and `rg --pre ./x.sh pattern` runs `./x.sh` once per
/// file. Neither guard upstream sees it — the safe-prefix match reads the first token only, and the
/// destructive blocklist matches known program names, which is exactly what an arbitrary payload is
/// not.
///
/// Matched as WHOLE tokens rather than by prefix, and that is load-bearing for `--pre`: `--pretty`
/// starts with it, and `git log --pretty=oneline` is one of the most common commands there is.
pub(super) fn runs_a_helper_command(command: &str) -> bool {
    const RUNS_A_COMMAND: &[&str] = &[
        // find: `-ok`/`-okdir` prompt first, but the prompt goes to a stdin nobody is holding.
        "-exec",
        "-execdir",
        "-ok",
        "-okdir",
        // ripgrep: `--pre` names a preprocessor run per file, `--hostname-bin` a command run to
        // build hyperlinks. `--pre-glob` only filters which files reach `--pre` and is meaningless
        // without it; it is listed so neither half reads as permitted on its own.
        "--pre",
        "--pre-glob",
        "--hostname-bin",
    ];
    // The same flags with the command ATTACHED: `rg --pre=./x.sh` runs `./x.sh` exactly as
    // `rg --pre ./x.sh` does. Matched on the `=` so `--pretty=oneline` stays a different word.
    // `--compress-program` is GNU `sort`'s, which runs it to compress its temporary files; GNU
    // accepts any unambiguous prefix of a long option, and `--co` is already unambiguous on `sort`.
    const RUNS_A_COMMAND_ATTACHED: &[&str] = &["--pre=", "--pre-glob=", "--hostname-bin="];
    command.split_whitespace().any(|token| {
        RUNS_A_COMMAND.contains(&token)
            || RUNS_A_COMMAND_ATTACHED
                .iter()
                .any(|flag| token.starts_with(flag))
            || (token.starts_with("--co") && "--compress-program".starts_with(token_name(token)))
    })
}

/// PURE: a long option's name without its attached `=value`.
pub(super) fn token_name(token: &str) -> &str {
    token.split_once('=').map_or(token, |(name, _)| name)
}

/// Whether the command uses a flag that is harmless on most programs and a file write on this one.
///
/// `-o` is the case that forces this check to know the program. On `go build` and `go test` it names
/// the output binary and takes a path, so `go build -o ../../evil.exe` writes outside the workspace
/// with none of the containment guards ever seeing it — those read a tool call's `file_path`, and a
/// shell command has none. On `grep` the same two characters mean `--only-matching` and print to
/// stdout, which is how half the world uses grep.
///
/// The file's other rejections (`--fix`, `--output`, `--ext-diff`) are global because no allowed
/// program gives them a second meaning. These do, so they are the only ones read next to a program.
pub(super) fn uses_a_flag_its_program_makes_dangerous(command: &str) -> bool {
    let mut tokens = command.split_whitespace();
    let Some(program) = tokens.next().map(program_name) else {
        return false;
    };
    match program {
        // `-o` names an output binary on `go build`/`go test` and an output FILE on `sort`, and
        // both take a path — so `go build -o ../../x.exe` and `sort -o ../../x f` write outside the
        // workspace with none of the containment guards seeing it, because those read a tool call's
        // `file_path` and a shell command has none. On `grep` the same two characters mean
        // `--only-matching` and print to stdout, which is why this guard has to know the program.
        //
        // Read with the value attached as well as apart. Go's flag package takes `-o x`, `-o=x`
        // and `--o=x` alike; `-exec`, `-toolexec` and `-vettool` name a program it runs.
        "go" => tokens.any(|token| {
            let name = token_name(token.trim_start_matches('-'));
            token.starts_with('-') && matches!(name, "o" | "exec" | "toolexec" | "vettool")
        }),
        // `sort` is GNU: short options cluster and take their value attached (`-o~/.bashrc`,
        // `-no out`), and a long option may be any unambiguous prefix (`--out=x`, `--o x`). So
        // any short cluster holding an `o`, and any long option starting `--o`, is the output
        // file. The cluster rule also catches `-to` (an `o` field separator) — a false alarm.
        "sort" => tokens.any(|token| match token.strip_prefix("--") {
            Some(long) => long.starts_with('o'),
            None => token.starts_with('-') && token.contains('o'),
        }),
        // `git reflog` earns a prefix on the read list because its default subcommand shows. Two
        // of its subcommands destroy instead: `expire` prunes entries and `delete` removes one,
        // and the reflog is the last copy of a commit a reset walked away from. Read here rather
        // than pinned as exact spellings, because the READING form takes arguments nobody can
        // enumerate — see the list entry for the argument.
        //
        // `git grep -O<pager>` / `--open-files-in-pager[=<pager>]` hands the matching files to a
        // program of the caller's choosing (or to the configured pager). The line arrives
        // lowercased, so `-O` is indistinguishable from `-o` (`--only-matching`) here: every short
        // cluster holding an `o` is refused on `git grep`, and `--only-matching` is the spelling
        // that stays allowed. `--open` is an unambiguous prefix git's parser also accepts.
        "git" => match tokens.next() {
            Some("reflog") => tokens.any(|token| matches!(token, "expire" | "delete")),
            Some("grep") => tokens.any(|token| match token.strip_prefix("--") {
                Some(long) => {
                    let name = token_name(long);
                    name.len() >= 2 && "open-files-in-pager".starts_with(name)
                }
                None => token.starts_with('-') && token.contains('o'),
            }),
            _ => false,
        },
        // `tail -f` never returns. Not a security hole — but an autonomous run that hangs until its
        // ceiling is the failure this whole feature exists to avoid, and it costs a whole night.
        // `-F` needs no arm of its own: the command reaching here has been lowercased already.
        "tail" => tokens.any(|token| matches!(token, "-f" | "--follow")),
        _ => false,
    }
}

/// `--ext-diff` forces a repo-configured external diff driver to run — arbitrary command
/// execution, not a read — on `git log`/`git show`, where it is off by default; rejecting the
/// token closes that door. On `git diff` a configured driver can already run with no flag at
/// all, a config-driven residual a lexical classifier cannot see and this does NOT close.
/// `--textconv` likewise forces a configured textconv filter where it would not otherwise run.
/// `--no-ext-diff` / `--no-textconv` disable the drivers (the safe direction) and must not match.
pub(super) fn forces_external_diff_or_textconv(command: &str) -> bool {
    command
        .split_whitespace()
        .any(|token| token.starts_with("--ext-diff") || token.starts_with("--textconv"))
}

/// `find`'s options that stop it being a search: `-exec`/`-execdir`/`-ok`/`-okdir` hand every hit
/// to a command of the caller's choosing, `-delete` removes what matched, and the `-fprint` family
/// writes the result list to a file. Allowing `find` without taking these back would allow anything
/// they name.
///
/// Checked on every token of every command rather than only when the program is `find`, because
/// nothing else in the safe set spells any of these — so a conditional form would buy no precision
/// and would first have to decide which token IS the program, the guess `has_destructive_flags`
/// exists precisely because `sudo`, `busybox` and `xargs` defeat.
pub(super) fn find_executes_or_writes(command: &str) -> bool {
    const EXECUTES_OR_WRITES: &[&str] = &[
        "-delete", "-exec", "-execdir", "-ok", "-okdir", "-fprint", "-fprintf", "-fls",
    ];
    command
        .split_whitespace()
        .any(|token| EXECUTES_OR_WRITES.contains(&token))
}

/// `pub(crate)` for `project_policy::ShellRules`, which measures a project's declared prefixes with
/// the same rule the compiled list uses. Generic over the element so `&[&str]` and `&[String]` are
/// the same call — two functions here is how the two lists would drift.
pub fn matches_command_prefix<S: AsRef<str>>(command: &str, prefixes: &[S]) -> bool {
    prefixes.iter().any(|prefix| {
        let prefix = prefix.as_ref();
        command == prefix || command.starts_with(&format!("{prefix} "))
    })
}
