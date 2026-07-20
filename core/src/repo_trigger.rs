use std::collections::HashMap;

use crate::config::RepoTrigger;

/// Repo triggers whose watched-branch SHA has changed since it was last seen.
/// A trigger with no recorded last SHA is being seen for the first time (armed, not fired); a trigger
/// whose current SHA is unknown (git failed this poll) is skipped.
pub fn due_repo_triggers<'a>(
    rules: &'a [RepoTrigger],
    last_shas: &HashMap<String, String>,
    current_shas: &HashMap<String, String>,
) -> Vec<&'a RepoTrigger> {
    rules
        .iter()
        .filter(
            |rule| match (last_shas.get(&rule.name), current_shas.get(&rule.name)) {
                (Some(last), Some(current)) => last != current,
                _ => false,
            },
        )
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn trigger(name: &str) -> RepoTrigger {
        RepoTrigger {
            name: name.to_string(),
            branch: "main".to_string(),
            prompt: "go".to_string(),
        }
    }

    #[test]
    fn unchanged_sha_is_not_due() {
        let rules = vec![trigger("t1")];
        let last = HashMap::from([("t1".to_string(), "aaa".to_string())]);
        let current = HashMap::from([("t1".to_string(), "aaa".to_string())]);
        assert!(due_repo_triggers(&rules, &last, &current).is_empty());
    }

    #[test]
    fn changed_sha_is_due() {
        let rules = vec![trigger("t1")];
        let last = HashMap::from([("t1".to_string(), "aaa".to_string())]);
        let current = HashMap::from([("t1".to_string(), "bbb".to_string())]);
        let due = due_repo_triggers(&rules, &last, &current);
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].name, "t1");
    }

    #[test]
    fn first_sight_without_last_sha_is_not_due() {
        let rules = vec![trigger("t1")];
        let last = HashMap::new();
        let current = HashMap::from([("t1".to_string(), "aaa".to_string())]);
        assert!(due_repo_triggers(&rules, &last, &current).is_empty());
    }

    #[test]
    fn missing_current_sha_is_not_due() {
        let rules = vec![trigger("t1")];
        let last = HashMap::from([("t1".to_string(), "aaa".to_string())]);
        let current = HashMap::new();
        assert!(due_repo_triggers(&rules, &last, &current).is_empty());
    }
}
