//! §spec alcada-por-projecto
//!
//! The three lists a project keeps about itself: what its worktrees may run, what the GitHub
//! manager may do on its remote, and where a landing may be sent.
//!
//! **Not `project_commands.rs`.** That module holds commands a project declares ABOUT itself —
//! `gate`, `fmt`, `typecheck` — things somebody presses a button to run. These are not commands,
//! they are permissions, and nothing in this module ever executes anything. Two neighbours with
//! the same first word, and the filename is the only place the difference can be stated before
//! somebody opens the wrong one.
//!
//! One module for three tables because the three answer ONE question — what may this project do
//! without asking — and splitting them would put the same `project_id` resolution in three places.
//!
//! **Every read here fails in the safe direction, and the direction is not the same for all
//! three.** An unreadable GitHub list or land-target list yields nothing, which withholds autonomy
//! — safe. An unreadable shell list cannot do that: yielding an empty `deny` would LOSE a refusal
//! somebody wrote down. So `shell_rules` returns a `Result` and its caller treats the error as
//! "I cannot say this is safe", which is an approval prompt and never an allow.

// Written here in one piece, wired in over Chunks 2 to 5: nothing outside this module's own tests
// calls any of it yet. The house idiom for exactly that — `capabilities.rs`, `config.rs`,
// `assistants.rs` and seventeen others — and it comes off the day the last consumer lands.
// Without it `cargo clippy --all-targets -- -D warnings`, which `scripts/gates.sh core` runs, is
// red from this commit until Chunk 5, and every gate run in between reports somebody else's fault.
//
// Every item below IS exercised by this module's own tests, which is why the suppression is
// `not(test)` and not blanket: the lint stays live under the test build, so an item that stops
// being exercised has to say so rather than hide behind this line.
#![cfg_attr(not(test), allow(dead_code))]

use sqlx::SqlitePool;

/// What a shell rule says about the prefix it names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Verdict {
    /// Runs without asking, in this project. Widens what the compiled list would have asked about
    /// — and never what it would have refused; the order in `classifier::classify_segment` is what
    /// makes that true, not this type.
    Allow,
    /// Never runs here, whatever the compiled list says.
    Deny,
}

impl Verdict {
    pub fn as_db_str(self) -> &'static str {
        match self {
            Self::Allow => "allow",
            Self::Deny => "deny",
        }
    }

    pub fn from_db_str(raw: &str) -> Option<Self> {
        match raw {
            "allow" => Some(Self::Allow),
            "deny" => Some(Self::Deny),
            _ => None,
        }
    }
}

/// The one fold a declared prefix gets, and it is `classifier::normalize_command` — the SAME
/// function that folds the command the prefix will be compared against, not a second spelling of it.
///
/// **The bug this exists to close.** `ShellRules::allows` already said it reuses the compiled list's
/// comparison "rather than restating it". It reused the comparison and not the INVARIANT that makes
/// the comparison correct: every entry in `SAFE_COMMAND_PREFIXES` is a lower-case, single-spaced
/// source literal, so `matches_command_prefix` never had to fold anything. A `TEXT` column carries
/// no such invariant. `normalize_command` lower-cases the command and collapses its whitespace, so
/// an unfolded `Remove-Item`, or a `npm  ci` typed with two spaces, could never match the very thing
/// it was written down to judge.
///
/// The failure was asymmetric in the worst direction. An unfolded `allow` prefix is inert and the
/// command merely waits for a person — annoying, and safe. An unfolded `deny` prefix is inert too,
/// and an inert refusal is an ALLOW: `deny = "LS"` measured `("allow", "read-local")` on `ls -la`,
/// with no prompt and no log line for the owner to find. `Remove-Item` is not a contrived spelling
/// here either; this daemon ships a `PowerShell` tool and PascalCase is the canonical cmdlet form.
///
/// Applied on the way OUT of the table as well as on the way in, because a row can arrive without
/// passing `declare_shell_rule` at all — an out-of-band write, or a migration older than this rule —
/// and such a row must not be able to smuggle in a dead refusal.
fn fold_prefix(prefix: &str) -> String {
    crate::classifier::normalize_command(prefix)
}

/// One project's two lists, already split by verdict so the classifier does no filtering.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ShellRules {
    pub allow: Vec<String>,
    pub deny: Vec<String>,
}

impl ShellRules {
    pub fn is_empty(&self) -> bool {
        self.allow.is_empty() && self.deny.is_empty()
    }

    /// Measured as a PREFIX, which is `SAFE_COMMAND_PREFIXES`'s form and reuses its comparison
    /// rather than restating it — two spellings of "starts with" is how the compiled list and the
    /// declared one would come to disagree about `bash scripts/gates.shell`.
    ///
    /// The prefix is folded HERE as well as at the table's edge, and the repetition is deliberate.
    /// These fields are `pub`: a `ShellRules` can be built without ever passing `shell_rules`, and
    /// this is the last place that can still make the comparison right. See `fold_prefix` for what
    /// went wrong when only the comparison was reused and not the invariant behind it.
    pub fn allows(&self, command: &str) -> bool {
        Self::matches(command, &self.allow)
    }

    pub fn denies(&self, command: &str) -> bool {
        Self::matches(command, &self.deny)
    }

    /// One comparison for both verdicts, for the reason `matches_command_prefix` gives for being
    /// generic: two functions here is how the two lists would drift.
    fn matches(command: &str, prefixes: &[String]) -> bool {
        prefixes.iter().any(|prefix| {
            crate::classifier::matches_command_prefix(command, &[fold_prefix(prefix)])
        })
    }
}

/// Both lists, split by verdict. `Err` when the table could not be read — see the module doc:
/// yielding an empty `deny` would lose a refusal somebody wrote down, so this one cannot fail soft.
pub async fn shell_rules(pool: &SqlitePool, project_id: &str) -> Result<ShellRules, String> {
    let rows = sqlx::query_as::<_, (String, String)>(
        "SELECT prefix, verdict FROM project_shell_rules WHERE project_id = ? ORDER BY prefix",
    )
    .bind(project_id)
    .fetch_all(pool)
    .await
    .map_err(|error| format!("could not read {project_id}'s shell rules: {error}"))?;

    let mut rules = ShellRules::default();
    for (prefix, verdict) in rows {
        // Folded before the match rather than in each arm, so no arm can be added later that
        // forgets to. The unreadable-verdict arm below needs it as much as the other two: the row
        // it pushes onto `deny` is precisely the one nobody vetted.
        let prefix = fold_prefix(&prefix);
        match Verdict::from_db_str(&verdict) {
            Some(Verdict::Allow) => rules.allow.push(prefix),
            Some(Verdict::Deny) => rules.deny.push(prefix),
            // The migration's `CHECK (verdict IN ('allow', 'deny'))` should make this arm
            // unreachable -- but "should" is not "cannot": schema drift, an out-of-band write, or a
            // future migration loosening the constraint could still put an unrecognised word here.
            // Dropping the row, as this used to do, is the PERMISSIVE answer: it silently discards a
            // row that might have been exactly the refusal somebody wrote down. This module's own
            // header says every read here fails toward refusal, so a prefix nobody can read the
            // verdict of is one this module DENIES, not one it forgets.
            None => {
                tracing::warn!(%prefix, %verdict, project_id, "shell rule: unreadable verdict; denied");
                rules.deny.push(prefix);
            }
        }
    }
    Ok(rules)
}

/// Declares a rule, or changes the verdict of one already declared.
///
/// `ON CONFLICT (project_id, prefix)` — the pair the unique index names — because the IDENTITY of a
/// rule is the prefix it names. Without it, changing your mind about a verdict would leave BOTH
/// answers in the table and the reader would pick one of them.
///
/// Stores the FOLDED prefix, not the typed one, so that what is written is what will be enforced —
/// see `fold_prefix`. Folding only on the way out would work for the classifier and lie to
/// everything else: the table, and the page a later chunk builds on it, would go on showing a
/// `Remove-Item` that is in fact enforced as `remove-item`.
pub async fn declare_shell_rule(
    pool: &SqlitePool,
    project_id: &str,
    prefix: &str,
    verdict: Verdict,
    note: Option<&str>,
) -> Result<(), String> {
    sqlx::query(
        "INSERT INTO project_shell_rules (project_id, prefix, verdict, note, created_at)
         VALUES (?, ?, ?, ?, datetime('now'))
         ON CONFLICT (project_id, prefix)
         DO UPDATE SET verdict = excluded.verdict, note = excluded.note",
    )
    .bind(project_id)
    .bind(fold_prefix(prefix))
    .bind(verdict.as_db_str())
    .bind(note)
    .execute(pool)
    .await
    .map(|_| ())
    .map_err(|error| format!("could not declare {prefix} for {project_id}: {error}"))
}

/// Undeclares one rule, leaving the rest of the project's shell list untouched. The `WHERE` names
/// exactly the pair `declare_shell_rule`'s `ON CONFLICT` would have matched — the identity of a rule
/// is the prefix, and only that row goes.
///
/// **`Ok(false)` means there was no such rule**, off `rows_affected`. It is the shape
/// `project_commands::remove` already uses, and it exists so a caller can tell a delete that
/// happened from one that matched nothing WITHOUT reading the table first. A route that re-read
/// instead would answer out of a second query — a race at best, and at worst a presence check
/// computing the key by a second spelling of the fold, which is the bug `fold_prefix` was written
/// to close. Answering from the statement that does the work is what makes the two unable to
/// disagree.
///
/// Folds for the same reason `declare_shell_rule` does, and with the SAME function: forget must
/// compute the same key declare wrote, or the argument matches zero rows — which now SAYS so
/// instead of wearing the face of a real delete. A `declare` that folded and a `forget` that only
/// trimmed would rebuild exactly that bug one level up from where it was first found, which is why
/// `a_declared_rule_comes_back_on_its_own_side` round-trips a mixed-case, double-spaced prefix
/// through both.
pub async fn forget_shell_rule(
    pool: &SqlitePool,
    project_id: &str,
    prefix: &str,
) -> Result<bool, String> {
    sqlx::query("DELETE FROM project_shell_rules WHERE project_id = ? AND prefix = ?")
        .bind(project_id)
        .bind(fold_prefix(prefix))
        .execute(pool)
        .await
        .map(|done| done.rows_affected() > 0)
        .map_err(|error| format!("could not forget {prefix} for {project_id}: {error}"))
}

/// The GitHub operations this project runs without asking, with a read failure still in hand.
///
/// **Two callers want opposite things out of one query, which is why there are two functions.** A
/// DECIDING caller wants the failure swallowed, because an empty list withholds autonomy and that
/// is the safe direction. A DISPLAYING caller — `GET /projects/{id}/github-ops` — wants it raised,
/// because to a page whose whole job is showing an owner what their project may do, `[]` is not an
/// absence of information but a positive claim that nothing is declared. Serving that claim out of
/// a database error tells the owner something false about their own autonomy, in the one place they
/// go to check it.
///
/// `shell_rules` needs no such pair: it has only ever returned `Result`, because losing a `deny`
/// is unsafe in either direction. This is that same shape, arrived at from the display half.
pub async fn try_github_ops(pool: &SqlitePool, project_id: &str) -> Result<Vec<String>, String> {
    sqlx::query_scalar::<_, String>(
        "SELECT op_kind FROM project_github_ops WHERE project_id = ? ORDER BY op_kind",
    )
    .bind(project_id)
    .fetch_all(pool)
    .await
    .map_err(|error| format!("could not read {project_id}'s github ops: {error}"))
}

/// The deciding half's answer: an unreadable list yields nothing, which WITHHOLDS autonomy — the
/// safe direction, and the one `narrow` already takes about a malformed entry. See `try_github_ops`
/// for why both exist.
pub async fn github_ops(pool: &SqlitePool, project_id: &str) -> Vec<String> {
    try_github_ops(pool, project_id).await.unwrap_or_else(|error| {
        tracing::warn!(%error, project_id, "github ops: unreadable; this project stays autonomous in nothing");
        Vec::new()
    })
}

/// Grants one GitHub operation without asking, in this project. Unlike `declare_shell_rule`, there
/// is no verdict or note attached to an op — presence in the table IS the grant, and the pair the
/// unique index names is still the identity, so a repeat declaration finds a row already saying
/// what it came to say. `DO NOTHING` leaves that row, and its original `created_at`, exactly as it
/// was; `DO UPDATE` would quietly turn the column from "when this was declared" into "when it was
/// last redeclared", which nothing reads today but would mislead whoever adds a "declared since"
/// column later and takes the name at face value.
pub async fn declare_github_op(
    pool: &SqlitePool,
    project_id: &str,
    op_kind: &str,
) -> Result<(), String> {
    sqlx::query(
        "INSERT INTO project_github_ops (project_id, op_kind, created_at)
         VALUES (?, ?, datetime('now'))
         ON CONFLICT (project_id, op_kind)
         DO NOTHING",
    )
    .bind(project_id)
    .bind(op_kind.trim())
    .execute(pool)
    .await
    .map(|_| ())
    .map_err(|error| format!("could not declare {op_kind} for {project_id}: {error}"))
}

/// Withdraws one operation from the project's autonomous set. After this it goes back to asking —
/// the safe direction, and the only one a forget can take here.
///
/// `Ok(false)` for an operation that was never declared, for `forget_shell_rule`'s reason. Here it
/// is load-bearing in a way a re-read could never be: `github_ops` swallows a read failure into an
/// empty `Vec`, so a caller checking presence through IT would read an unreadable table as "never
/// declared" and answer 404 while the row stands and the operation goes on running unattended.
///
/// Trims like `declare_github_op` does, so the two agree on what a key is — the same asymmetry that
/// would leave `forget_shell_rule` matching zero rows applies here too.
pub async fn forget_github_op(
    pool: &SqlitePool,
    project_id: &str,
    op_kind: &str,
) -> Result<bool, String> {
    sqlx::query("DELETE FROM project_github_ops WHERE project_id = ? AND op_kind = ?")
        .bind(project_id)
        .bind(op_kind.trim())
        .execute(pool)
        .await
        .map(|done| done.rows_affected() > 0)
        .map_err(|error| format!("could not forget {op_kind} for {project_id}: {error}"))
}

/// The branches a `--land` may target in this project, besides `integration_branch` — which is
/// always admissible, table empty or not, and so never has a row of its own here. With a read
/// failure still in hand, for the display half; see `try_github_ops` for why the pair exists.
pub async fn try_land_targets(pool: &SqlitePool, project_id: &str) -> Result<Vec<String>, String> {
    sqlx::query_scalar::<_, String>(
        "SELECT branch FROM project_land_targets WHERE project_id = ? ORDER BY branch",
    )
    .bind(project_id)
    .fetch_all(pool)
    .await
    .map_err(|error| format!("could not read {project_id}'s land targets: {error}"))
}

/// The deciding half's answer: an unreadable table must not open a landing spot this project never
/// earned, so it lands nowhere extra. `land::resolve_target` is the caller that wants exactly this,
/// and its own doc already argues the cost — that a database hiccup is indistinguishable here from
/// a project that declared nothing, which is why its refusal says what was *recorded* rather than
/// what the project *admits*.
pub async fn land_targets(pool: &SqlitePool, project_id: &str) -> Vec<String> {
    try_land_targets(pool, project_id).await.unwrap_or_else(|error| {
        tracing::warn!(%error, project_id, "land targets: unreadable; this project lands nowhere extra");
        Vec::new()
    })
}

/// Opens one more landing target for this project. Unlike `declare_shell_rule`, there is no verdict
/// or note attached to a target — presence in the table IS the grant, and the pair the unique index
/// names is still the identity, so a repeat declaration finds a row already saying what it came to
/// say. `DO NOTHING` leaves that row, and its original `created_at`, exactly as it was; `DO UPDATE`
/// would quietly turn the column from "when this was declared" into "when it was last redeclared",
/// which nothing reads today but would mislead whoever adds a "declared since" column later and
/// takes the name at face value.
pub async fn declare_land_target(
    pool: &SqlitePool,
    project_id: &str,
    branch: &str,
) -> Result<(), String> {
    sqlx::query(
        "INSERT INTO project_land_targets (project_id, branch, created_at)
         VALUES (?, ?, datetime('now'))
         ON CONFLICT (project_id, branch)
         DO NOTHING",
    )
    .bind(project_id)
    .bind(branch.trim())
    .execute(pool)
    .await
    .map(|_| ())
    .map_err(|error| format!("could not declare {branch} for {project_id}: {error}"))
}

/// Closes one landing target. `integration_branch` needs no row to stay admissible, so this can
/// never take away the one destination every project already has — which is also why `Ok(false)`
/// over the integration branch's own name is the honest answer rather than a missing feature: there
/// was no target of that name to close.
///
/// `Ok(false)` for a branch that was never declared, for `forget_github_op`'s reason, and it matters
/// here for the same one: `land_targets` swallows a read failure too.
///
/// Trims like `declare_land_target` does, for the same reason as the other two `forget_*`
/// functions: declare and forget must agree on what a key is.
pub async fn forget_land_target(
    pool: &SqlitePool,
    project_id: &str,
    branch: &str,
) -> Result<bool, String> {
    sqlx::query("DELETE FROM project_land_targets WHERE project_id = ? AND branch = ?")
        .bind(project_id)
        .bind(branch.trim())
        .execute(pool)
        .await
        .map(|done| done.rows_affected() > 0)
        .map_err(|error| format!("could not forget {branch} for {project_id}: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rules(allow: &[&str], deny: &[&str]) -> ShellRules {
        ShellRules {
            allow: allow.iter().map(|entry| (*entry).to_owned()).collect(),
            deny: deny.iter().map(|entry| (*entry).to_owned()).collect(),
        }
    }

    /// A prefix matches the command that IS it and the command that starts with it plus a space,
    /// which is `SAFE_COMMAND_PREFIXES`'s own rule and not a second one.
    #[test]
    fn a_prefix_matches_itself_and_what_follows_it() {
        let rules = rules(&["bash scripts/gates.sh"], &[]);
        assert!(rules.allows("bash scripts/gates.sh"));
        assert!(rules.allows("bash scripts/gates.sh all"));
        assert!(!rules.allows("bash scripts/gates.shell"));
        assert!(!rules.allows("bash other.sh"));
    }

    /// Decision #3: `deny` wins. Both lists naming the same prefix is a person contradicting
    /// themselves, and the answer to that is the refusal, every time.
    #[test]
    fn deny_wins_over_allow_for_the_same_prefix() {
        let rules = rules(&["git push"], &["git push"]);
        assert!(rules.denies("git push origin master"));
    }

    /// The two lists are two lists. `deny_wins_over_allow_for_the_same_prefix` puts one prefix on
    /// both, so it cannot tell `denies` apart from a `denies` that read `self.allow` by mistake —
    /// this can. The separation is the whole design: precedence lives in the order the classifier
    /// asks, not in these two answers.
    #[test]
    fn neither_list_answers_for_the_other() {
        let rules = rules(&["cargo run"], &["npm ci"]);
        assert!(rules.allows("cargo run") && !rules.denies("cargo run"));
        assert!(rules.denies("npm ci") && !rules.allows("npm ci"));
    }

    /// The pair is a round trip, and anything else is `None`. `shell_rules` leans on that `None` to
    /// DENY a malformed row rather than guess at it or discard it, so it is the safety property and
    /// not a formality. The migration's `CHECK (verdict IN ('allow', 'deny'))` should stop such a
    /// row ever being written; this is what happens to one that exists anyway.
    ///
    /// It is also what keeps `#![cfg_attr(not(test), allow(dead_code))]` honest: that attribute
    /// leaves the lint live under `cfg(test)`, so an item no test touches is still flagged. Without
    /// this test `Verdict` is dead under the test build and `clippy -D warnings` is red.
    #[test]
    fn a_verdict_survives_the_round_trip_and_nothing_else_is_one() {
        for verdict in [Verdict::Allow, Verdict::Deny] {
            assert_eq!(Verdict::from_db_str(verdict.as_db_str()), Some(verdict));
        }
        assert_eq!(Verdict::from_db_str("Allow"), None);
        assert_eq!(Verdict::from_db_str(""), None);

        // Two spellings of the same word, and nothing but this makes them agree. The derive is
        // what a JSON body will be read through; `as_db_str` is what the table holds.
        for verdict in [Verdict::Allow, Verdict::Deny] {
            let json = serde_json::to_string(&verdict).unwrap();
            assert_eq!(json, format!("\"{}\"", verdict.as_db_str()));
            assert_eq!(serde_json::from_str::<Verdict>(&json).unwrap(), verdict);
        }
    }

    /// A project that declared nothing is not a project that forbade everything.
    #[test]
    fn no_rules_at_all_neither_allows_nor_denies() {
        let rules = ShellRules::default();
        assert!(!rules.allows("ls"));
        assert!(!rules.denies("ls"));
        assert!(rules.is_empty());
    }

    /* ---------------------------------------------------------------- db -- */

    async fn pool() -> SqlitePool {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::migrate!().run(&pool).await.unwrap();
        pool
    }

    #[tokio::test]
    async fn a_declared_rule_comes_back_on_its_own_side() {
        let pool = pool().await;
        declare_shell_rule(
            &pool,
            "alpha",
            "bash scripts/gates.sh",
            Verdict::Allow,
            None,
        )
        .await
        .unwrap();
        declare_shell_rule(
            &pool,
            "alpha",
            "git push",
            Verdict::Deny,
            Some("never from a worktree"),
        )
        .await
        .unwrap();

        let rules = shell_rules(&pool, "alpha").await.unwrap();
        assert_eq!(rules.allow, vec!["bash scripts/gates.sh".to_owned()]);
        assert_eq!(rules.deny, vec!["git push".to_owned()]);

        // And forgetting one takes only that one. Without this call `forget_shell_rule` is the
        // single one of the nine that no test touches, which under the module's
        // `#![cfg_attr(not(test), allow(dead_code))]` leaves it dead in the TEST build and turns
        // `clippy -D warnings` red -- that attribute covers the non-test build only.
        assert!(
            forget_shell_rule(&pool, "alpha", "git push").await.unwrap(),
            "a rule that was there reports that it went"
        );
        let rules = shell_rules(&pool, "alpha").await.unwrap();
        assert!(rules.deny.is_empty());
        assert_eq!(rules.allow, vec!["bash scripts/gates.sh".to_owned()]);

        // Declare and forget must agree on what a key IS. `declare_shell_rule` trims before
        // binding, so a forget that does not would compare a padded string against the trimmed
        // stored value, match zero rows, and still return `Ok(())` -- indistinguishable from a real
        // delete. That is the worst shape of this bug: the caller believes a refusal was lifted and
        // it was not.
        declare_shell_rule(&pool, "alpha", "cargo run", Verdict::Allow, None)
            .await
            .unwrap();
        forget_shell_rule(&pool, "alpha", "  cargo run  ")
            .await
            .unwrap();
        let rules = shell_rules(&pool, "alpha").await.unwrap();
        assert!(!rules.allow.contains(&"cargo run".to_owned()));

        // The same agreement, now over the whole fold rather than only the ends. `declare` stores
        // what `fold_prefix` returns, so what comes back is lower-cased and single-spaced whatever
        // was typed -- and `forget` has to compute that same key from a DIFFERENT spelling of the
        // same rule, which is the only way to prove the two fold identically. A `declare` that
        // folded and a `forget` that only trimmed would match zero rows and still answer `Ok(())`:
        // the caller believes a refusal was lifted, and it was not.
        declare_shell_rule(
            &pool,
            "alpha",
            "  Remove-Item   -Recurse ",
            Verdict::Deny,
            None,
        )
        .await
        .unwrap();
        let rules = shell_rules(&pool, "alpha").await.unwrap();
        assert_eq!(rules.deny, vec!["remove-item -recurse".to_owned()]);
        forget_shell_rule(&pool, "alpha", "REMOVE-ITEM  -recurse")
            .await
            .unwrap();
        let rules = shell_rules(&pool, "alpha").await.unwrap();
        assert!(rules.deny.is_empty());
    }

    /// A row that never passed `declare_shell_rule` -- an out-of-band write, or a migration older
    /// than the fold -- must still come back enforceable. This is what makes the READ-side fold
    /// load-bearing rather than belt-and-braces: with only the write side, this row's refusal is
    /// dead on arrival, and a dead refusal is an allow.
    ///
    /// Asserted on the strings `shell_rules` returns rather than on a classification, so it pins the
    /// fold at this boundary specifically and cannot be satisfied by the comparison folding later.
    #[tokio::test]
    async fn a_prefix_written_out_of_band_is_folded_on_the_way_out() {
        let pool = pool().await;
        sqlx::query(
            "INSERT INTO project_shell_rules (project_id, prefix, verdict, created_at)
             VALUES (?, ?, ?, datetime('now'))",
        )
        .bind("alpha")
        .bind("  Remove-Item   -Recurse ")
        .bind("deny")
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO project_shell_rules (project_id, prefix, verdict, created_at)
             VALUES (?, ?, ?, datetime('now'))",
        )
        .bind("alpha")
        .bind("BASH  scripts/gates.sh")
        .bind("allow")
        .execute(&pool)
        .await
        .unwrap();

        let rules = shell_rules(&pool, "alpha").await.unwrap();
        assert_eq!(rules.deny, vec!["remove-item -recurse".to_owned()]);
        assert_eq!(rules.allow, vec!["bash scripts/gates.sh".to_owned()]);
        // And the folded list judges the command the unfolded one could not.
        assert!(rules.denies("remove-item -recurse x"));
        assert!(rules.allows("bash scripts/gates.sh all"));
    }

    /// The identity is (project, prefix): declaring the same prefix again EDITS it. Otherwise
    /// changing your mind about a verdict leaves both answers in the table and the reader picks one.
    #[tokio::test]
    async fn declaring_the_same_prefix_again_changes_its_verdict() {
        let pool = pool().await;
        declare_shell_rule(&pool, "alpha", "git push", Verdict::Allow, None)
            .await
            .unwrap();
        declare_shell_rule(&pool, "alpha", "git push", Verdict::Deny, None)
            .await
            .unwrap();

        let rules = shell_rules(&pool, "alpha").await.unwrap();
        assert!(rules.allow.is_empty());
        assert_eq!(rules.deny, vec!["git push".to_owned()]);
    }

    /// One project's rules are not another's, in ANY of the three tables. Stated as a test because
    /// the `project_id` is a bind parameter and a missing `WHERE` is the cheapest possible way to
    /// leak a whole machine's policy — all three tables share the same shape, and nothing pins
    /// `github_ops`'s and `land_targets`'s own `WHERE` beyond this.
    #[tokio::test]
    async fn one_projects_rules_do_not_reach_another() {
        let pool = pool().await;
        declare_shell_rule(&pool, "alpha", "cargo run", Verdict::Allow, None)
            .await
            .unwrap();
        declare_github_op(&pool, "alpha", "run_list").await.unwrap();
        declare_land_target(&pool, "alpha", "master").await.unwrap();

        assert!(shell_rules(&pool, "beta").await.unwrap().is_empty());
        assert!(github_ops(&pool, "beta").await.is_empty());
        assert!(land_targets(&pool, "beta").await.is_empty());
    }

    #[tokio::test]
    async fn a_project_that_declared_nothing_reads_empty_everywhere() {
        let pool = pool().await;
        assert!(shell_rules(&pool, "alpha").await.unwrap().is_empty());
        assert!(github_ops(&pool, "alpha").await.is_empty());
        assert!(land_targets(&pool, "alpha").await.is_empty());
    }

    #[tokio::test]
    async fn github_ops_and_land_targets_round_trip_and_forget() {
        let pool = pool().await;
        declare_github_op(&pool, "alpha", "run_list").await.unwrap();
        // Declared twice on purpose: `declare_github_op`'s `ON CONFLICT ... DO NOTHING` must make
        // this a no-op, not a second row or a constraint error. Without this line a regression to a
        // bare `INSERT` would only surface as `UNIQUE constraint failed`, and nothing here would
        // catch it.
        declare_github_op(&pool, "alpha", "run_list").await.unwrap();
        declare_github_op(&pool, "alpha", "pr_comment")
            .await
            .unwrap();
        assert_eq!(
            github_ops(&pool, "alpha").await,
            vec!["pr_comment".to_owned(), "run_list".to_owned()]
        );
        assert!(forget_github_op(&pool, "alpha", "run_list").await.unwrap());
        assert_eq!(
            github_ops(&pool, "alpha").await,
            vec!["pr_comment".to_owned()]
        );
        // And again, which is the answer the routes turn into a 404. Asserted here rather than only
        // through HTTP because this is where `rows_affected` is read.
        assert!(!forget_github_op(&pool, "alpha", "run_list").await.unwrap());

        declare_land_target(&pool, "alpha", "master").await.unwrap();
        // Same idempotency check as `run_list` above, for `declare_land_target`'s own `DO NOTHING`.
        declare_land_target(&pool, "alpha", "master").await.unwrap();
        assert_eq!(
            land_targets(&pool, "alpha").await,
            vec!["master".to_owned()]
        );
        assert!(forget_land_target(&pool, "alpha", "master").await.unwrap());
        assert!(land_targets(&pool, "alpha").await.is_empty());
        assert!(!forget_land_target(&pool, "alpha", "master").await.unwrap());
        // The unforgotten rule reports the same way, so `Ok(false)` is about THIS row and not about
        // the table having gone empty.
        assert!(
            !forget_shell_rule(&pool, "alpha", "never declared")
                .await
                .unwrap()
        );
    }

    /// `shell_rules`' `None` arm and this module's header both lean on the migration's `CHECK`s
    /// actually holding — pins that claim. Raw `sqlx::query`, not `declare_shell_rule`: the point is
    /// the database's own guarantee, not the Rust wrapper that happens to already normalise input
    /// before it would ever reach a `CHECK`.
    #[tokio::test]
    async fn the_check_constraints_reject_what_they_say_they_reject() {
        let pool = pool().await;

        let wrong_case = sqlx::query(
            "INSERT INTO project_shell_rules (project_id, prefix, verdict, created_at)
             VALUES (?, ?, ?, datetime('now'))",
        )
        .bind("alpha")
        .bind("git push")
        .bind("Allow")
        .execute(&pool)
        .await;
        assert!(wrong_case.is_err());

        let empty_prefix = sqlx::query(
            "INSERT INTO project_shell_rules (project_id, prefix, verdict, created_at)
             VALUES (?, ?, ?, datetime('now'))",
        )
        .bind("alpha")
        .bind("")
        .bind("allow")
        .execute(&pool)
        .await;
        assert!(empty_prefix.is_err());
    }
}
