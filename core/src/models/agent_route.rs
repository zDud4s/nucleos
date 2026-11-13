//! The fixed model ladder for agent spawns: a pure classifier, the escalation step and the
//! source labels a spawn is recorded under. No IO; the hook endpoint and the spawn store build on it.

/// A rung of the ladder, ordered cheapest to dearest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Rung {
    Haiku,
    Sonnet,
    Opus,
}

impl Rung {
    pub fn as_alias(self) -> &'static str {
        match self {
            Rung::Haiku => "haiku",
            Rung::Sonnet => "sonnet",
            Rung::Opus => "opus",
        }
    }

    pub fn parse_alias(alias: &str) -> Option<Rung> {
        match alias {
            "haiku" => Some(Rung::Haiku),
            "sonnet" => Some(Rung::Sonnet),
            "opus" => Some(Rung::Opus),
            _ => None,
        }
    }

    fn up(self) -> Rung {
        match self {
            Rung::Haiku => Rung::Sonnet,
            Rung::Sonnet | Rung::Opus => Rung::Opus,
        }
    }
}

/// Where the model of a spawn came from; the value stored in `agent_spawns.source`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// The call named a `model` itself; never rewritten.
    Explicit,
    /// The router answered with a model the ladder knows.
    Router,
    /// The router had no usable answer and the ladder chose.
    Ladder,
    /// No answer inside the hook's time ceiling; the spawn went through untouched.
    Failopen,
    /// A spawn the hook deliberately leaves alone (`wf-*` cockpits).
    Skipped,
}

pub const SOURCE_EXPLICIT: &str = "explicit";
pub const SOURCE_ROUTER: &str = "router";
pub const SOURCE_LADDER: &str = "ladder";
pub const SOURCE_FAILOPEN: &str = "failopen";
pub const SOURCE_SKIPPED: &str = "skipped";

impl Source {
    pub const ALL: [Source; 5] = [
        Source::Explicit,
        Source::Router,
        Source::Ladder,
        Source::Failopen,
        Source::Skipped,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Source::Explicit => SOURCE_EXPLICIT,
            Source::Router => SOURCE_ROUTER,
            Source::Ladder => SOURCE_LADDER,
            Source::Failopen => SOURCE_FAILOPEN,
            Source::Skipped => SOURCE_SKIPPED,
        }
    }

    pub fn parse(text: &str) -> Option<Source> {
        Source::ALL.into_iter().find(|s| s.as_str() == text)
    }
}

/// The model family named by a full model id, or `None` for anything the Agent tool would not accept.
pub fn rung_of_model_id(id: &str) -> Option<Rung> {
    let id = id.to_ascii_lowercase();
    if !id.starts_with("claude") {
        return None;
    }
    if id.contains("opus") {
        Some(Rung::Opus)
    } else if id.contains("sonnet") {
        Some(Rung::Sonnet)
    } else if id.contains("haiku") {
        Some(Rung::Haiku)
    } else {
        None
    }
}

/// Subagent types that only read and search.
const HAIKU_TYPES: &[&str] = &["Explore", "researcher"];

/// Read-only location and search words (whole words, lowercase).
const SEARCH_WORDS: &[&str] = &[
    "find", "locate", "search", "list", "read", "grep", "lookup",
];

/// Multi-word search phrases.
const SEARCH_PHRASES: &[&str] = &["where is", "which files"];

/// Git and housekeeping routine (whole words, lowercase).
const OPS_WORDS: &[&str] = &[
    "commit", "rebase", "merge", "cherry-pick", "cleanup", "prune", "worktree",
];

/// Words that mean the task changes code; they keep it off the cheap rung.
const WORK_WORDS: &[&str] = &[
    "implement", "fix", "write", "add", "refactor", "build", "create", "change", "update", "design",
    "debug",
];

fn words(text: &str) -> Vec<String> {
    text.to_lowercase()
        .split(|c: char| !(c.is_alphanumeric() || c == '-'))
        .filter(|w| !w.is_empty())
        .map(str::to_owned)
        .collect()
}

/// The rung the ladder starts a spawn on. Never Opus: that comes from escalation or an explicit ask.
pub fn classify(subagent_type: &str, description: &str) -> Rung {
    let ws = words(description);
    let has = |list: &[&str]| ws.iter().any(|w| list.contains(&w.as_str()));
    if has(WORK_WORDS) {
        return Rung::Sonnet;
    }
    let lower = description.to_lowercase();
    if HAIKU_TYPES.contains(&subagent_type)
        || has(SEARCH_WORDS)
        || has(OPS_WORDS)
        || SEARCH_PHRASES.iter().any(|p| lower.contains(p))
    {
        Rung::Haiku
    } else {
        Rung::Sonnet
    }
}

/// One rung above the highest rung that already failed, capped at Opus, never below `base`.
pub fn escalate(base: Rung, failed: &[Rung]) -> Rung {
    match failed.iter().max() {
        Some(&top) => top.up().max(base),
        None => base,
    }
}

/// The first `max` characters of `text`, cut on a character boundary.
pub fn head_chars(text: &str, max: usize) -> &str {
    match text.char_indices().nth(max) {
        Some((idx, _)) => &text[..idx],
        None => text,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explore_and_search_descriptions_are_haiku() {
        assert_eq!(classify("Explore", "find where X is defined"), Rung::Haiku);
        assert_eq!(classify("worker", "locate the retry code"), Rung::Haiku);
        assert_eq!(classify("worker", "Where is the config read"), Rung::Haiku);
    }

    #[test]
    fn git_ops_descriptions_are_haiku() {
        assert_eq!(classify("worker", "rebase and merge the branch"), Rung::Haiku);
        assert_eq!(classify("worker", "Prune stale worktree entries"), Rung::Haiku);
    }

    #[test]
    fn implementation_work_is_sonnet() {
        assert_eq!(classify("coder", "implement the retry in resolve"), Rung::Sonnet);
        assert_eq!(classify("worker", "find the bug and fix it"), Rung::Sonnet);
    }

    #[test]
    fn a_keyword_inside_a_longer_word_does_not_match() {
        assert_eq!(classify("zzz", "already threaded"), Rung::Sonnet);
    }

    #[test]
    fn an_unknown_type_with_no_keyword_is_sonnet() {
        assert_eq!(classify("zzz", ""), Rung::Sonnet);
    }

    #[test]
    fn the_ladder_never_picks_opus_on_its_own() {
        for ty in ["Explore", "researcher", "worker", "coder", "wf-reviewer", "zzz", ""] {
            for d in ["", "find x", "rebase", "implement y", "review the design", "ünïcode ✓"] {
                assert_ne!(classify(ty, d), Rung::Opus, "{ty}/{d}");
            }
        }
    }

    #[test]
    fn escalate_steps_one_rung_and_caps_at_opus() {
        assert_eq!(escalate(Rung::Haiku, &[Rung::Haiku]), Rung::Sonnet);
        assert_eq!(escalate(Rung::Sonnet, &[Rung::Sonnet]), Rung::Opus);
        assert_eq!(escalate(Rung::Sonnet, &[Rung::Sonnet, Rung::Opus]), Rung::Opus);
        assert_eq!(escalate(Rung::Sonnet, &[]), Rung::Sonnet);
    }

    #[test]
    fn escalate_never_goes_below_the_base() {
        assert_eq!(escalate(Rung::Sonnet, &[Rung::Haiku]), Rung::Sonnet);
    }

    #[test]
    fn rung_of_model_id_maps_families_and_refuses_strangers() {
        assert_eq!(rung_of_model_id("claude-sonnet-5"), Some(Rung::Sonnet));
        assert_eq!(rung_of_model_id("claude-opus-5-5"), Some(Rung::Opus));
        assert_eq!(rung_of_model_id("claude-haiku-5-5"), Some(Rung::Haiku));
        assert_eq!(rung_of_model_id("gpt-5"), None);
        assert_eq!(rung_of_model_id("fable"), None);
        assert_eq!(rung_of_model_id(""), None);
    }

    #[test]
    fn aliases_round_trip() {
        for r in [Rung::Haiku, Rung::Sonnet, Rung::Opus] {
            assert_eq!(Rung::parse_alias(r.as_alias()), Some(r));
        }
        assert_eq!(Rung::parse_alias("Opus"), None);
    }

    #[test]
    fn head_chars_cuts_on_a_char_boundary() {
        assert_eq!(head_chars(&"é".repeat(10), 3).chars().count(), 3);
        assert_eq!(head_chars("abc", 10), "abc");
        assert_eq!(head_chars("abc", 0), "");
        assert_eq!(head_chars("", 4), "");
    }

    #[test]
    fn the_source_values_are_the_recorded_five() {
        let all: Vec<&str> = Source::ALL.iter().map(|s| s.as_str()).collect();
        assert_eq!(all, ["explicit", "router", "ladder", "failopen", "skipped"]);
        for s in Source::ALL {
            assert_eq!(Source::parse(s.as_str()), Some(s));
        }
        assert_eq!(Source::parse("other"), None);
    }
}
