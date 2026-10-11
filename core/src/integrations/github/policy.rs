//! GitHub autonomy policy, runtime configuration and policy digests.

use super::*;

// **Where the map is going: `gh` in Bash stops being autonomous, and `REFUSED_READ_FLAGS` goes with
// it.** Decision #6 of `.ai/specs/2026-09-03-alcada-por-projecto-design.md`, written down here as
// the destination and deliberately not taken.
//
// **It is not a refusal.** A `gh` line an agent needs can still be ASKED for: it becomes a
// `pending_approval` like every other command this house has no opinion about, and a person
// answers. What ends is the line running with nobody asked, on the strength of a prefix.
//
// **`REFUSED_READ_FLAGS` ends with it, because a typed operation has no flags.** That constant
// exists for exactly one reason, and its own doc says so: a prefix cannot spell "`gh run view`
// without `--log`", so the flags had to be refused beside the prefix. A NAME spells it —
// `run_status` and `run_logs` are two entries and the caller picks one — and there is no `--json`
// for a caller to reach for, because `ReadRequest` has three fields and not one of them is a flag.
// The paragraph in that constant about short forms that do not remember themselves is a paragraph
// about a failure mode the typed door does not have.
//
// **What has to be true first**, so that whoever reads this can tell whether the moment has arrived
// instead of deciding that it has:
//
// - **The typed read door has to consult the list.** Today it does not — `submit` runs every read
//   without asking anything, which its own doc states on purpose — so closing Bash now would WIDEN
//   autonomy rather than narrow it: everything Bash refuses would simply be typed instead, and the
//   owner's file would go from half an effect to none. This is the first condition and not one
//   among several.
// - **The catalogue has to cover what people actually type.** Not "cover `READ_CEILING`" — it
//   already does, and `every_read_ceiling_prefix_is_built_by_a_real_operation` is what says so. The
//   evidence lives on the other side: runs that stop and wait for a person on a `gh` line
//   `op_kind_of_gh_command` cannot name. While those keep arriving the catalogue is short, and the
//   answer is one more `ReadOp` — the way `WorkflowList`, `PrDiff`, `PrThread` and `ChecksForRef`
//   each were, one measured gap at a time.
// - **The tests above have to be measuring something other than themselves.**
//   `one_name_picks_out_the_same_operation_at_both_doors` feeds the map the lines the operations
//   build, so it can never discover a line nobody typed. It proves the map is faithful to the
//   catalogue and says nothing about whether the catalogue is wide enough, and it is that second
//   question this step turns on. A green suite is not the signal.
//
// **And until that day, a name from the map is not a grant.** The paragraph above about
// `REFUSED_READ_FLAGS` ending is true of the DESTINATION and false of every state before it.
// `Policy::for_project` consumes the map while the Bash door is still open, and in that interim a
// caller still holds a `gh` line with flags on it — `read_is_autonomous`'s flag check runs AFTER the
// map and not instead of it. The map's own refused-flag guard covers the flags that change which
// operation a line is; the rest of that constant, `--limit` above all, is a bound on HOW MUCH a
// stranger gets to say, and `ReadOp::effect` leans on it by name. Whoever reads this comment while
// implementing the interim step is the person likeliest to delete the only guard there is.
//
// The day all three hold, the change is small and almost entirely deletion: `READ_CEILING` and
// `REFUSED_READ_FLAGS` go, `read_is_autonomous` goes with them, and `classifier.rs::Segment::
// GithubRead` has nothing left to classify. **The map above goes too**, and that is not a loss but
// the shape of the thing: it exists to read a `gh` command line, and after this step no `gh`
// command line is being read for autonomy — which is also why its refused-flag guard may not
// outlive the constant it consults. That the step is a deletion is the point. It is the proof the
// two vocabularies really did become one, and until then this comment is a plan and not an
// achievement.

/// PURE: the widest policy this codebase can construct — both compiled ceilings, entire.
///
/// It exists so that "inside the ceiling" is asked in exactly the way production asks it, of the
/// same two functions, rather than by a second comparison somebody would have to keep in step.
pub(super) fn ceiling_policy() -> Policy {
    Policy::from_config(&crate::config::GithubConfig {
        enabled: true,
        autonomous_reads: READ_CEILING
            .iter()
            .map(|entry| (*entry).to_owned())
            .collect(),
        autonomous_actions: ACTION_CEILING
            .iter()
            .map(|entry| (*entry).to_owned())
            .collect(),
    })
}

/// PURE: the READ operations a project may declare, by name.
///
/// **Derived, never written out.** The kinds come off `ReadOp::all()` through `kind()`, and
/// admissibility is asked of `ceiling_policy` — the same question production asks of the owner's
/// file. A hand-written list would be a second spelling of a set that already exists, and a second
/// spelling is how a set drifts.
///
/// Matched on the COMMAND each operation builds and not on its kind, because `READ_CEILING` is
/// written in `gh` prefixes and this half has to be asked in its own vocabulary. That is also what
/// excludes `run_logs`, whose argv carries `--log`: `REFUSED_READ_FLAGS` refuses it here exactly as
/// it refuses the same flag typed into Bash. `pr_view` is excluded the same way and just as
/// deliberately — `READ_CEILING`'s own doc refuses it in words.
///
/// Built once, for `gh_forms`' reason: its caller is the decision path.
pub(super) fn declarable_read_ops() -> &'static [&'static str] {
    static KINDS: std::sync::OnceLock<Vec<&'static str>> = std::sync::OnceLock::new();
    KINDS.get_or_init(|| {
        let ceiling = ceiling_policy();
        ReadOp::all()
            .iter()
            .filter(|operation| {
                ceiling.read_is_autonomous(&format!("gh {}", operation.argv().join(" ")))
            })
            .map(|operation| operation.kind())
            .collect()
    })
}

/// PURE: the ACTIONS a project may declare, by name.
///
/// `api_read` is outside `ACTION_CEILING`, so it is outside this, so a project cannot declare it —
/// the ceiling holding across a route that did not exist when it was written.
pub(super) fn declarable_act_ops() -> &'static [&'static str] {
    static KINDS: std::sync::OnceLock<Vec<&'static str>> = std::sync::OnceLock::new();
    KINDS.get_or_init(|| {
        let ceiling = ceiling_policy();
        ActOp::all()
            .iter()
            .filter(|operation| ceiling.action_is_autonomous(operation.kind()))
            .map(|operation| operation.kind())
            .collect()
    })
}

/// Every operation kind a project may declare, reads first and actions after.
///
/// One list, because decision #4 gives the owner one list of NAMES. Two halves underneath, because
/// decision #5 keeps the partition in the TYPES and each door may only ever be answered by its own
/// half: `Policy::for_project` narrows against this list and then splits on `declarable_read_ops`,
/// so both decisions are honoured, in that order, by construction rather than by a check anyone
/// could forget.
pub fn declarable_ops() -> Vec<&'static str> {
    declarable_read_ops()
        .iter()
        .chain(declarable_act_ops())
        .copied()
        .collect()
}

/// Where one operation stands against the ceilings.
///
/// `declarable` is the same question `declarable_ops` answers and asked of the same two functions —
/// this is not a second derivation, it is that one carrying the operations it says no to as well as
/// the ones it says yes to.
pub struct OpStanding {
    /// The typed name, as `op_kind` is written in `project_github_ops` and in the POST body.
    pub kind: &'static str,
    /// Which door this operation goes through — `"read"` or `"action"`, the two words
    /// `config::GithubConfig` already uses for its own two lists.
    ///
    /// It is here because the two halves are not the same promise once declared, and only the
    /// núcleo knows which is which. A declared READ binds the very next decision:
    /// `Policy::for_project` splits the stored rows on `declarable_read_ops`, and the reading half
    /// is already consulted through the Bash door. A declared ACTION is recorded and inert until a
    /// later step wires it. A surface that could not tell them apart would either have to hardcode
    /// the names — the drift this whole function exists to end — or promise an owner their declared
    /// action is in force.
    pub half: &'static str,
    /// Whether a project may declare it. `false` is a fact about this build and not a state
    /// anything can change: it means the compiled ceilings do not admit the operation, so no route,
    /// no file and no owner can turn it on.
    pub declarable: bool,
}

/// PURE: every operation this núcleo can build, and whether a project may declare each one.
///
/// **The ones it says NO to are the reason this exists.** `declarable_ops` is the set a POST
/// accepts, and until this function that set reached the wire only inside an `undeclarable_op`
/// refusal — so a surface offering a picker had to hardcode the list and drift from the ceilings in
/// silence, or discover it by POSTing something invalid. Serving only the admitted names would have
/// fixed the picker and left the other half unsayable: `api_read` is outside `ACTION_CEILING` and
/// cannot be switched on by anybody, and that is a FACT to state rather than a control to draw. A
/// checkbox that did nothing would be a lie about who decides.
///
/// Reads first and actions after, which is `Op::all`'s order and `declarable_ops`' — one list, in
/// the order decision #5 keeps the partition in.
///
/// Not memoised, unlike the two halves it consults: its only caller is a route a person opens a
/// page against, where `declarable_ops`' caller is the decision path.
pub fn every_op() -> Vec<OpStanding> {
    let declarable = declarable_ops();
    Op::all()
        .iter()
        .map(|operation| OpStanding {
            kind: operation.kind(),
            half: match operation {
                Op::Read(_) => "read",
                Op::Act(_) => "action",
            },
            declarable: declarable.contains(&operation.kind()),
        })
        .collect()
}

/// What runs without asking.
///
/// Built from `~/.nucleos/github.yaml` at startup as the MACHINE default, and immutable once built — it
/// does no I/O after construction, which is what lets `classifier::classify` take it by reference
/// and stay pure. There is deliberately no hot reload: a policy a run could reload is a policy a run
/// could change in the middle of itself.
///
/// **A project's own policy is a second value, not a mutation of this one.** `for_project` reads
/// `project_github_ops` and returns a NEW `Policy` with the project's operations laid over the
/// machine default; the value handed to a decision is built for that decision and thrown away
/// after. The immutability above survives intact — what changed is that there is now more than one
/// of these, and §4.4 of `.ai/specs/2026-09-03-alcada-por-projecto-design.md` is where that was
/// decided and where the "read at the moment of the decision, with no cache" rule is argued.
///
/// **Every list here is an intersection with a compiled ceiling, and never a union with one.**
/// Configuration chooses inside what the code fixes, and a project's table is configuration exactly
/// as the file is. `.ai/` is gitignored and travels with nobody; a row in a database travels with
/// nobody either. Neither may be the only thing between an autonomous run and `gh api -X DELETE`.
///
/// It answers WHETHER, never HOW: `execute` builds the argv and this type never sees one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Policy {
    /// The machine's autonomous reads, as `gh` PREFIXES, narrowed to `READ_CEILING`.
    pub(super) reads: Vec<String>,
    /// This project's autonomous reads, as operation NAMES, narrowed to `declarable_read_ops`.
    ///
    /// A second field and not more entries in `reads`, because the two are different vocabularies
    /// answering the same question — which is §1.2's whole complaint — and a name is the one of the
    /// two that can tell `run_status` from `run_logs`. `op_kind_of_gh_command` is what turns a
    /// command line into something this list can be asked about.
    ///
    /// It holds only READS. An action's name never reaches it, so `gh pr comment` cannot come
    /// through the reading door on the strength of a project having declared `pr_comment` for the
    /// typed one.
    pub(super) read_ops: Vec<String>,
    /// Autonomous actions, by kind, narrowed to `ACTION_CEILING`. The machine's and the project's
    /// in ONE list, because for actions the two vocabularies already agree: both are kinds.
    pub(super) actions: Vec<String>,
    pub(super) digest: String,
}

impl Policy {
    /// Autonomous in nothing — what an absent, unreadable or malformed file produces, and what every
    /// test that is not about the policy itself should be given.
    pub fn empty() -> Self {
        Self {
            reads: Vec::new(),
            read_ops: Vec::new(),
            actions: Vec::new(),
            digest: digest_of(&[], &[], &[]),
        }
    }

    /// Narrows the owner's two lists to the two ceilings.
    ///
    /// An entry outside a ceiling is **dropped with a warning**: neither ignored in silence (the
    /// owner would hold a policy other than the one they believe they wrote) nor fatal (a typo may
    /// not take the daemon down). The rest of the file stays valid, because one bad line is a
    /// mistake and not a reason to discard the good ones.
    ///
    /// `enabled: false` collapses both lists rather than being carried as a third state. What this
    /// type answers is "does it run without asking", and a pillar the owner switched off answers no
    /// to that in exactly the way an empty list does. A project's rows must not undo that, and they
    /// cannot: `GithubRuntime::policy_for_project` is the door the daemon uses and it is where the
    /// switch is read, because by the time a `Policy` exists the two states are the same value.
    ///
    /// **This is the MACHINE default and it is unchanged by the per-project work.** It declares no
    /// `read_ops` — that field is a project's alone — so a machine with no projects, or a run with
    /// no project, classifies exactly as it did before `for_project` existed.
    pub fn from_config(config: &crate::config::GithubConfig) -> Self {
        if !config.enabled {
            return Self::empty();
        }
        let reads = narrow(&config.autonomous_reads, READ_CEILING, "read");
        let actions = narrow(&config.autonomous_actions, ACTION_CEILING, "action");
        let digest = digest_of(&reads, &[], &actions);
        Self {
            reads,
            read_ops: Vec::new(),
            actions,
            digest,
        }
    }

    /// This project's policy: the machine default with the project's declared operations laid over
    /// it.
    ///
    /// **It ADDS to the machine default; it does not replace it**, and the argument is the table's
    /// own vocabulary. `project_github_ops` holds only operations somebody switched ON — there is no
    /// `deny` here and no room for one, because presence in the table IS the grant. A list that can
    /// only say yes cannot say "and take that other one away", so reading it as a REPLACEMENT would
    /// hand it a power it has no words for: withdrawing a machine-wide grant would happen as a side
    /// effect of declaring something unrelated, and the owner would have no way to write the
    /// opposite down. The shell rules can replace, and that is precisely why they carry a `deny`.
    ///
    /// The empty case then needs no special rule, which is the second half of the argument. Under a
    /// union, a project that declared nothing IS the machine default, and this returns a value equal
    /// to the one it was called on — the non-regression the shell-rules side answered with "empty
    /// means today's behaviour", arrived at here as a consequence instead of as an exception. Read
    /// as a replacement it would have to be an exception, and an exception at zero rows is a cliff
    /// at one: declare a single operation and every machine-wide grant would vanish unannounced.
    ///
    /// **Narrowed on the way out, though `POST /projects/{id}/github-ops` already validated on the
    /// way in.** Two writes the route never saw can reach that table: a row stored before a ceiling
    /// was narrowed, and a row written out of band. The shell-rules side made this argument for its
    /// own fold and it is the stronger one here, because the cost is not a dead rule but a live
    /// grant — `api_read` in a row would otherwise name `gh api -X DELETE` through the map and run
    /// it unasked. The read side is the side that decides, so the read side checks.
    ///
    /// A row outside the ceiling is dropped with a warning and the rest stay valid, which is
    /// `narrow`'s rule and §6 of the design restating it: one bad line is a mistake, not a reason to
    /// discard the good ones.
    ///
    /// **The acting half of the overlay reaches nothing yet, and that is worth knowing before
    /// reading it as live.** `action_is_autonomous` has exactly one production caller — `submit`,
    /// which reads `runtime.policy`, the machine default. `POST /github/requests` is the only way
    /// into `submit`, its body is `{op}` and nothing else, and `github_caller_is_allowed` admits only
    /// the control token and an Admin key — so there is no project on that path, and no run id to
    /// resolve one from either. Naming the project in the body would make it a CLAIM the caller
    /// makes about itself, which is the shape `pretooluse_decision` had to take apart: a caller free
    /// to name any project could borrow that project's grants. That is a decision the design did not
    /// take and this is not the place to take it.
    ///
    /// Merged anyway, and deliberately. The route accepts action kinds, the table stores them, and
    /// `digest` has to say what the effective policy IS rather than what happens to be consulted; on
    /// the day `submit` is handed a project, nothing here changes. Until then a project's declared
    /// ACTIONS are recorded and inert, while its declared READS are live through Bash.
    ///
    /// **One visible consequence of merging them, so nobody has to discover it from a graph.** A
    /// declared action moves the digest — `actions` is always hashed — while changing no verdict
    /// anywhere, so declaring `pr_create` fragments that project's shadow sample for a grant that
    /// does nothing yet. It is the honest answer rather than a bug: the configuration genuinely did
    /// change, and a digest that hid the change would be lying about which policy a row was recorded
    /// under the moment `submit` learns its project. The cost is bounded to
    /// `shadow_readiness` counting one more distinct digest for that project.
    ///
    /// **PRIVATE, and that is the `enabled` hole closed by construction rather than by comment.**
    /// `GithubRuntime::policy_for_project` is the only way in, and it is where the switch is read;
    /// a `pub` constructor here would let a caller outside this module lay a project's rows straight
    /// onto a policy that a switched-off pillar had already collapsed to `empty()`, which is the one
    /// thing `enabled: false` exists to prevent. The tests below still reach it, being in-module.
    pub(super) async fn for_project(&self, pool: &sqlx::SqlitePool, project_id: &str) -> Self {
        // The SWALLOWING reader, and this is the consumer it was written for. An unreadable table
        // yields no operations, an empty overlay is the machine default, and that is strictly FEWER
        // operations running unasked — the safe direction here. It is the opposite of the shell
        // `deny`, where an empty list would lose a refusal somebody wrote down and the `Result`
        // reader is the only honest one; `project_policy` carries both halves for exactly this
        // reason, and picking the wrong one is how a database hiccup becomes a permission.
        let declared = crate::project::policy::github_ops(pool, project_id).await;
        // One narrowing against the whole declarable set, so a row that is in neither half warns
        // once and by name...
        let kept = narrow(&declared, &declarable_ops(), "project operation");
        // ...and then the partition decision #5 keeps in the types, applied here so that each door
        // holds only what it may answer for.
        //
        // **A `partition` and not two intersections, and that is only exact because of the line
        // above.** `declarable_ops` IS the two halves concatenated, and the halves are disjoint
        // because `ReadOp` and `ActOp` are — `read_ops_and_act_ops_partition_op` is the test that
        // says so — so after `narrow`, "not a read" and "an action" are the same set. Take `narrow`
        // away and they stop being: `api_read` would fall into the acting half and be autonomous
        // there. That is the one ceiling check, deliberately in one place, and this is the sentence
        // that says what depends on it.
        let (read_ops, acted): (Vec<String>, Vec<String>) = kept
            .into_iter()
            .partition(|kind| declarable_read_ops().contains(&kind.as_str()));
        let mut actions = self.actions.clone();
        actions.extend(acted);
        actions.sort();
        actions.dedup();
        let digest = digest_of(&self.reads, &read_ops, &actions);
        Self {
            reads: self.reads.clone(),
            read_ops,
            actions,
            digest,
        }
    }

    /// The effective lists, for the tests that assert what the ceilings let through. Production
    /// asks the two questions below instead, which is why these carry the allow.
    pub fn autonomous_reads(&self) -> &[String] {
        &self.reads
    }

    pub fn autonomous_actions(&self) -> &[String] {
        &self.actions
    }

    /// A short, stable fingerprint of the EFFECTIVE policy — already narrowed, sorted and
    /// deduplicated. `CLASSIFIER_VERSION` goes on meaning *the code*; this means *the configuration*.
    pub fn digest(&self) -> &str {
        &self.digest
    }

    /// Whether this shell command runs without asking.
    ///
    /// **The raw command, not a normalized one, and that is load-bearing.** `normalize_command`
    /// lowercases, and `-L` is `gh`'s short `--limit` while `-l` is its short `--label`: folded to
    /// one case the list would either miss the first or refuse the second. The same reason
    /// `lands_inside_the_workspace` reads raw tokens.
    ///
    /// Tokenized with `classifier::shell_words` — the tokenizer already in the house, rather than a
    /// second one that would drift from it. Quotes are stripped by it, so
    /// `--search 'in:body x'` is two tokens and the flag is seen.
    ///
    /// A refused flag is matched by `word_is_flag`, which is that rule and lives beside the map that
    /// also needs it — see its own doc for why one rule may not have two spellings here.
    ///
    /// **This decides the lists and the flags and nothing else.** Whether the line is a single
    /// command at all, whether it redirects, whether it hides a second command behind a separator —
    /// those are `classifier.rs`'s guards, applied before this is ever consulted, and this function
    /// would be wrong to be read as covering them.
    ///
    /// **Two doors, one flag check, and the ORDER is the whole safety argument.** The machine's list
    /// is asked by prefix and the project's by name, but `REFUSED_READ_FLAGS` runs before either and
    /// binds both. `op_kind_of_gh_command` has a refused-flag guard of its own and it is not this
    /// one: it covers the flags that change WHICH operation a line is, so that `--log-failed` cannot
    /// be named `run_status`. The rest of the constant — `--limit` above all — bounds HOW MUCH a
    /// stranger gets to say, and `ReadOp::effect` leans on it by name: `PrList` is graded `ReadsOwn`
    /// because a stranger reaches it through thirty short fields and no further. A name from the map
    /// is not a grant, and a mapped name allowed to skip this check would take the grading with it.
    pub fn read_is_autonomous(&self, command: &str) -> bool {
        if self.reads.is_empty() && self.read_ops.is_empty() {
            return false;
        }
        let words = crate::governance::classifier::shell_words(command);
        if words.iter().any(|word| {
            REFUSED_READ_FLAGS
                .iter()
                .any(|flag| word_is_flag(word, flag))
        }) {
            return false;
        }
        let normalized = words.join(" ").to_ascii_lowercase();
        if self
            .reads
            .iter()
            .any(|prefix| normalized == *prefix || normalized.starts_with(&format!("{prefix} ")))
        {
            return true;
        }
        // The named door. `read_ops` holds no action, so this cannot answer for one however the line
        // is spelled — see the field's own doc.
        //
        // Skipped outright when there is no name to ask about, which is EVERY machine policy — and
        // that is a cost and not only a tidiness. This runs per segment of every shell line a hook
        // sees, and `op_kind_of_gh_command` tokenizes the segment a second time before it can even
        // tell that the program is not `gh`. A daemon whose projects declared nothing goes on paying
        // exactly what it paid before this field existed.
        if self.read_ops.is_empty() {
            return false;
        }
        op_kind_of_gh_command(command).is_some_and(|kind| self.read_ops.iter().any(|op| op == kind))
    }

    /// Whether an operation of this `kind()` is executed without asking. Everything else becomes a
    /// proposal a person approves, and the turn carries on either way.
    ///
    /// One list for both authors: an action the machine granted and an action the project declared
    /// are the same sentence in the same vocabulary, so `for_project` merges rather than adding a
    /// second field. The reading half could not do that, and its field says why.
    pub fn action_is_autonomous(&self, kind: &str) -> bool {
        self.actions.iter().any(|allowed| allowed == kind)
    }
}

impl Default for Policy {
    fn default() -> Self {
        Self::empty()
    }
}

/// The pillar as the daemon holds it: whether the owner switched it on, and what runs without
/// asking. Resolved once at startup and read-only afterwards, like `WebRuntime` and
/// `BrowserRuntime` beside it in `AppState`.
///
/// `enabled` is kept even though `Policy` already collapses to empty when it is false, and the
/// reason is `health.rs`: a pillar the owner switched off is `NotConfigured` and a pillar with an
/// empty list is working as intended. Those are different sentences to read at two in the morning,
/// and only one of them is a problem.
///
/// `Default` is what an ABSENT `~/.nucleos/github.yaml` produces — on, and autonomous in nothing — so a
/// test that does not care about GitHub gets the shipped state rather than an invented one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GithubRuntime {
    pub enabled: bool,
    /// Whether `~/.nucleos/github.yaml` EXISTS. Not whether it says anything useful.
    ///
    /// It is here because `enabled` defaults to true, so `enabled` alone can no longer answer "did
    /// anybody ask for this pillar" — and `health.rs` needs that answer or every machine that has
    /// never heard of GitHub reports a fault. **Writing the file is the opt-in; its contents are the
    /// autonomy**, and those are two different decisions the owner makes at two different times.
    ///
    /// The inverse of the distinction `config.rs` draws about a working capability "as opposed to a
    /// file that merely exists": there a file that exists proves nothing, and here it is the only
    /// thing that proves anything.
    pub configured: bool,
    /// The `gh` executable, resolved once at startup from `NUCLEOS_GH_BIN`.
    ///
    /// Overridable for the reason `NUCLEOS_CLAUDE_BIN` exists, and it is the same reason rather than
    /// a borrowed one: on Windows a CLI installed through npm is a `.cmd` shim that Rust's `Command`
    /// cannot spawn by name. `health.rs::binary_candidate` deliberately refuses `.cmd` candidates
    /// too, so without this a machine where `gh` works in the console would report `Missing` and
    /// nothing would say why.
    ///
    /// It lives HERE and is not read from the environment per call, which matters twice. Startup is
    /// where every other pillar resolves its configuration, so a run cannot change what the daemon
    /// spawns halfway through itself. And a test can hand this a stub program without mutating
    /// process-global state, which in Rust 2024 is `unsafe` and races every other test in the
    /// binary.
    pub binary: String,
    pub policy: Policy,
}

impl Default for GithubRuntime {
    fn default() -> Self {
        Self {
            enabled: true,
            configured: false,
            binary: "gh".to_owned(),
            policy: Policy::empty(),
        }
    }
}

impl GithubRuntime {
    /// `configured` is passed in rather than derived, because whether the file exists is a fact
    /// about the disk and this module does no I/O to find out. `main.rs` knows the path it just
    /// read and is the one place that should.
    pub fn from_config(
        config: &crate::config::GithubConfig,
        configured: bool,
        binary: String,
    ) -> Self {
        Self {
            enabled: config.enabled,
            configured,
            binary,
            policy: Policy::from_config(config),
        }
    }

    /// The policy a decision about THIS project is taken under: the machine default with the
    /// project's declared operations laid over it.
    ///
    /// **The door the daemon uses, and the one place the switch is read.** `Policy::for_project`
    /// does the layering and knows nothing about `enabled`, because by the time a `Policy` exists a
    /// switched-off pillar and an empty pair of lists are the same value — `from_config` collapses
    /// one into the other on purpose. Layering onto that collapsed value would let a project's row
    /// turn back on a pillar the owner switched off, which is the one thing `enabled: false` is for.
    /// Kept here rather than at the call sites so that neither `hooks.rs` nor `runs.rs` has to
    /// remember it.
    pub async fn policy_for_project(&self, pool: &sqlx::SqlitePool, project_id: &str) -> Policy {
        if !self.enabled {
            return Policy::empty();
        }
        self.policy.for_project(pool, project_id).await
    }
}

/// PURE: one list intersected with its ceiling, sorted and deduplicated, warning about each entry it
/// had to drop.
///
/// Written for the owner's file and used unchanged for a project's rows, which is why the warning
/// says "config" rather than "file": a row in `project_github_ops` is configuration too, and it
/// reaches this by the same route and for the same reason.
pub(super) fn narrow(asked: &[String], ceiling: &[&str], what: &str) -> Vec<String> {
    let mut kept: Vec<String> = Vec::new();
    for entry in asked {
        let entry = entry.trim();
        if ceiling.contains(&entry) {
            kept.push(entry.to_owned());
        } else {
            tracing::warn!(
                entry = %entry,
                kind = %what,
                "github config: outside the compiled ceiling; dropped, and it will keep asking"
            );
        }
    }
    kept.sort();
    kept.dedup();
    kept
}

/// PURE: FNV-1a over the effective policy, rendered as sixteen hex characters.
///
/// **Not a cryptographic hash, and it does not need to be.** What this labels is a scoreboard row,
/// so the property required is that two different effective policies get different labels and one
/// policy gets the same label everywhere. Nothing here defends against a chosen collision: the text
/// being hashed is the owner's own file, already narrowed by the ceilings.
///
/// Hand-written rather than `DefaultHasher`, and that is the reason it exists as ten lines instead
/// of two: `DefaultHasher`'s algorithm is explicitly allowed to change between Rust releases, and a
/// toolchain upgrade that silently renumbered every digest would fragment the very scoreboard this
/// is for. FNV-1a is fixed forever and costs no dependency.
///
/// The input is the effective policy and NOT the file, so a comment edit or a reordering does not
/// break the scoreboard — which is the whole reason `§7` asked for it normalized before hashed. A
/// switched-off pillar and an empty pair of lists hash alike, and they should: they are the same
/// effective policy.
///
/// **`read_ops` is written only when there IS one, and that conditional is the same sentence again
/// rather than a special case.** A project that declared nothing has the machine's effective policy,
/// so it must carry the machine's label; appending an empty field instead would renumber every
/// digest ever recorded, and `shadow::shadow_readiness` would then warn that one action class spans
/// two policies on machines where nothing changed. A project's declared ACTIONS need no such care —
/// they merge into `actions`, so they move the digest through a field that was always written.
pub(super) fn digest_of(reads: &[String], read_ops: &[String], actions: &[String]) -> String {
    let mut text = format!(
        "v1\nreads={}\nactions={}",
        reads.join(","),
        actions.join(",")
    );
    if !read_ops.is_empty() {
        text.push_str(&format!("\nread_ops={}", read_ops.join(",")));
    }
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in text.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")
}
