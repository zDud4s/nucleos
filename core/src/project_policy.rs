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
//! One module for four tables because the four answer ONE question — what may this project do
//! without asking — and splitting them would put the same `project_id` resolution in four places.
//!
//! The fourth is the judge, and it is the only one that does not name an OPERATION. The other three
//! say what may be done; this one says WHO may say yes when a conversation on `auto` would
//! otherwise stop and ask a person. Same question, answered one step further back — which is why it
//! belongs here and not beside the conversation settings, where the mode that consults it lives: a
//! judge is a property of the codebase being worked in, not of the chat window open on it.
//!
//! **Every read here fails in the safe direction, and the direction is not the same for all
//! three.** An unreadable GitHub list or land-target list yields nothing, which withholds autonomy
//! — safe. An unreadable shell list cannot do that: yielding an empty `deny` would LOSE a refusal
//! somebody wrote down. So `shell_rules` returns a `Result` and its caller treats the error as
//! "I cannot say this is safe", which is an approval prompt and never an allow.

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
    /// Whether this project declared nothing at all — both lists empty.
    ///
    /// **The one item in this module that only the tests reach**, which is why the suppression is
    /// here and not over the file. It was over the file from the day the module was written, on the
    /// promise that it would come off "the day the last consumer lands"; that day was the page, and
    /// on it exactly one item was still dead. A blanket attribute kept for a single method stops
    /// being a note about unfinished wiring and becomes a place for the next dead item to hide.
    ///
    /// Kept rather than deleted because three assertions read better for it than for
    /// `allow.is_empty() && deny.is_empty()` spelled twice, and because "declared nothing" is a real
    /// question this type should be able to answer when a caller finally asks it. `not(test)` rather
    /// than blanket, for the reason the file-level one gave: the lint stays live under the test
    /// build, so the day this stops being exercised it has to say so.
    #[cfg_attr(not(test), allow(dead_code))]
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

/// One shell rule as the table actually holds it: the two columns the classifier reads, and the two
/// it never asks about.
///
/// `note` and `created_at` are for a PERSON and never for a decision, which is why the deciding path
/// has its own smaller shape. Migration `0128` calls the note "a única defesa contra uma lista que
/// daqui a seis meses ninguém sabe justificar" — a defence that only ever went INTO the table is no
/// defence, and this struct is the way back out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeclaredShellRule {
    pub prefix: String,
    pub verdict: Verdict,
    /// Why, in the words of whoever declared it. `None` is a rule with no justification, which is a
    /// state the column really has and not a read failure.
    pub note: Option<String>,
    /// When this prefix was FIRST declared, in `datetime('now')`'s spelling.
    ///
    /// **Not "last changed", and the difference is `declare_shell_rule`'s doing.** Its `DO UPDATE`
    /// sets `verdict` and `note` and deliberately leaves this column alone, so a rule whose verdict
    /// was flipped this morning still carries the day somebody wrote it down. `declare_github_op`
    /// argues the same point about its own copy of this column, for the same later reader.
    pub created_at: String,
}

/// Every rule this project has declared, whole — the DISPLAY half of `shell_rules`.
///
/// **One query and one reading of `verdict` for both halves.** `shell_rules` is a fold over this
/// function rather than a second `SELECT`, because the two answer about the same rows and the one
/// row where they could disagree is the one that matters most: a `verdict` word neither can parse.
/// A second statement with its own `match` is how the list an owner is shown comes to say `allow`
/// while their runs are being refused — silently, and about precisely the row nobody vetted.
///
/// `Err` when the table could not be read, for the module header's reason: yielding an empty `deny`
/// would lose a refusal somebody wrote down, so this read cannot fail soft in either of its uses.
pub async fn declared_shell_rules(
    pool: &SqlitePool,
    project_id: &str,
) -> Result<Vec<DeclaredShellRule>, String> {
    let rows = sqlx::query_as::<_, (String, String, Option<String>, String)>(
        "SELECT prefix, verdict, note, created_at FROM project_shell_rules
         WHERE project_id = ? ORDER BY prefix",
    )
    .bind(project_id)
    .fetch_all(pool)
    .await
    .map_err(|error| format!("could not read {project_id}'s shell rules: {error}"))?;

    let mut declared = Vec::with_capacity(rows.len());
    for (prefix, verdict, note, created_at) in rows {
        // Folded before the verdict is read rather than after, so no later branch can be added that
        // forgets to. The unreadable-verdict arm below needs the fold as much as the two real
        // verdicts do: the row it turns into a `deny` is precisely the one nobody vetted.
        let prefix = fold_prefix(&prefix);
        let verdict = match Verdict::from_db_str(&verdict) {
            Some(verdict) => verdict,
            // The migration's `CHECK (verdict IN ('allow', 'deny'))` should make this arm
            // unreachable -- but "should" is not "cannot": schema drift, an out-of-band write, or a
            // future migration loosening the constraint could still put an unrecognised word here.
            // Dropping the row, as this used to do, is the PERMISSIVE answer: it silently discards a
            // row that might have been exactly the refusal somebody wrote down. This module's own
            // header says every read here fails toward refusal, so a prefix nobody can read the
            // verdict of is one this module DENIES, not one it forgets.
            None => {
                tracing::warn!(%prefix, %verdict, project_id, "shell rule: unreadable verdict; denied");
                Verdict::Deny
            }
        };
        declared.push(DeclaredShellRule {
            prefix,
            verdict,
            note,
            created_at,
        });
    }
    Ok(declared)
}

/// Both lists, split by verdict — the DECIDING half, and the only shape `classifier` is given.
///
/// The two extra columns are dropped here rather than never fetched, and the cost is a `String` per
/// rule per decision over a list of a handful of rows. What it buys is the paragraph above: the
/// picture and the decision cannot disagree, because there is nothing for them to disagree with.
pub async fn shell_rules(pool: &SqlitePool, project_id: &str) -> Result<ShellRules, String> {
    let mut rules = ShellRules::default();
    for rule in declared_shell_rules(pool, project_id).await? {
        match rule.verdict {
            Verdict::Allow => rules.allow.push(rule.prefix),
            Verdict::Deny => rules.deny.push(rule.prefix),
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
///
/// **What was validated before a row got here depends on the verdict, and the asymmetry is the
/// thing not to tidy up.** `POST /projects/{id}/shell-rules` refuses an `allow` whose prefix fails
/// `classifier::shell_form_is_readable`, because `classify_segment` demands that same guard BEFORE
/// `rules.allows` — so a permission of that shape would be stored and could never authorise
/// anything. A `deny` is not validated, and validating it would be worse than useless:
/// `rules.denies` answers at the LINE level, ahead of the segment loop and with no shape guard
/// above it, so a project really does refuse `tail -f`, `sort -o`, `find . -exec` and `curl … | sh`.
/// Those are precisely the shapes that otherwise stop at `pending_approval` — which makes them the
/// refusals most worth writing down.
///
/// This paragraph lives here rather than in `0128_project_alcada.sql` for a reason worth knowing:
/// `sqlx::migrate!` checksums a migration's whole file, comments included, so a database that has
/// already applied `0128` refuses to start if a single word of it changes. Editing it twice to keep
/// its comments true is what taught us that. A migration's comments must describe what the schema
/// IS; anything that describes what the code does with it belongs next to the code, where it can be
/// corrected.
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

/* --------------------------------------------------------------------------------- the judge -- */

/// Who answers an approval when a conversation on `auto` would otherwise ask a person.
///
/// **Three states, and a nullable column is what makes them tellable apart.** No row at all is the
/// ordinary case and means the local brain with whatever model it is configured for — every project
/// has that without anybody deciding anything. A row naming no brain is somebody having decided the
/// opposite on purpose: nothing answers but a person. A row naming one is a choice.
///
/// Two states would have collapsed the first and second, and the collapse is not cosmetic: it is
/// the difference between "nobody has thought about this" and "somebody thought about it and said
/// no", and only the second should survive a change to what the default is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Judge {
    /// No row. The local brain, on the model it is already configured with.
    Default,
    /// A row naming no brain. Every question goes to the person.
    Off,
    /// A row naming one. `model` absent means that brain's own configured model.
    Named {
        brain: crate::chats::Brain,
        model: Option<String>,
    },
}

/// Reads the judge, and fails toward asking a person.
///
/// **`Off` and not `Default` on an error**, which is the opposite of what this code did when the
/// judge first shipped: it read the row with `.unwrap_or(None)`, so an unreadable table became "no
/// row", and "no row" is the state that GRANTS an automatic approver. A database that cannot be
/// read would have quietly handed out a yes-man. This module's own header says every read here
/// fails in the safe direction; for this table the safe direction is nobody.
///
/// Note which way that cuts and which way it does not. Failing to `Off` withholds autonomy — the
/// person is asked, the turn waits, nothing is approved that would not have been. It cannot cost an
/// approval somebody wrote down, because the thing it withholds is an approval nobody wrote down.
pub async fn judge(pool: &SqlitePool, project_id: &str) -> Judge {
    let row: Result<Option<(Option<String>, Option<String>)>, _> =
        sqlx::query_as("SELECT brain, model FROM project_judge WHERE project_id = ?")
            .bind(project_id)
            .fetch_optional(pool)
            .await;
    match row {
        Ok(None) => Judge::Default,
        Ok(Some((None, _))) => Judge::Off,
        Ok(Some((Some(brain), model))) => Judge::Named {
            brain: crate::chats::Brain::from_wire(&brain),
            model,
        },
        Err(error) => {
            tracing::warn!(%error, %project_id, "reading the judge failed; the person will be asked");
            Judge::Off
        }
    }
}

/// Names this project's judge, or switches it off.
///
/// `None` for the brain is the switch-off, and it is a row rather than the absence of one — see
/// `Judge`. The model travels with the brain and is dropped when there is none, so a switched-off
/// judge cannot keep a stale model to be resurrected by a later half-edit.
///
/// `Brain::Cloud` is refused here rather than left to the CHECK constraint. The constraint would
/// reject it too, but as a database error the caller has to translate — and the reason is worth one
/// sentence in the one place somebody will read it: the cloud route answers through the CLI, and a
/// CLI launched to answer a hook would re-enter that very hook.
pub async fn declare_judge(
    pool: &SqlitePool,
    project_id: &str,
    brain: Option<crate::chats::Brain>,
    model: Option<&str>,
) -> Result<(), String> {
    if brain == Some(crate::chats::Brain::Cloud) {
        return Err(
            "the cloud route answers through the CLI, and a CLI launched to answer a hook would              re-enter that hook — a judge has to be local or openrouter"
                .to_owned(),
        );
    }
    let model = model.map(str::trim).filter(|value| !value.is_empty());
    sqlx::query(
        "INSERT INTO project_judge (project_id, brain, model, created_at)
         VALUES (?, ?, ?, datetime('now'))
         ON CONFLICT (project_id)
         DO UPDATE SET brain = excluded.brain, model = excluded.model",
    )
    .bind(project_id)
    .bind(brain.map(crate::chats::Brain::as_str))
    .bind(if brain.is_none() { None } else { model })
    .execute(pool)
    .await
    .map(|_| ())
    .map_err(|error| format!("could not name a judge for {project_id}: {error}"))
}

/// Takes the row away, putting the project back on the default.
///
/// `Ok(false)` when there was none — a project that never named a judge is already on the default,
/// and saying so is more honest than inventing a deletion.
pub async fn clear_judge(pool: &SqlitePool, project_id: &str) -> Result<bool, String> {
    sqlx::query("DELETE FROM project_judge WHERE project_id = ?")
        .bind(project_id)
        .execute(pool)
        .await
        .map(|done| done.rows_affected() > 0)
        .map_err(|error| format!("could not clear the judge for {project_id}: {error}"))
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

    async fn judged_pool() -> sqlx::SqlitePool {
        let pool = crate::testdb::pool_migrated_through(0).await;
        crate::testdb::apply_migrations_after(&pool, 0).await;
        pool
    }

    /// The three states are distinguishable, and the middle one only exists because a column can be
    /// null. Written as a walk rather than three tests because the point is that they DIFFER.
    #[tokio::test]
    async fn a_project_can_have_a_judge_named_switched_off_or_left_alone() {
        let pool = judged_pool().await;

        assert_eq!(judge(&pool, "nucleos").await, Judge::Default);

        declare_judge(
            &pool,
            "nucleos",
            Some(crate::chats::Brain::Local),
            Some("qwen3"),
        )
        .await
        .unwrap();
        assert_eq!(
            judge(&pool, "nucleos").await,
            Judge::Named {
                brain: crate::chats::Brain::Local,
                model: Some("qwen3".to_owned()),
            }
        );

        // The row is replaced, not added to: one judge per project, and a second naming is a change
        // of mind rather than a second opinion.
        declare_judge(
            &pool,
            "nucleos",
            Some(crate::chats::Brain::OpenRouter),
            None,
        )
        .await
        .unwrap();
        assert_eq!(
            judge(&pool, "nucleos").await,
            Judge::Named {
                brain: crate::chats::Brain::OpenRouter,
                model: None,
            }
        );

        declare_judge(&pool, "nucleos", None, Some("qwen3"))
            .await
            .unwrap();
        assert_eq!(
            judge(&pool, "nucleos").await,
            Judge::Off,
            "a switched-off judge keeps no model to be resurrected by a later half-edit"
        );

        assert!(clear_judge(&pool, "nucleos").await.unwrap());
        assert_eq!(judge(&pool, "nucleos").await, Judge::Default);
        assert!(
            !clear_judge(&pool, "nucleos").await.unwrap(),
            "a project already on the default has no row to take away"
        );
    }

    /// **The security half.** A read that fails must not hand out a judge.
    ///
    /// This is what the code did when the judge first shipped: `.unwrap_or(None)` turned an
    /// unreadable table into "no row", and no row is the state that GRANTS the default local
    /// approver. A database somebody could not read would have quietly appointed a yes-man.
    ///
    /// A pool stopped before `0129` is the real shape of that failure -- `project_judge` genuinely
    /// does not exist -- rather than a mock that returns an error nobody has to believe in.
    #[tokio::test]
    async fn a_judge_that_cannot_be_read_is_nobody_rather_than_the_default() {
        let pool = crate::testdb::pool_migrated_through(128).await;

        assert_eq!(
            judge(&pool, "nucleos").await,
            Judge::Off,
            "withholding a judge costs an approval nobody wrote down; granting one costs the person"
        );
    }

    /// `cloud` is refused where the reason for refusing it can be given.
    ///
    /// The CHECK constraint would reject it too, and as a database error the caller would have to
    /// translate — so this fails earlier and says why, in the words somebody reading the refusal
    /// needs: a CLI launched to answer a hook re-enters that hook.
    #[tokio::test]
    async fn the_cloud_route_cannot_be_a_judge_because_it_would_re_enter_the_hook() {
        let pool = judged_pool().await;

        let refused = declare_judge(&pool, "nucleos", Some(crate::chats::Brain::Cloud), None)
            .await
            .unwrap_err();
        assert!(refused.contains("re-enter"), "{refused}");
        assert_eq!(judge(&pool, "nucleos").await, Judge::Default);
    }

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
    /// It is also what keeps `Verdict` reachable at all: nothing outside this module constructs one
    /// by name, so without this test it is dead under the test build and `clippy -D warnings` is
    /// red. That used to be a note about the module's file-level `allow(dead_code)`; the attribute
    /// is gone now that every item but `ShellRules::is_empty` has a production caller, so the
    /// obligation this test discharges is its own rather than an exception to a blanket.
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

        // And forgetting one takes only that one. `forget_shell_rule` has a production caller now
        // (the DELETE route), so this call is no longer what keeps it alive -- it is here because
        // withdrawing exactly one rule and leaving the rest is the behaviour, and nothing else
        // asserts it.
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

    /// The two halves of one read, held to the same answer about the same rows.
    ///
    /// `declared_shell_rules` carries the note and the day; `shell_rules` is a fold over it. What
    /// this pins is that the fold is a projection and not a second query: the same prefix, folded
    /// the same way, on the same side. A display read that folded differently — or not at all —
    /// would show an owner a `Remove-Item` while their runs are judged against `remove-item`, which
    /// is `fold_prefix`'s bug wearing the display half's clothes.
    #[tokio::test]
    async fn the_display_read_and_the_deciding_read_say_the_same_thing_about_a_rule() {
        let pool = pool().await;
        declare_shell_rule(
            &pool,
            "alpha",
            "Remove-Item  -Recurse",
            Verdict::Deny,
            Some("nothing here deletes recursively"),
        )
        .await
        .unwrap();
        declare_shell_rule(&pool, "alpha", "npm ci", Verdict::Allow, None)
            .await
            .unwrap();

        let declared = declared_shell_rules(&pool, "alpha").await.unwrap();
        assert_eq!(declared.len(), 2);
        // `ORDER BY prefix`, over the folded spelling — the only one stored.
        assert_eq!(declared[0].prefix, "npm ci");
        assert_eq!(declared[0].verdict, Verdict::Allow);
        // A rule declared without one, and the absence is a state rather than a failure.
        assert_eq!(declared[0].note, None);
        assert!(!declared[0].created_at.is_empty());
        assert_eq!(declared[1].prefix, "remove-item -recurse");
        assert_eq!(declared[1].verdict, Verdict::Deny);
        assert_eq!(
            declared[1].note.as_deref(),
            Some("nothing here deletes recursively"),
            "the note is the whole reason this read exists beside the other one"
        );

        let rules = shell_rules(&pool, "alpha").await.unwrap();
        assert_eq!(rules.allow, vec!["npm ci".to_owned()]);
        assert_eq!(rules.deny, vec!["remove-item -recurse".to_owned()]);
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
