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
    pub fn allows(&self, command: &str) -> bool {
        crate::classifier::matches_command_prefix(command, &self.allow)
    }

    pub fn denies(&self, command: &str) -> bool {
        crate::classifier::matches_command_prefix(command, &self.deny)
    }
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

    /// The pair is a round trip, and anything else is `None`. `shell_rules` leans on that `None`
    /// to DROP a malformed row rather than guess at it, so it is the safety property and not a
    /// formality. The migration's `CHECK (verdict IN ('allow', 'deny'))` should stop such a row
    /// ever being written; this is what happens to one that exists anyway.
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
}
