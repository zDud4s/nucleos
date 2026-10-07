//! Whether a feed row may reach a notification channel — as opposed to `notify.rs`, which decides
//! WHEN one goes out. The two questions are independent: this module answers "is this kind
//! wanted at all", `notify.rs` answers "is now a good moment", and a row can be held by one and
//! silenced by the other without either knowing about the second.
//!
//! This module GUARDS, VALIDATES and OBSERVES. It does not resolve the policy against a feed row
//! — that is the sidecar's job (spec §4, §6), on purpose: the núcleo stores a preference, the
//! sidecar is the one place that decides whether to forward a Telegram message, and keeping the
//! resolution there means a filter bug in one channel cannot silence the feed the shell reads.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};

/// A selector no longer than this is trusted to be typed, not pasted. No kind in the núcleo comes
/// close; a longer one is accidental input.
const MAX_SELECTOR_BYTES: usize = 64;

/// The ceiling on rules in one payload. The sidecar rereads the policy every few seconds and the
/// screen that writes it cannot produce more than a few dozen rows; a payload past this is either
/// a bug or abuse, and the check costs one line.
const MAX_RULES: usize = 500;

/// One rule the owner has set: a family prefix or a kind literal, and whether it may reach a
/// channel.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, sqlx::FromRow)]
pub struct Rule {
    pub selector: String,
    pub enabled: bool,
}

/// The whole policy, split by scope the way the table is (§5.1): family rules match by prefix,
/// kind rules match the literal and win over any family.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Policy {
    pub families: Vec<Rule>,
    pub kinds: Vec<Rule>,
}

/// Why a submitted policy was refused. Carries the offending selector so the page can point at
/// the row rather than at the form.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ValidationError {
    EmptySelector {
        scope: &'static str,
    },
    MalformedSelector {
        scope: &'static str,
        selector: String,
    },
    SelectorTooLong {
        scope: &'static str,
        selector: String,
    },
    DuplicateSelector {
        scope: &'static str,
        selector: String,
    },
    TooManyRules {
        count: usize,
    },
}

impl std::fmt::Display for ValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ValidationError::EmptySelector { scope } => {
                write!(f, "a {scope} selector is empty")
            }
            ValidationError::MalformedSelector { scope, selector } => {
                write!(
                    f,
                    "the {scope} selector {selector:?} is not lowercase letters, digits, `_` or `.`"
                )
            }
            ValidationError::SelectorTooLong { scope, selector } => {
                write!(
                    f,
                    "the {scope} selector {selector:?} is longer than {MAX_SELECTOR_BYTES} bytes"
                )
            }
            ValidationError::DuplicateSelector { scope, selector } => {
                write!(f, "the {scope} selector {selector:?} appears twice")
            }
            ValidationError::TooManyRules { count } => {
                write!(
                    f,
                    "{count} rules were submitted, more than the {MAX_RULES} allowed"
                )
            }
        }
    }
}

/// A selector that will never correspond to any kind is not a working rule that does nothing
/// today — it is one that does nothing FOREVER, in silence. `priority.rs`'s `is_known_verdict`
/// refuses an unrecognised value at the door for the same reason: that is the wrong place to
/// discover a typo.
fn is_well_formed(selector: &str) -> bool {
    !selector.is_empty()
        && selector.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_' || byte == b'.'
        })
}

/// Pure and testable isolated from any database. The route validates BEFORE writing; `replace`
/// itself does not call this, both so it keeps one responsibility and so a validation refusal and
/// a database error never share a type.
pub fn validate(policy: &Policy) -> Result<(), ValidationError> {
    let total = policy.families.len() + policy.kinds.len();
    if total > MAX_RULES {
        return Err(ValidationError::TooManyRules { count: total });
    }

    let mut seen: HashSet<(&'static str, String)> = HashSet::new();
    for (scope, rules) in [("family", &policy.families), ("kind", &policy.kinds)] {
        for rule in rules {
            let selector = rule.selector.trim();
            if selector.is_empty() {
                return Err(ValidationError::EmptySelector { scope });
            }
            if !is_well_formed(selector) {
                return Err(ValidationError::MalformedSelector {
                    scope,
                    selector: selector.to_owned(),
                });
            }
            if selector.len() > MAX_SELECTOR_BYTES {
                return Err(ValidationError::SelectorTooLong {
                    scope,
                    selector: selector.to_owned(),
                });
            }
            if !seen.insert((scope, selector.to_owned())) {
                return Err(ValidationError::DuplicateSelector {
                    scope,
                    selector: selector.to_owned(),
                });
            }
            // A kind selector that matches no kind this machine has ever observed is NOT
            // refused here on purpose (spec §5.2): the feed is pruned and `email_<class>` is
            // built at runtime, so validating against what has been seen would make a refusal
            // depend on data that changes, and would delete a legitimate rule for a kind that
            // simply has not shown up in the last 90 days.
        }
    }
    Ok(())
}

/// Reads the stored policy. A fresh database has no rows and this returns the empty `Policy` —
/// the same shape a brand-new install's `GET /notifications/policy` answers, by design (§5.1):
/// an absent row means "passes", not "yes".
pub async fn load(pool: &sqlx::SqlitePool) -> sqlx::Result<Policy> {
    let families: Vec<Rule> = sqlx::query_as(
        "SELECT selector, enabled FROM notify_policy WHERE scope = 'family' ORDER BY selector",
    )
    .fetch_all(pool)
    .await?;
    let kinds: Vec<Rule> = sqlx::query_as(
        "SELECT selector, enabled FROM notify_policy WHERE scope = 'kind' ORDER BY selector",
    )
    .fetch_all(pool)
    .await?;
    Ok(Policy { families, kinds })
}

/// Replaces the whole policy in one transaction: deletes every row and inserts the new set.
///
/// Substitutes rather than merges, because the shell edits a form and saves it whole — a partial
/// write would invite the two halves to drift apart. Does NOT call `validate`; the caller (the
/// route) validates first and only then writes, so an error here is always a database error and
/// never a rejected shape. A failure leaves the table exactly as it was, because the transaction
/// never commits.
pub async fn replace(pool: &sqlx::SqlitePool, policy: &Policy) -> sqlx::Result<()> {
    let mut tx = pool.begin().await?;
    sqlx::query("DELETE FROM notify_policy")
        .execute(&mut *tx)
        .await?;

    let now = chrono::Utc::now().to_rfc3339();
    for (scope, rules) in [("family", &policy.families), ("kind", &policy.kinds)] {
        for rule in rules {
            sqlx::query(
                "INSERT INTO notify_policy (scope, selector, enabled, updated_at) VALUES (?, ?, ?, ?)",
            )
            .bind(scope)
            // Trimmed, because `validate` judged the trimmed form: storing " job_" after
            // approving "job_" would file a rule that can never match anything, and the owner
            // would see a switch that does nothing with no way to tell why.
            .bind(rule.selector.trim())
            .bind(rule.enabled)
            .bind(&now)
            .execute(&mut *tx)
            .await?;
        }
    }

    tx.commit().await
}

/// The kinds this machine has written, deduplicated and ordered — `SELECT DISTINCT kind FROM
/// feed ORDER BY kind`.
///
/// **These are the kinds of the retention window, not of all time.** `feed::prune` deletes
/// anything past `feed::DEFAULT_RETENTION_DAYS` (90, overridable by `NUCLEOS_FEED_RETENTION_DAYS`)
/// once an hour, so a rare kind can be absent here even though a family prefix still covers it —
/// see spec §4.2 for why that gap does not open a hole: a family switch matches future rows by
/// prefix regardless, and a stored kind rule that has fallen off the window is reunited with this
/// list on the shell side, not here.
///
/// No `LIMIT`: the answer's size is bounded by the núcleo's own kind vocabulary (about sixty
/// today), not by the feed's size, and an arbitrary ceiling here would hide families instead of
/// protecting anything.
pub async fn observed_kinds(pool: &sqlx::SqlitePool) -> sqlx::Result<Vec<String>> {
    sqlx::query_scalar("SELECT DISTINCT kind FROM feed ORDER BY kind")
        .fetch_all(pool)
        .await
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn test_pool() -> sqlx::SqlitePool {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                sqlx::sqlite::SqliteConnectOptions::new()
                    .filename(":memory:")
                    .create_if_missing(true),
            )
            .await
            .expect("an in-memory database");
        crate::storage::MIGRATOR
            .run(&pool)
            .await
            .expect("migrations to apply");
        pool
    }

    fn rule(selector: &str, enabled: bool) -> Rule {
        Rule {
            selector: selector.to_owned(),
            enabled,
        }
    }

    #[tokio::test]
    async fn a_fresh_database_has_no_policy_and_changes_nothing() {
        let pool = test_pool().await;

        let policy = load(&pool).await.expect("a fresh policy to load");

        assert_eq!(policy, Policy::default());
        assert!(policy.families.is_empty());
        assert!(policy.kinds.is_empty());
    }

    #[test]
    fn the_five_refusals_each_name_their_selector() {
        let long_selector = "a".repeat(MAX_SELECTOR_BYTES + 1);
        let cases: Vec<(ValidationError, &str)> = vec![
            (ValidationError::EmptySelector { scope: "family" }, "family"),
            (
                ValidationError::MalformedSelector {
                    scope: "kind",
                    selector: "Job Failed".to_owned(),
                },
                "Job Failed",
            ),
            (
                ValidationError::SelectorTooLong {
                    scope: "kind",
                    selector: long_selector.clone(),
                },
                long_selector.as_str(),
            ),
            (
                ValidationError::DuplicateSelector {
                    scope: "family",
                    selector: "job_".to_owned(),
                },
                "job_",
            ),
            (ValidationError::TooManyRules { count: 501 }, "501"),
        ];

        for (error, needle) in cases {
            let message = error.to_string();
            assert!(
                message.contains(needle),
                "{message:?} should mention {needle:?}"
            );
        }

        // And each is reachable through `validate` itself, not just constructible by hand.
        assert_eq!(
            validate(&Policy {
                families: vec![rule("", true)],
                kinds: vec![],
            }),
            Err(ValidationError::EmptySelector { scope: "family" })
        );
        assert_eq!(
            validate(&Policy {
                families: vec![],
                kinds: vec![rule("Job Failed", true)],
            }),
            Err(ValidationError::MalformedSelector {
                scope: "kind",
                selector: "Job Failed".to_owned(),
            })
        );
        assert_eq!(
            validate(&Policy {
                families: vec![],
                kinds: vec![rule(&long_selector, true)],
            }),
            Err(ValidationError::SelectorTooLong {
                scope: "kind",
                selector: long_selector.clone(),
            })
        );
        assert_eq!(
            validate(&Policy {
                families: vec![rule("job_", true), rule("job_", false)],
                kinds: vec![],
            }),
            Err(ValidationError::DuplicateSelector {
                scope: "family",
                selector: "job_".to_owned(),
            })
        );
        let too_many: Vec<Rule> = (0..MAX_RULES + 1)
            .map(|n| rule(&format!("kind_{n}"), true))
            .collect();
        assert_eq!(
            validate(&Policy {
                families: vec![],
                kinds: too_many,
            }),
            Err(ValidationError::TooManyRules {
                count: MAX_RULES + 1
            })
        );

        // A kind selector matching no observed kind is accepted — the feed is pruned and
        // `email_<class>` is built at runtime, so this must not depend on what has been seen.
        assert!(
            validate(&Policy {
                families: vec![],
                kinds: vec![rule("email_whatevercomesnext", true)],
            })
            .is_ok()
        );
    }

    #[tokio::test]
    async fn a_round_trip_and_a_replace_leave_exactly_the_new_set() {
        let pool = test_pool().await;

        let first = Policy {
            families: vec![rule("job_", false), rule("worktree_", true)],
            kinds: vec![rule("job_failed", true)],
        };
        replace(&pool, &first).await.expect("the first write");
        let loaded = load(&pool).await.expect("the first read");
        assert_eq!(loaded, first, "round-trip returns the same set");

        let second = Policy {
            families: vec![rule("job_", false)],
            kinds: vec![],
        };
        replace(&pool, &second).await.expect("the replacing write");
        let loaded = load(&pool).await.expect("the second read");
        assert_eq!(
            loaded, second,
            "replace substitutes rather than merging: `worktree_` and the kind rule are gone"
        );
    }

    /// `validate` judges the trimmed selector, so the write has to store the trimmed one too.
    /// Otherwise a payload the route ACCEPTED files a rule no kind can ever match, and the owner
    /// gets a switch that does nothing with nothing on screen to explain it.
    #[tokio::test]
    async fn a_padded_selector_is_stored_as_the_form_it_was_judged_in() {
        let pool = test_pool().await;
        let padded = Policy {
            families: vec![rule("  job_  ", false)],
            kinds: vec![],
        };

        validate(&padded).expect("the trimmed selector is well formed, so this is accepted");
        replace(&pool, &padded).await.expect("the write");

        assert_eq!(
            load(&pool).await.expect("the read"),
            Policy {
                families: vec![rule("job_", false)],
                kinds: vec![],
            },
            "stored as judged, not as typed"
        );
    }

    #[tokio::test]
    async fn observed_kinds_are_distinct_and_ordered() {
        let pool = test_pool().await;
        for kind in ["worktree_gc", "job_failed", "job_failed", "email_digest"] {
            crate::feed::append(&pool, None, kind, "a summary", None, None)
                .await
                .expect("the feed row to insert");
        }

        let kinds = observed_kinds(&pool).await.expect("kinds to be readable");

        assert_eq!(
            kinds,
            vec!["email_digest", "job_failed", "worktree_gc"],
            "each kind once, alphabetically"
        );
    }
}
