//! Who is the legitimate author of a file in a project.
//!
//! "The shell never writes" answers the `.rs` case and fails the `.yaml` case, which is the one
//! people do weekly. The boundary that holds is not *read vs. write* — it is **who is the file's
//! legitimate author**, and that has an answer per file rather than per verb.
//!
//! The answer is DATA. A list of globs inside an `if` in a request handler is the same rule written
//! where nobody can see it, and it rots: the next person adds a pattern to make their case work and
//! nothing says what the patterns were for. Here the boundary is a table, every row carries the
//! sentence that justifies it, and `GET /projects/{id}/ownership` serves the table so the page can
//! show the fence. A fence you can read is a fence somebody can argue with.
//!
//! **The rule for membership: the núcleo owns a file if the núcleo PARSES it.** A claim and a
//! validator arrive together or not at all — a claim without a parser is permission to write bytes
//! nobody checks, which is exactly what the registry exists to prevent. The one claim below is
//! asserted against [`crate::config::AUTOPILOT_RULES_PATH`] rather than written out again, so a
//! claim cannot come to name a file no loader reads.
//!
//! # The table has one row, and the spec asked for eight
//!
//! The design said the núcleo claims `.ai/*.yaml`. Measured against `main.rs`, that is wrong twice
//! over, and both mistakes point outward from the project:
//!
//! - **Seven of those files are not a project's.** `email`, `voice`, `calendar`, `web`, `browser`,
//!   `github`, `council` and `nucleos-models` are loaded relative to the daemon's own working
//!   directory — they are this machine's settings. A route under `/projects/{id}` that wrote them
//!   would edit one daemon's configuration through a URL naming a project, and would do it
//!   identically whichever project was named.
//! - **`.ai/` in a project usually belongs to somebody else.** In this repository it holds
//!   `project.yaml`, `models.yaml` and `pricing.yaml`, which are the `.ai/` workflow harness's and
//!   which the núcleo has never opened. A glob would have claimed a workflow's config as the
//!   núcleo's — the precise confusion §7.3 of the design introduced the registry to end.
//!
//! So: one row, `.ai/autopilot.yaml`. That is not a thin registry, it is an accurate one, and the
//! row it has is the load-bearing one — see below.
//!
//! # The one claimed file is the most dangerous file in the project
//!
//! `.ai/autopilot.yaml` carries `gate_command`. Whoever writes it decides what *green* means, so an
//! agent able to rewrite it could make every gate it will ever face pass. The write route is
//! therefore Admin's, by appearing in no table in `auth.rs` — `permits` is default-deny — and there
//! is a test in that module pinning it, because default-deny protects a route nobody thought about
//! and would stop protecting this one the moment somebody added it to a list "for consistency".

/// A parser that must accept the candidate text before it is written.
///
/// A plain `fn` pointer rather than a boxed closure so the table below stays a `static`: the
/// registry is data, and data that needs a constructor run at startup is a step back towards the
/// `if` this module replaced.
pub type Validator = fn(&str) -> Result<(), String>;

/// One file the app may write, and everything a caller needs to write it safely.
pub struct Claim {
    /// Relative to the project root, forward slashes. Compared against a caller's path only after
    /// [`normalise`], which is what decides that `./​.ai/autopilot.yaml` is the same file and that
    /// `.ai/../.ai/autopilot.yaml` is not a question this module answers.
    pub path: &'static str,
    /// Who declares it, as a stable wire value. The page renders its own words for this; a label
    /// meant for a person would be a label somebody translates and a client then compares against.
    pub owner: &'static str,
    /// What editing it changes, in one sentence, for the page that shows the fence.
    pub what: &'static str,
    /// The parser the candidate must satisfy first.
    pub validate: Validator,
}

/// The answer to *who may write this file*.
///
/// Two variants, and a workflow's claim will be a third ROW rather than a third variant — which is
/// what "the boundary is data" has to mean if it is to mean anything. `Repository` is the default
/// and the answer for every path not in the table: not a refusal to answer, an answer.
///
/// Deliberately derives neither `PartialEq` nor `Debug`: a `Claim` holds a function pointer, and
/// comparing or printing one is a question about an address rather than about the fence. Callers
/// and tests ask `matches!`, which is the question they actually have.
#[derive(Clone, Copy)]
pub enum Owner<'a> {
    /// Somebody the app writes for. Carries the claim, so the caller has the validator and the name
    /// without a second lookup that could disagree with this one.
    Declared(&'a Claim),
    /// Nobody this app may write as.
    Repository,
}

fn validate_autopilot(contents: &str) -> Result<(), String> {
    crate::config::parse_schedule_rules(contents)
        .map(|_| ())
        .map_err(|error| error.to_string())
}

/// The núcleo's claims. See the module header for why there is one.
pub static CLAIMS: &[Claim] = &[Claim {
    path: crate::config::AUTOPILOT_RULES_PATH,
    owner: "core",
    what: "what this project does on its own: its schedules, its repository triggers, and the gate command that decides what green means",
    validate: validate_autopilot,
}];

/// One relative path, in the one spelling this module compares.
///
/// Deliberately NOT `std::path::Path`. `Path` means different things on the two platforms, and the
/// difference is not cosmetic here: on Windows `.ai\autopilot.yaml` is the claimed file, and on
/// Linux it is a file whose NAME contains a backslash, sitting in the project root. A registry that
/// used `Path` would grant the claim on both and write two different files.
///
/// So a backslash is refused outright rather than translated. Every caller reaches this through
/// HTTP, where the path is written with forward slashes; refusing is the only rule under which this
/// function and `inspect::safe_join` agree about which file a string names on every platform.
///
/// `.` segments are dropped and empty ones skipped, matching what `Path::components` does to the
/// same string, so the two guards cannot disagree about `./x` or `a//b`. `..` returns `None`: not
/// because resolving it would necessarily escape, but because this module has no business deciding
/// that — the path guard refuses traversal first and with its own status code, and an ownership
/// answer for a path with `..` in it would be an answer about a file nobody named.
fn normalise(rel: &str) -> Option<String> {
    let rel = rel.trim();
    if rel.is_empty() || rel.contains('\\') || rel.starts_with('/') {
        return None;
    }
    // A drive letter. Absolute on Windows, and on Linux a filename with a colon in it — the same
    // two-answers-for-one-string problem the backslash has, refused for the same reason.
    if rel.as_bytes().get(1) == Some(&b':') {
        return None;
    }
    let mut parts = Vec::new();
    for part in rel.split('/') {
        match part {
            "" | "." => continue,
            ".." => return None,
            other => parts.push(other),
        }
    }
    if parts.is_empty() {
        return None;
    }
    Some(parts.join("/"))
}

/// Who owns `rel` in this project.
///
/// Pure, and takes the table as an argument rather than reading `CLAIMS` directly, because the
/// table is about to grow a per-project half: a workflow installed into a project declares the
/// files it owns, and that arrives from the database. Passing it in is what keeps this function —
/// the one that decides — testable without a pool, a directory, or a file.
pub fn owner_of<'a>(claims: &'a [Claim], rel: &str) -> Owner<'a> {
    let Some(path) = normalise(rel) else {
        return Owner::Repository;
    };
    claims
        .iter()
        .find(|claim| claim.path == path)
        .map_or(Owner::Repository, Owner::Declared)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The claim and the loader name the same file BY CONSTRUCTION, and this is the assertion that
    /// keeps it that way if somebody inlines the const back into a literal.
    ///
    /// The module's whole rule is that the núcleo owns what the núcleo parses. A registry row
    /// pointing at a path no loader reads is write access to bytes nobody checks — the file would
    /// pass validation, be written, and change nothing, which is the failure that looks most like
    /// success.
    #[test]
    fn every_claim_names_a_file_the_nucleo_actually_parses() {
        assert_eq!(CLAIMS.len(), 1, "see the module header before adding a row");
        assert_eq!(CLAIMS[0].path, crate::config::AUTOPILOT_RULES_PATH);
    }

    /// Everything not in the table belongs to the repository, including the files this app is
    /// itself made of. There is no "it is only a comment" exception: `.rs` is layer 2, and layer 2
    /// is read plus a door to an editor.
    #[test]
    fn a_path_outside_the_table_belongs_to_the_repository() {
        for path in [
            "core/src/http.rs",
            "README.md",
            "package.json",
            ".gitignore",
            // The neighbours a glob would have swept in. These are the `.ai/` workflow harness's,
            // and the núcleo has never opened one of them.
            ".ai/project.yaml",
            ".ai/models.yaml",
            ".ai/pricing.yaml",
            // And this machine's own settings, which are not any project's however they are spelled.
            ".ai/email.yaml",
            ".ai/github.yaml",
        ] {
            assert!(
                matches!(owner_of(CLAIMS, path), Owner::Repository),
                "{path} must belong to nobody this app writes as"
            );
        }
    }

    /// The claimed file, and the one sentence about it worth keeping in a test: it holds the gate
    /// command, so writing it is deciding what green means.
    #[test]
    fn the_rules_file_is_declared_and_carries_its_own_validator() {
        let Owner::Declared(claim) = owner_of(CLAIMS, ".ai/autopilot.yaml") else {
            panic!("the rules file must be declared");
        };
        assert_eq!(claim.owner, "core");
        assert!(claim.what.contains("gate command"));
        assert!((claim.validate)("gate_command: cargo test\n").is_ok());
    }

    /// Traversal reaches nothing, and the reason this test exists is that it is the SECOND guard.
    ///
    /// `inspect::safe_join` refuses `..` first and with a different status code, so nothing here is
    /// load-bearing today. It is load-bearing the day somebody reorders the guards, or calls
    /// `owner_of` from a second caller that has no path guard in front of it — and both of those
    /// are ordinary edits that no other test would catch.
    #[test]
    fn a_traversal_cannot_reach_a_claimed_file() {
        for path in [
            ".ai/../.ai/autopilot.yaml",
            "../nucleos/.ai/autopilot.yaml",
            ".ai/./../.ai/autopilot.yaml",
            "..",
            "../.ai/autopilot.yaml",
        ] {
            assert!(
                matches!(owner_of(CLAIMS, path), Owner::Repository),
                "{path} must not resolve to a claim"
            );
        }
    }

    /// A backslash is refused rather than translated, and the test says which platform each answer
    /// would have been wrong on.
    ///
    /// On Windows `.ai\autopilot.yaml` IS the claimed file. On Linux it is a file whose name
    /// contains a backslash, in the project root. Granting the claim would write the rules file on
    /// one platform and create a junk file on the other, from the identical request — and the
    /// validator would pass in both cases, because the CONTENT is fine. Nothing downstream could
    /// catch it.
    #[test]
    fn a_backslash_names_two_different_files_and_so_names_none() {
        for path in [
            ".ai\\autopilot.yaml",
            ".ai/autopilot.yaml\\",
            "\\.ai\\autopilot.yaml",
        ] {
            assert!(
                matches!(owner_of(CLAIMS, path), Owner::Repository),
                "{path}"
            );
        }
    }

    /// The spellings that ARE the same file, agreeing with what `Path::components` does to them, so
    /// the ownership answer and the path guard cannot disagree about which file was named.
    #[test]
    fn a_dot_segment_or_a_doubled_slash_does_not_change_the_answer() {
        for path in [
            "./.ai/autopilot.yaml",
            ".ai//autopilot.yaml",
            ".ai/./autopilot.yaml",
            "  .ai/autopilot.yaml  ",
        ] {
            assert!(
                matches!(owner_of(CLAIMS, path), Owner::Declared(_)),
                "{path} is the rules file"
            );
        }
    }

    /// Absolute is nobody's, whichever platform's spelling of absolute it is. A registry that
    /// answered for `/etc/passwd` would be answering about a file outside every project.
    #[test]
    fn an_absolute_path_belongs_to_nobody() {
        for path in [
            "/etc/passwd",
            "/",
            "C:/Windows/System32/drivers/etc/hosts",
            "c:/projects/nucleos/.ai/autopilot.yaml",
            "//server/share/.ai/autopilot.yaml",
        ] {
            assert!(
                matches!(owner_of(CLAIMS, path), Owner::Repository),
                "{path}"
            );
        }
    }

    /// The empty path is the project root, not a file, and the root is nobody's to overwrite.
    #[test]
    fn the_empty_path_and_the_root_belong_to_nobody() {
        for path in ["", "   ", ".", "./", "/"] {
            assert!(
                matches!(owner_of(CLAIMS, path), Owner::Repository),
                "{path:?}"
            );
        }
    }

    /// A file that is nothing but comments is VALID, and this is the trap the validator was most
    /// likely to fall into.
    ///
    /// `load_schedule_rules` has an explicit branch for it, with a comment saying why: commenting
    /// the `gate_command:` line out is how somebody switches a gate off for an afternoon, and in
    /// this repository's own config that leaves a file of comments. A validator that refused it
    /// would refuse to save the exact edit the loader was written to accept — the app would be
    /// stricter than the daemon about the daemon's own file.
    #[test]
    fn a_file_of_nothing_but_comments_saves_because_that_is_how_a_gate_is_switched_off() {
        let validate = CLAIMS[0].validate;
        assert!(validate("# gate_command: cargo test\n").is_ok());
        assert!(validate("").is_ok());
        assert!(validate("   \n\n").is_ok());
    }

    /// A typo'd key is refused, which is the sharpest thing the structured surface buys over an
    /// editor.
    ///
    /// `AutopilotRules` is `deny_unknown_fields`. `vim` would save `gate_commmand:` cheerfully and
    /// the gate would silently be absent from then on; the daemon would report `no gate` and be
    /// telling the truth. Here it cannot be saved, and the parser's own words say which key it did
    /// not recognise.
    #[test]
    fn a_key_the_daemon_does_not_know_cannot_be_saved() {
        let validate = CLAIMS[0].validate;
        let refusal = validate("gate_commmand: cargo test\n").expect_err("a typo must not save");
        assert!(
            refusal.contains("gate_commmand"),
            "the refusal must name the key: {refusal}"
        );
    }

    /// A value that parses as a number but that no rule could have meant is refused too, because
    /// the validator is the loader's own range check and not a second, laxer one.
    ///
    /// A NaN budget is the one worth pinning: every comparison against it is false, so
    /// `job_over_budget` reads as already blown and the job stops at its first node while the file
    /// reads as though it had asked for something generous.
    #[test]
    fn a_number_no_rule_could_have_meant_is_refused_by_the_loaders_own_check() {
        let validate = CLAIMS[0].validate;
        let rule = |budget: &str| {
            format!(
                "schedules:\n  - name: nightly\n    cron: \"0 3 * * *\"\n    prompt: go\n    graph:\n      budget_usd: {budget}\n"
            )
        };
        assert!(validate(&rule("2.5")).is_ok());
        assert!(validate(&rule("-1")).is_err());
        assert!(validate(&rule(".nan")).is_err());
        assert!(validate(&rule(".inf")).is_err());
    }

    /// Malformed YAML comes back with the parser's words rather than a bare "invalid", because the
    /// raw hatch is unusable without them — a refusal that does not say where the file broke sends
    /// somebody to an editor, which is the surface the hatch exists to replace.
    #[test]
    fn malformed_yaml_is_refused_in_the_parsers_own_words() {
        let refusal = (CLAIMS[0].validate)("schedules: [\n").expect_err("this is not YAML");
        assert!(!refusal.is_empty());
        assert!(
            refusal.to_lowercase().contains("line") || refusal.to_lowercase().contains("column"),
            "the refusal must locate the break: {refusal}"
        );
    }
}
