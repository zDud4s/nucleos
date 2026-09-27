//! §spec escrita-com-concessao-por-origem
//!
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
//! nobody checks, which is exactly what the registry exists to prevent. The núcleo's claims below
//! are asserted against [`crate::project_state`]'s file names rather than written out again, so a
//! claim cannot come to name a file no loader reads.
//!
//! # Two homes: the project's state directory, and the project
//!
//! The núcleo's own two files no longer live in the project. They are
//! `~/.nucleos/projects/<id>/autopilot.yaml` and `.../workflows.yaml` — see `project_state.rs` for
//! why neither the project's `.ai/` nor a folder inside the project could hold them. A workflow's
//! claims are still files IN the project, relative to its root. So every row says which of the two
//! it is relative to ([`Home`]), and the write door resolves a row against that home and nothing
//! else: a core row's `path` is a bare file name joined onto the state directory, never onto the
//! project root.
//!
//! # The table has two rows, and the spec asked for eight
//!
//! The design said the núcleo claims `.ai/*.yaml`. Measured against `main.rs`, that is wrong twice
//! over, and both mistakes point outward from the project:
//!
//! - **Seven of those files are not a project's.** `email`, `voice`, `calendar`, `web`, `browser`,
//!   `github`, `council` and `nucleos-models` are this machine's settings — then loaded relative to
//!   the daemon's own working directory, now from `~/.nucleos/`. A route under `/projects/{id}` that wrote them
//!   would edit one daemon's configuration through a URL naming a project, and would do it
//!   identically whichever project was named.
//! - **`.ai/` in a project usually belongs to somebody else.** In this repository it holds
//!   `project.yaml`, `models.yaml` and `pricing.yaml`, which are the `.ai/` workflow harness's and
//!   which the núcleo has never opened. A glob would have claimed a workflow's config as the
//!   núcleo's — the precise confusion §7.3 of the design introduced the registry to end.
//!
//! Those seven machine-level files are not unowned, they are owned by the DAEMON, and they have
//! their own table in [`crate::machine_config`] — same shape, same membership rule, different
//! root. What this module must never do is claim one of them, and a test in that module asserts
//! the two registries stay disjoint.
//!
//! So: the project's `autopilot.yaml`, and its `workflows.yaml` beside it once there was a module
//! that parses that one too. That is not a thin registry, it is an accurate one, and the first row
//! is the load-bearing one — see below.
//!
//! # A third answer: files a WORKFLOW authors
//!
//! An installed workflow declares the files it owns in a project, and those rows arrive here from
//! [`crate::workflows`] rather than from the `static` below. §12 asks for three states where the
//! first version of this module had two — `writable` ≠ `read-only by policy` ≠ `read-only because
//! it is code` — and this is the middle one. `.ai/models.yaml` in this repository is nobody's
//! mistake and nobody's source file: it belongs to the `.ai/` harness, and the harness is the thing
//! that should be edited to change it.
//!
//! **It arrives as ROWS and not as a third [`Owner`] variant**, which is what "the boundary is
//! data" has to mean if it is to mean anything. What separates the two kinds of row is the
//! validator: a workflow's claim has none, because the núcleo does not parse the file, and the
//! membership rule above says a claim without a parser is permission to write bytes nobody checks.
//! So it is shown and not written, and the exit offered is §6.3's second one — edit it where its
//! author lives — instead of a text box that would make this app a second author of somebody else's
//! file.
//!
//! # The one claimed file is the most dangerous file in the project
//!
//! The rules file carries `gate_command`. Whoever writes it decides what *green* means, so an agent
//! able to rewrite it could make every gate it will ever face pass. Moving it out of the project did
//! not change that; `classifier.rs` refuses an agent's write to it in its new home as it did in the
//! old one. The write route is
//! therefore Admin's, by appearing in no table in `auth.rs` — `permits` is default-deny — and there
//! is a test in that module pinning it, because default-deny protects a route nobody thought about
//! and would stop protecting this one the moment somebody added it to a list "for consistency".

/// A parser that must accept the candidate text before it is written.
///
/// A plain `fn` pointer rather than a boxed closure so the table below stays a `static`: the
/// registry is data, and data that needs a constructor run at startup is a step back towards the
/// `if` this module replaced.
pub type Validator = fn(&str) -> Result<(), String>;

/// One file with an author this app knows, and everything a caller needs to act on that safely.
///
/// `Cow` and not `&'static str`, on every field, because half the table is no longer a `static`: an
/// installed workflow's rows are read off a manifest at request time. Borrowed rows cost nothing
/// they used to cost, and owned rows are possible without a second, parallel type for them — which
/// is the shape that would have grown two `owner_of`s that disagreed.
#[derive(Clone)]
pub struct Claim {
    /// Relative to [`Claim::home`], forward slashes. Compared against a caller's path only after
    /// [`normalise`], which is what decides that `./autopilot.yaml` is the same file and that
    /// `x/../autopilot.yaml` is not a question this module answers. It is the row's identity on
    /// the wire; a person is shown [`Claim::display`] instead.
    pub path: std::borrow::Cow<'static, str>,
    /// Which directory `path` is relative to. See the module header.
    pub home: Home,
    /// Who declares it, as a stable wire value — `core`, or a workflow's name. The page renders its
    /// own words for this; a label meant for a person would be a label somebody translates and a
    /// client then compares against.
    pub owner: std::borrow::Cow<'static, str>,
    /// What editing it changes, in one sentence, for the page that shows the fence.
    pub what: std::borrow::Cow<'static, str>,
    /// The parser the candidate must satisfy first, or `None` for a file this app shows and does
    /// not write.
    ///
    /// **The `None` is the whole of the middle state.** The membership rule at the top of this
    /// module is that the núcleo owns what the núcleo parses; a row with no parser is therefore not
    /// the núcleo's to write, and the write route refuses it by asking this question rather than by
    /// keeping a second list of exceptions somewhere else.
    pub validate: Option<Validator>,
}

/// Which directory a claim's `path` is relative to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Home {
    /// The project's state directory, `~/.nucleos/projects/<id>/`. The núcleo's own rows.
    State,
    /// The project's root. A workflow's rows.
    Project,
}

impl Claim {
    /// The row as a person is shown it: `~/.nucleos/projects/<id>/autopilot.yaml` for a state
    /// file, the path relative to the project root for a project file.
    pub fn display(&self, project_id: &str) -> String {
        match self.home {
            Home::State => crate::project_state::display_path(project_id, &self.path),
            Home::Project => self.path.clone().into_owned(),
        }
    }
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

/// The núcleo's own claims. See the module header for why there are two and not eight.
pub static CLAIMS: &[Claim] = &[
    Claim {
        path: std::borrow::Cow::Borrowed(crate::project_state::AUTOPILOT_FILE),
        home: Home::State,
        owner: std::borrow::Cow::Borrowed("core"),
        what: std::borrow::Cow::Borrowed(
            "what this project does on its own: its schedules, its repository triggers, and the gate command that decides what green means",
        ),
        validate: Some(validate_autopilot),
    },
    Claim {
        path: std::borrow::Cow::Borrowed(crate::project_state::PINS_FILE),
        home: Home::State,
        owner: std::borrow::Cow::Borrowed("core"),
        what: std::borrow::Cow::Borrowed(
            "which workflows this project uses, which version of each, and what it overrides on their nodes",
        ),
        validate: Some(crate::workflows::validate_pins),
    },
];

/// Every claim in force in one project: the núcleo's, plus whatever its workflows declare.
///
/// A `Vec` rather than a slice, and read per project rather than per process, because the second
/// half depends on what is installed *here* — which is the whole difference between a fence that
/// describes a project and a fence that describes a build of this app.
///
/// A workflow's row carries no validator, so the write route refuses it. See the module header:
/// the exit for those files is their author, not a text box in this app.
///
/// A path a workflow declares that the núcleo already claims is dropped, and the núcleo keeps it.
/// The two homes differ, so a workflow naming `autopilot.yaml` means a file at the project's root
/// and not the rules file — but one identity on the wire must answer one question, and the row
/// that decides what *green* means is the last one that should become ambiguous because somebody
/// wrote a line in a manifest.
///
/// `pins_file` is the project's `workflows.yaml` in its state directory; `None` (no home directory,
/// or an id that cannot name one) means nothing is installed, and the núcleo's rows still stand.
pub fn claims_for(
    project_root: &std::path::Path,
    pins_file: Option<&std::path::Path>,
    library_root: &std::path::Path,
) -> Vec<Claim> {
    let mut claims: Vec<Claim> = CLAIMS.to_vec();
    let Some(pins_file) = pins_file else {
        return claims;
    };
    let Ok(installed) = crate::workflows::installed(project_root, pins_file, library_root) else {
        return claims;
    };
    for workflow in installed {
        for path in workflow.owns {
            let Some(path) = normalise(&path) else {
                continue;
            };
            if claims.iter().any(|claim| claim.path == path) {
                continue;
            }
            claims.push(Claim {
                path: std::borrow::Cow::Owned(path),
                home: Home::Project,
                owner: std::borrow::Cow::Owned(workflow.name.clone()),
                what: std::borrow::Cow::Owned(format!(
                    "the {} workflow's, declared in its bundle",
                    workflow.name
                )),
                validate: None,
            });
        }
    }
    claims
}

/// One relative path, in the one spelling this module compares.
///
/// Deliberately NOT `std::path::Path`. `Path` means different things on the two platforms, and the
/// difference is not cosmetic here: on Windows `.ai\models.yaml` names a file in `.ai/`, and on
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
pub(crate) fn normalise(rel: &str) -> Option<String> {
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
        .find(|claim| claim.path.as_ref() == path.as_str())
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
        assert_eq!(CLAIMS.len(), 2, "see the module header before adding a row");
        assert_eq!(CLAIMS[0].path, crate::project_state::AUTOPILOT_FILE);
        assert_eq!(CLAIMS[1].path, crate::project_state::PINS_FILE);
        // Both are the project's STATE, never a file in the project: see the module header.
        assert!(CLAIMS.iter().all(|claim| claim.home == Home::State));
        // A `static` row with no parser would be write access to bytes nobody checks. The `None`
        // exists for the rows that come from a workflow, and those are never in here.
        assert!(CLAIMS.iter().all(|claim| claim.validate.is_some()));
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
            // And this machine's own settings, which are not any project's however they are spelled
            // — in the `.ai/` they used to be read from, or under the name they have now.
            ".ai/email.yaml",
            ".ai/github.yaml",
            "email.yaml",
            "nucleos-models.yaml",
            // And the project's own rules and pins where they used to live. Those files are no
            // longer read, so there is nothing for the app to author there.
            ".ai/autopilot.yaml",
            ".ai/workflows.yaml",
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
        let Owner::Declared(claim) = owner_of(CLAIMS, "autopilot.yaml") else {
            panic!("the rules file must be declared");
        };
        assert_eq!(claim.owner, "core");
        assert_eq!(
            claim.display("alpha"),
            "~/.nucleos/projects/alpha/autopilot.yaml"
        );
        assert!(claim.what.contains("gate command"));
        assert!((claim.validate.unwrap())("gate_command: cargo test\n").is_ok());
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
            "x/../autopilot.yaml",
            "../projects/alpha/autopilot.yaml",
            "./../autopilot.yaml",
            "..",
            "../autopilot.yaml",
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
    /// On Windows `.\autopilot.yaml` IS the claimed file. On Linux it is a file whose name
    /// contains a backslash. Granting the claim would write the rules file on
    /// one platform and create a junk file on the other, from the identical request — and the
    /// validator would pass in both cases, because the CONTENT is fine. Nothing downstream could
    /// catch it.
    #[test]
    fn a_backslash_names_two_different_files_and_so_names_none() {
        for path in [".\\autopilot.yaml", "autopilot.yaml\\", "\\autopilot.yaml"] {
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
            "./autopilot.yaml",
            ".//autopilot.yaml",
            "././autopilot.yaml",
            "  autopilot.yaml  ",
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
            "c:/users/me/.nucleos/projects/alpha/autopilot.yaml",
            "//server/share/autopilot.yaml",
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
        let validate = CLAIMS[0].validate.unwrap();
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
        let validate = CLAIMS[0].validate.unwrap();
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
        let validate = CLAIMS[0].validate.unwrap();
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

    /// A workflow's file is claimed and NOT writable, which is §12's middle state.
    ///
    /// Three answers where the first version of this module had two. The row exists so the page can
    /// say who authors the file and offer the right exit; the missing validator is what stops the
    /// app from becoming a second author of it.
    #[test]
    fn a_workflow_declared_file_is_shown_and_not_written() {
        let temp = tempfile::tempdir().unwrap();
        let project = temp.path().join("project");
        std::fs::create_dir_all(&project).unwrap();
        let library = temp.path().join("lib");
        let bundle_dir = library.join("harness").join("1.0");
        std::fs::create_dir_all(&bundle_dir).unwrap();
        std::fs::write(
            bundle_dir.join(crate::workflows::MANIFEST),
            "owns:\n  - .ai/models.yaml\n",
        )
        .unwrap();
        let bundle = crate::workflows::read_bundle(&bundle_dir, "harness", "1.0")
            .unwrap()
            .unwrap();
        let pins = temp
            .path()
            .join("state")
            .join(crate::project_state::PINS_FILE);
        crate::workflows::install(&pins, &bundle).unwrap();

        let claims = claims_for(&project, Some(&pins), &library);
        let Owner::Declared(claim) = owner_of(&claims, ".ai/models.yaml") else {
            panic!("an installed workflow's file must be in the fence");
        };
        assert_eq!(claim.owner, "harness");
        assert_eq!(claim.home, Home::Project);
        assert_eq!(claim.display("alpha"), ".ai/models.yaml");
        assert!(
            claim.validate.is_none(),
            "the app does not parse it, so it does not write it"
        );

        // And the núcleo's own row is still writable beside it, so the page shows two kinds.
        let Owner::Declared(rules) = owner_of(&claims, "autopilot.yaml") else {
            panic!("the rules file must still be declared");
        };
        assert!(rules.validate.is_some());
    }

    /// A bundle cannot take the file that decides what green means.
    ///
    /// The rules file carries `gate_command`. A manifest naming the same identity would make the
    /// row ambiguous, and a row with no validator in its place would be write access with the check
    /// removed — which is the one substitution this merge must not perform.
    #[test]
    fn a_workflow_cannot_claim_a_file_the_nucleo_already_parses() {
        let temp = tempfile::tempdir().unwrap();
        let project = temp.path().join("project");
        std::fs::create_dir_all(&project).unwrap();
        let library = temp.path().join("lib");
        let bundle_dir = library.join("greedy").join("1.0");
        std::fs::create_dir_all(&bundle_dir).unwrap();
        std::fs::write(
            bundle_dir.join(crate::workflows::MANIFEST),
            "owns:\n  - autopilot.yaml\n  - ../outside.yaml\n",
        )
        .unwrap();
        let bundle = crate::workflows::read_bundle(&bundle_dir, "greedy", "1.0")
            .unwrap()
            .unwrap();
        let pins = temp
            .path()
            .join("state")
            .join(crate::project_state::PINS_FILE);
        crate::workflows::install(&pins, &bundle).unwrap();

        let claims = claims_for(&project, Some(&pins), &library);
        let Owner::Declared(rules) = owner_of(&claims, "autopilot.yaml") else {
            panic!("the rules file must still be declared");
        };
        assert_eq!(rules.owner, "core");
        assert_eq!(rules.home, Home::State);
        assert!(rules.validate.is_some());
        // The traversal never becomes a row at all — `normalise` refuses it before it is stored,
        // so the fence cannot be made to describe a file outside the project.
        assert!(claims.iter().all(|claim| !claim.path.contains("outside")));
    }

    /// A project with nothing installed has exactly the núcleo's rows, and a library that is not
    /// there is an empty library rather than a failure that would empty the fence.
    #[test]
    fn a_project_with_no_workflow_has_the_nucleos_rows_and_no_others() {
        let temp = tempfile::tempdir().unwrap();
        let project = temp.path().join("project");
        std::fs::create_dir_all(&project).unwrap();
        let pins = temp
            .path()
            .join("state")
            .join(crate::project_state::PINS_FILE);
        let claims = claims_for(&project, Some(&pins), &temp.path().join("no-library-here"));
        assert_eq!(claims.len(), CLAIMS.len());
        assert_eq!(
            claims_for(&project, None, &temp.path().join("no-library-here")).len(),
            CLAIMS.len(),
            "no state directory is nothing installed, never an empty fence"
        );
    }

    /// Malformed YAML comes back with the parser's words rather than a bare "invalid", because the
    /// raw hatch is unusable without them — a refusal that does not say where the file broke sends
    /// somebody to an editor, which is the surface the hatch exists to replace.
    #[test]
    fn malformed_yaml_is_refused_in_the_parsers_own_words() {
        let refusal =
            (CLAIMS[0].validate.unwrap())("schedules: [\n").expect_err("this is not YAML");
        assert!(!refusal.is_empty());
        assert!(
            refusal.to_lowercase().contains("line") || refusal.to_lowercase().contains("column"),
            "the refusal must locate the break: {refusal}"
        );
    }
}
