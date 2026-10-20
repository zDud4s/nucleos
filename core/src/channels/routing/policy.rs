//! Per-channel notification policy (Anexo D D.12, Anexo E). A port of the Telegram sidecar's
//! `notifier/policy.go`: the stored default rows (`notify_policy`) merged with the rows one channel
//! overrides (`channel_notify_policy`), and the pure decision "may this feed kind be announced".

use chrono::{DateTime, Utc};
use nucleos_base::notify_policy::{self, Policy, Rule};
use sqlx::SqlitePool;

#[derive(Debug)]
pub enum PolicyError {
    Invalid(notify_policy::ValidationError),
    Db(sqlx::Error),
}

impl From<sqlx::Error> for PolicyError {
    fn from(error: sqlx::Error) -> Self {
        PolicyError::Db(error)
    }
}

/// Whether a feed kind may be announced. An exact kind rule wins; otherwise the longest matching
/// family prefix decides; otherwise the kind passes. Pure.
///
/// Fail-open on purpose: the empty policy passes everything, an empty kind passes (it matches no
/// family and reaches the default), and a rule with an empty selector is ignored (the empty prefix
/// would match everything and mute the channel). The mechanism guards against noise, so its own
/// failure must never be silence.
pub fn passes(policy: &Policy, kind: &str) -> bool {
    for rule in &policy.kinds {
        if rule.selector.is_empty() {
            continue;
        }
        if rule.selector == kind {
            return rule.enabled;
        }
    }

    let mut best: Option<usize> = None;
    let mut allowed = true;
    for rule in &policy.families {
        if rule.selector.is_empty() || !kind.starts_with(rule.selector.as_str()) {
            continue;
        }
        // Ties cannot happen: a duplicate selector is a validation refusal and the table has a
        // unique index on (channel, scope, selector).
        if best.is_none_or(|len| rule.selector.len() > len) {
            best = Some(rule.selector.len());
            allowed = rule.enabled;
        }
    }
    if best.is_some() { allowed } else { true }
}

/// The channel's own override rows, in the order they were stored.
pub async fn channel_overrides(pool: &SqlitePool, channel: &str) -> sqlx::Result<Policy> {
    let rows: Vec<(String, String, bool)> = sqlx::query_as(
        "SELECT scope, selector, enabled FROM channel_notify_policy WHERE channel = ? ORDER BY id",
    )
    .bind(channel)
    .fetch_all(pool)
    .await?;
    let mut policy = Policy::default();
    for (scope, selector, enabled) in rows {
        let rule = Rule { selector, enabled };
        if scope == "family" {
            policy.families.push(rule);
        } else {
            policy.kinds.push(rule);
        }
    }
    Ok(policy)
}

fn merge(rules: &mut Vec<Rule>, overrides: Vec<Rule>) {
    for over in overrides {
        match rules.iter_mut().find(|rule| rule.selector == over.selector) {
            Some(existing) => existing.enabled = over.enabled,
            None => rules.push(over),
        }
    }
}

/// The default rows, merged with the channel's overrides when a channel is named. The channel's
/// row wins for the same `(scope, selector)`; selectors only the channel has come after, in the
/// order they were stored.
pub async fn resolved_policy(pool: &SqlitePool, channel: Option<&str>) -> sqlx::Result<Policy> {
    let mut policy = notify_policy::load(pool).await?;
    if let Some(channel) = channel {
        let overrides = channel_overrides(pool, channel).await?;
        merge(&mut policy.families, overrides.families);
        merge(&mut policy.kinds, overrides.kinds);
    }
    Ok(policy)
}

/// Replaces every override row of `channel` in one transaction. Validates FIRST, so a refusal
/// writes nothing. The empty policy clears the overrides (the channel inherits the defaults).
pub async fn put_channel_policy(
    pool: &SqlitePool,
    channel: &str,
    policy: &Policy,
    now: DateTime<Utc>,
) -> Result<(), PolicyError> {
    notify_policy::validate(policy).map_err(PolicyError::Invalid)?;

    let mut tx = pool.begin().await?;
    sqlx::query("DELETE FROM channel_notify_policy WHERE channel = ?")
        .bind(channel)
        .execute(&mut *tx)
        .await?;
    let stamp = now.to_rfc3339();
    for (scope, rules) in [("family", &policy.families), ("kind", &policy.kinds)] {
        for rule in rules {
            // Trimmed, because `validate` judged the trimmed form.
            sqlx::query(
                "INSERT INTO channel_notify_policy (channel, scope, selector, enabled, updated_at)
                 VALUES (?, ?, ?, ?, ?)",
            )
            .bind(channel)
            .bind(scope)
            .bind(rule.selector.trim())
            .bind(rule.enabled)
            .bind(&stamp)
            .execute(&mut *tx)
            .await?;
        }
    }
    tx.commit().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(selector: &str, enabled: bool) -> Rule {
        Rule {
            selector: selector.to_owned(),
            enabled,
        }
    }

    #[test]
    fn passes_longest_family_prefix_and_exact_kind() {
        let policy = Policy {
            families: vec![rule("job_", false), rule("job_fail", true)],
            kinds: vec![rule("job_failed_hard", false)],
        };
        // The longest family prefix decides.
        assert!(passes(&policy, "job_failed"));
        assert!(!passes(&policy, "job_done"));
        // An exact kind rule beats any family.
        assert!(!passes(&policy, "job_failed_hard"));
        let policy = Policy {
            families: vec![rule("job_", false)],
            kinds: vec![rule("job_done", true)],
        };
        assert!(passes(&policy, "job_done"));
        assert!(!passes(&policy, "job_other"));
        // Unclaimed kinds pass.
        assert!(passes(&policy, "email_digest"));
    }

    #[test]
    fn passes_empty_selector_and_empty_kind_fail_open() {
        let policy = Policy {
            families: vec![rule("", false)],
            kinds: vec![rule("", false)],
        };
        assert!(passes(&policy, "job_done"));
        assert!(passes(&policy, ""));
        let muted = Policy {
            families: vec![rule("job_", false)],
            kinds: vec![],
        };
        assert!(passes(&muted, ""));
        assert!(passes(&Policy::default(), "anything"));
    }

    #[tokio::test]
    async fn resolved_policy_channel_row_wins_for_same_selector() {
        let pool = crate::testdb::fresh_pool().await;
        notify_policy::replace(
            &pool,
            &Policy {
                families: vec![rule("job_", false), rule("email_", true)],
                kinds: vec![rule("job_done", false)],
            },
        )
        .await
        .unwrap();
        put_channel_policy(
            &pool,
            "fake",
            &Policy {
                families: vec![rule("job_", true), rule("zeta_", false)],
                kinds: vec![rule("alpha", false)],
            },
            Utc::now(),
        )
        .await
        .unwrap();

        // Defaults come back ordered by selector ("email_" before "job_"); channel-only
        // selectors follow.
        let resolved = resolved_policy(&pool, Some("fake")).await.unwrap();
        assert_eq!(
            resolved.families,
            vec![
                rule("email_", true),
                rule("job_", true),
                rule("zeta_", false)
            ]
        );
        assert_eq!(
            resolved.kinds,
            vec![rule("job_done", false), rule("alpha", false)]
        );

        // Another channel and the no-channel form see only the defaults.
        let other = resolved_policy(&pool, Some("other")).await.unwrap();
        assert_eq!(other, resolved_policy(&pool, None).await.unwrap());
        assert!(!passes(&other, "job_x"));
        assert_eq!(
            channel_overrides(&pool, "fake").await.unwrap().families,
            vec![rule("job_", true), rule("zeta_", false)]
        );
    }

    #[tokio::test]
    async fn put_channel_policy_validates_before_writing() {
        let pool = crate::testdb::fresh_pool().await;
        let good = Policy {
            families: vec![rule("job_", false)],
            kinds: vec![],
        };
        put_channel_policy(&pool, "fake", &good, Utc::now())
            .await
            .unwrap();

        let bad = Policy {
            families: vec![rule("", true)],
            kinds: vec![],
        };
        let refused = put_channel_policy(&pool, "fake", &bad, Utc::now()).await;
        assert!(matches!(refused, Err(PolicyError::Invalid(_))));
        // Nothing was written: the first policy is intact.
        assert_eq!(channel_overrides(&pool, "fake").await.unwrap(), good);
    }

    #[tokio::test]
    async fn put_channel_policy_empty_inherits() {
        let pool = crate::testdb::fresh_pool().await;
        notify_policy::replace(
            &pool,
            &Policy {
                families: vec![rule("job_", false)],
                kinds: vec![],
            },
        )
        .await
        .unwrap();
        put_channel_policy(
            &pool,
            "fake",
            &Policy {
                families: vec![rule("job_", true)],
                kinds: vec![],
            },
            Utc::now(),
        )
        .await
        .unwrap();
        assert!(passes(
            &resolved_policy(&pool, Some("fake")).await.unwrap(),
            "job_x"
        ));

        put_channel_policy(&pool, "fake", &Policy::default(), Utc::now())
            .await
            .unwrap();
        assert_eq!(
            channel_overrides(&pool, "fake").await.unwrap(),
            Policy::default()
        );
        assert!(!passes(
            &resolved_policy(&pool, Some("fake")).await.unwrap(),
            "job_x"
        ));
    }
}
