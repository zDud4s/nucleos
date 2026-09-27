//! §spec pilar-de-browser
//!
//! Which profile a browsing session runs in, and whether it may run at all.
//!
//! Pure: no I/O, no database, no Chrome. The same split `trust.rs` has from the modules that call
//! it, and for the same reason — this is the security decision of the browser pillar, so it is a
//! table of cases rather than a branch buried in a handler.
//!
//! # Two decisions, not one
//!
//! 1. **May this reach the browser at all?** (spec §6.0b) The v1 serves the assistant. An autonomous
//!    pillar is refused structurally, and an assistant turn with nobody in front of it is refused
//!    situationally. Those are different refusals with different remedies, and collapsing them into
//!    one would send someone to fix the wrong thing.
//! 2. **Where does it happen?** (spec §5.3) The agent chooses WHAT to look at; this module chooses
//!    WHERE. A host the project has logged into runs in the project profile; anything else is handed
//!    to a throwaway. Landing somewhere off the list is not an error — it is a downgrade.
//!
//! # Why this is stricter than `trust.rs`, on purpose
//!
//! `trust::allowed` covers an entry AND its subdomains, which is right for deciding whether text may
//! be read as written. This module decides what runs inside a profile holding live login cookies,
//! and there a subdomain is a different principal: `evil.jira.example.com` is registrable by whoever
//! controls the zone, and one XSS on any subdomain would otherwise reach the session. So here the
//! match is on the **whole origin** — scheme, host and port, exactly (spec §5.3). No wildcards.

use url::Url;

pub use crate::trust::Requester;

/// Where a session runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Profile {
    /// The project's persistent profile — the one with the logins in it.
    Project,
    /// A throwaway, discarded when the session ends (spec §5.1).
    Ephemeral,
}

impl Profile {
    /// How this is stored and reported over the API.
    pub fn as_str(self) -> &'static str {
        match self {
            Profile::Project => "project",
            Profile::Ephemeral => "ephemeral",
        }
    }
}

/// Which surface asked. Distinct from [`Requester`], and both are needed — see [`decide`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Surface {
    /// An assistant turn: a person is having a conversation, whether or not they are looking right
    /// now.
    Assistant,
    /// A pillar, a scheduled run, a repo trigger. Not an assistant surface at all.
    ///
    /// Never constructed in production, and that IS the design (spec §6.0b): the v1 serves the
    /// assistant, and the autonomous path is left as a seam rather than a road. The variant exists
    /// so the refusal is a case in the table with a test on it, instead of an absence somebody later
    /// fills in with a default. The pillar that builds that path is the one that deletes this line.
    #[allow(dead_code)]
    Autonomous,
}

/// What happens to the request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// Open a session in this profile.
    Open(Profile),
    /// Do not open one.
    ///
    /// `recoverable` says whether the person can do something about it right now. Opening the shell
    /// fixes [`RULE_NO_ONE_PRESENT`]; nothing the person does today fixes [`RULE_REACH_UNDESIGNED`],
    /// because that path is not built. A refusal that does not say which of the two it is invites
    /// the wrong remedy.
    Refused { recoverable: bool },
}

/// The verdict and the rule that produced it.
///
/// The rule travels with the verdict for the reason it does in `trust.rs` and `priority.rs`:
/// "refused" alone cannot be audited, and the interesting question is always which condition failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Decision {
    pub outcome: Outcome,
    pub rule: &'static str,
}

/// Not an assistant surface. **Not recoverable** — the autonomous path is designed as a seam and
/// not built (spec §6.0b). The pillar that builds it is the one that removes this rule.
pub const RULE_REACH_UNDESIGNED: &str = "reach-undesigned";
/// An assistant turn, but nobody in the foreground to take the wheel. **Recoverable:** the person
/// opens the shell.
pub const RULE_NO_ONE_PRESENT: &str = "no-one-present";
/// The url could not be read, or was not `https`.
pub const RULE_UNPARSEABLE: &str = "unparseable-url";
/// Both the requested and the final origin belong to the project's list.
pub const RULE_PROJECT_SITE: &str = "project-site";
/// The requested origin is not on the list. Handed to a throwaway rather than refused.
pub const RULE_OFF_LIST: &str = "off-list-ephemeral";
/// The requested origin was on the list; the redirect landed somewhere that is not.
pub const RULE_REDIRECTED_OUT: &str = "redirected-out-ephemeral";

/// Decide whether a session may open, and in which profile.
///
/// `requested` is what the caller asked for; `final_url` is where it actually landed after every
/// redirect. Both are needed, and passing the same string twice is only correct when nothing
/// redirected.
///
/// `surface` and `requester` are two questions, not one. `requester` is derived from
/// `attention::owner_is_present` and therefore cannot tell a pillar apart from an assistant turn
/// whose owner has the shell closed — an assistant turn arriving over Telegram at midnight looks
/// exactly like a cron job. Asking the surface separately is what stops that person being told
/// "autonomous reach is not designed" when the reach is designed and what is missing is them.
pub fn decide(
    requested: &str,
    final_url: &str,
    surface: Surface,
    requester: Requester,
    project_sites: &[String],
) -> Decision {
    // Structure before situation, and both before the url. The order is the same argument
    // `trust.rs` makes at its line 91: report the FIRST thing that was wrong, so the reported
    // reason is the one worth acting on.
    if surface != Surface::Assistant {
        return Decision {
            outcome: Outcome::Refused { recoverable: false },
            rule: RULE_REACH_UNDESIGNED,
        };
    }
    if requester != Requester::Owner {
        return Decision {
            outcome: Outcome::Refused { recoverable: true },
            rule: RULE_NO_ONE_PRESENT,
        };
    }

    let (Some(requested_origin), Some(final_origin)) = (origin_of(requested), origin_of(final_url))
    else {
        return Decision {
            outcome: Outcome::Refused { recoverable: false },
            rule: RULE_UNPARSEABLE,
        };
    };

    // Off the list is a DOWNGRADE, not a refusal (spec §5.4): the url is handed to a throwaway,
    // where a stranger's page runs with no login to steal.
    if !listed(&requested_origin, project_sites) {
        return Decision {
            outcome: Outcome::Open(Profile::Ephemeral),
            rule: RULE_OFF_LIST,
        };
    }

    // The redirect trap, and the reason the final url is a parameter at all. A listed host with an
    // open redirect must not launder an arbitrary destination into the profile that holds the
    // logins. The reverse — an unlisted host redirecting INTO the list — is already covered by the
    // check above, on the requested origin.
    if !listed(&final_origin, project_sites) {
        return Decision {
            outcome: Outcome::Open(Profile::Ephemeral),
            rule: RULE_REDIRECTED_OUT,
        };
    }

    Decision {
        outcome: Outcome::Open(Profile::Project),
        rule: RULE_PROJECT_SITE,
    }
}

/// The origin of a url — `scheme://host[:port]` — normalised, or `None` if it is not one we admit.
///
/// `https` only (spec §5.3). Plain `http` inside a profile that holds live session cookies means
/// anyone on the path can read and rewrite the page the agent is about to act on, so it is refused
/// here rather than downgraded. The cost is real and worth naming: an internal tool served over
/// `http` cannot be a project site, and has to be reached by hand.
///
/// `Url::host_str` does the work rather than any amount of string handling, because the shapes that
/// break a hand-rolled extractor are the shapes an attacker reaches for: `https://jira.example@evil.com/`
/// has host `evil.com`. It also gives IDN hosts back in punycode, which is what makes the homograph
/// case fall out for free instead of needing to be spotted.
pub fn origin_of(raw: &str) -> Option<String> {
    let parsed = Url::parse(raw).ok()?;
    if parsed.scheme() != "https" {
        return None;
    }
    let host = parsed
        .host_str()?
        .trim_end_matches('.')
        .to_ascii_lowercase();
    if host.is_empty() {
        return None;
    }
    // The port is part of the identity (spec §5.3): a different port is a different service, even
    // on the same machine. `Url::port_or_known_default` fills in 443 so that `https://x` and
    // `https://x:443` are the same origin, which they are.
    let port = parsed.port_or_known_default()?;
    Some(format!("https://{host}:{port}"))
}

/// Whether an origin is on the project's list.
///
/// Exact match on the whole origin. No subdomain rule, no wildcards — see the module comment for
/// why this is deliberately stricter than `trust::allowed`.
fn listed(origin: &str, project_sites: &[String]) -> bool {
    project_sites.iter().any(|entry| {
        // Entries are normalised through the same parser as the url, so `jira.example.com`,
        // `https://jira.example.com` and `https://JIRA.example.com:443/browse` all mean the same
        // thing and a formatting difference in the YAML is not a security difference.
        match normalise_entry(entry) {
            Some(entry) => entry == origin,
            None => false,
        }
    })
}

/// Bring a list entry into the same shape [`origin_of`] produces.
///
/// A bare host is accepted for the owner's convenience, and gains `https://` — not `http://`,
/// because the permissive reading of an ambiguous entry is the one that must not win.
fn normalise_entry(entry: &str) -> Option<String> {
    let entry = entry.trim();
    if entry.is_empty() || entry == "." {
        // An empty line in `~/.nucleos/browser.yaml` must match nothing. Left alone it would parse into
        // something, and a formatting slip would quietly widen the list.
        return None;
    }
    if entry.contains("://") {
        return origin_of(entry);
    }
    origin_of(&format!("https://{entry}"))
}

/// The origins a human login traversed, in order, deduplicated — the set granted as one when the
/// person comes back (spec §5.3a).
///
/// Real logins are not one host. Signing in to a Microsoft-backed tool walks through
/// `login.microsoftonline.com` and `login.live.com` before landing, and granting only the
/// destination leaves every subsequent login broken in a way that looks like the allowlist is
/// simply wrong. Granting the chain is the honest alternative, and it is granted **at return** —
/// once, for the whole set — so a login abandoned halfway grants nothing.
pub fn granted_origins(chain: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for step in chain {
        if let Some(origin) = origin_of(step)
            && !out.contains(&origin)
        {
            out.push(origin);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sites() -> Vec<String> {
        vec![
            "https://jira.exemplo.com".to_string(),
            "https://docs.exemplo.com".to_string(),
        ]
    }

    fn assistant(requested: &str, final_url: &str) -> Decision {
        decide(
            requested,
            final_url,
            Surface::Assistant,
            Requester::Owner,
            &sites(),
        )
    }

    // ---- reach (spec §6.0b) ------------------------------------------------

    /// A pillar is refused for being a pillar, and the refusal says it is not recoverable. Telling
    /// the person "nobody is present" here would send them to open a window that changes nothing.
    #[test]
    fn an_autonomous_surface_is_refused_structurally() {
        let url = "https://jira.exemplo.com/browse/X-1";
        let decision = decide(url, url, Surface::Autonomous, Requester::Owner, &sites());
        assert_eq!(decision.rule, RULE_REACH_UNDESIGNED);
        assert_eq!(decision.outcome, Outcome::Refused { recoverable: false });
    }

    /// The case that made this two parameters instead of one: an assistant turn over Telegram with
    /// the shell closed. The reach is designed; what is missing is a person.
    #[test]
    fn an_assistant_turn_with_nobody_present_is_refused_recoverably() {
        let url = "https://jira.exemplo.com/browse/X-1";
        let decision = decide(
            url,
            url,
            Surface::Assistant,
            Requester::Autonomous,
            &sites(),
        );
        assert_eq!(decision.rule, RULE_NO_ONE_PRESENT);
        assert_eq!(decision.outcome, Outcome::Refused { recoverable: true });
    }

    /// Structure is reported before situation, so the reported reason is the one worth acting on.
    #[test]
    fn a_pillar_with_nobody_present_reports_the_structural_reason() {
        let url = "https://jira.exemplo.com/browse/X-1";
        let decision = decide(
            url,
            url,
            Surface::Autonomous,
            Requester::Autonomous,
            &sites(),
        );
        assert_eq!(decision.rule, RULE_REACH_UNDESIGNED);
    }

    // ---- profile choice (spec §5.3) ---------------------------------------

    #[test]
    fn a_listed_origin_runs_in_the_project_profile() {
        let decision = assistant(
            "https://jira.exemplo.com/browse/X-1",
            "https://jira.exemplo.com/browse/X-1",
        );
        assert_eq!(decision.outcome, Outcome::Open(Profile::Project));
        assert_eq!(decision.rule, RULE_PROJECT_SITE);
    }

    #[test]
    fn an_unlisted_origin_is_downgraded_to_a_throwaway_rather_than_refused() {
        let decision = assistant(
            "https://noticias.exemplo/artigo",
            "https://noticias.exemplo/artigo",
        );
        assert_eq!(decision.outcome, Outcome::Open(Profile::Ephemeral));
        assert_eq!(decision.rule, RULE_OFF_LIST);
    }

    /// The redirect trap: an open redirect on a listed host must not launder a destination into the
    /// profile that holds the logins.
    #[test]
    fn a_redirect_out_of_the_list_lands_in_a_throwaway() {
        let decision = assistant(
            "https://jira.exemplo.com/go?to=elsewhere",
            "https://evil.example/payload",
        );
        assert_eq!(decision.outcome, Outcome::Open(Profile::Ephemeral));
        assert_eq!(decision.rule, RULE_REDIRECTED_OUT);
    }

    /// The same trap upward, which a naive implementation gets wrong: deciding over the final url
    /// alone would put this in the project profile, and nobody chose the final url.
    #[test]
    fn a_redirect_into_the_list_does_not_gain_the_project_profile() {
        let decision = assistant(
            "https://evil.example/bounce",
            "https://jira.exemplo.com/browse/X-1",
        );
        assert_eq!(decision.outcome, Outcome::Open(Profile::Ephemeral));
        assert_eq!(decision.rule, RULE_OFF_LIST);
    }

    // ---- the host semantics that eat allowlists (spec §5.3) ---------------

    /// The difference from `trust.rs`, and the reason this module has its own matcher. A subdomain
    /// is a DIFFERENT principal here: one XSS on any subdomain would otherwise reach the session.
    #[test]
    fn a_subdomain_of_a_listed_host_is_not_listed() {
        for url in [
            "https://evil.jira.exemplo.com/x",
            "https://jira.exemplo.com.evil.com/x",
            "https://evil-jira.exemplo.com/x",
            "https://exemplo.com/x",
        ] {
            let decision = assistant(url, url);
            assert_eq!(
                decision.outcome,
                Outcome::Open(Profile::Ephemeral),
                "{url} reached the project profile"
            );
        }
    }

    /// The port is part of the identity: a different port is a different service.
    #[test]
    fn a_different_port_is_a_different_origin() {
        let url = "https://jira.exemplo.com:8443/browse/X-1";
        assert_eq!(
            assistant(url, url).outcome,
            Outcome::Open(Profile::Ephemeral)
        );
    }

    #[test]
    fn the_default_port_is_the_same_origin_as_no_port() {
        let url = "https://jira.exemplo.com:443/browse/X-1";
        assert_eq!(assistant(url, url).outcome, Outcome::Open(Profile::Project));
    }

    /// `http` is refused outright rather than downgraded: inside a profile holding live cookies,
    /// anyone on the path could rewrite the page the agent is about to act on.
    #[test]
    fn plain_http_is_not_admitted_at_all() {
        let url = "http://jira.exemplo.com/browse/X-1";
        let decision = assistant(url, url);
        assert_eq!(decision.rule, RULE_UNPARSEABLE);
        assert_eq!(decision.outcome, Outcome::Refused { recoverable: false });
    }

    /// `https://jira.exemplo.com@evil.com/` is the shape every hand-rolled extractor gets wrong at
    /// least once. The host is `evil.com`.
    #[test]
    fn userinfo_does_not_smuggle_a_listed_host() {
        for url in [
            "https://jira.exemplo.com@evil.com/x",
            "https://user:jira.exemplo.com@evil.com/x",
            "https://evil.com/?next=https://jira.exemplo.com/",
            "https://evil.com/jira.exemplo.com",
        ] {
            assert_eq!(
                assistant(url, url).outcome,
                Outcome::Open(Profile::Ephemeral),
                "{url} smuggled a listed host past the check"
            );
        }
    }

    /// An IDN homograph resolves to a different host, and punycode is what makes that visible.
    #[test]
    fn an_idn_homograph_is_a_different_origin() {
        // Cyrillic "е" in "exemplo".
        let url = "https://jira.ехemplo.com/browse/X-1";
        assert_eq!(
            assistant(url, url).outcome,
            Outcome::Open(Profile::Ephemeral),
            "a homograph reached the project profile"
        );
    }

    #[test]
    fn matching_ignores_case_and_a_trailing_root_dot() {
        for url in [
            "https://JIRA.EXEMPLO.COM/x",
            "https://jira.exemplo.com./x",
            "https://Jira.Exemplo.Com./x",
        ] {
            assert_eq!(
                assistant(url, url).outcome,
                Outcome::Open(Profile::Project),
                "{url} should be the same origin"
            );
        }
    }

    /// The owner may write a bare host, a scheme, a port or a path; they all mean the same site.
    #[test]
    fn list_entries_are_accepted_in_the_shapes_people_write_them() {
        let url = "https://jira.exemplo.com/browse/X-1";
        for entry in [
            "jira.exemplo.com",
            "https://jira.exemplo.com",
            "https://jira.exemplo.com/",
            "https://JIRA.exemplo.com:443/browse/anything",
            "  jira.exemplo.com  ",
        ] {
            let decision = decide(
                url,
                url,
                Surface::Assistant,
                Requester::Owner,
                &[entry.to_string()],
            );
            assert_eq!(
                decision.outcome,
                Outcome::Open(Profile::Project),
                "entry {entry:?} did not match"
            );
        }
    }

    /// A formatting slip in `~/.nucleos/browser.yaml` must not widen the list.
    #[test]
    fn empty_and_junk_entries_match_nothing() {
        let sloppy = vec![
            String::new(),
            "   ".to_string(),
            ".".to_string(),
            "http://jira.exemplo.com".to_string(), // not https: not a site
        ];
        let url = "https://jira.exemplo.com/browse/X-1";
        let decision = decide(url, url, Surface::Assistant, Requester::Owner, &sloppy);
        assert_eq!(decision.outcome, Outcome::Open(Profile::Ephemeral));
    }

    #[test]
    fn an_empty_list_sends_everything_to_a_throwaway() {
        let url = "https://jira.exemplo.com/x";
        let decision = decide(url, url, Surface::Assistant, Requester::Owner, &[]);
        assert_eq!(decision.outcome, Outcome::Open(Profile::Ephemeral));
    }

    #[test]
    fn a_url_that_does_not_parse_is_refused() {
        for url in ["not a url", "", "https://", "///x", "data:text/html,hi"] {
            let decision = assistant(url, url);
            assert_eq!(decision.rule, RULE_UNPARSEABLE, "{url}");
        }
    }

    // ---- the SSO chain (spec §5.3a) ---------------------------------------

    #[test]
    fn a_login_chain_is_granted_as_a_set_in_order_without_duplicates() {
        let chain = vec![
            "https://jira.exemplo.com/login".to_string(),
            "https://login.microsoftonline.com/oauth2/authorize".to_string(),
            "https://login.live.com/oauth20".to_string(),
            "https://login.microsoftonline.com/oauth2/token".to_string(),
            "https://jira.exemplo.com/browse/X-1".to_string(),
        ];
        assert_eq!(
            granted_origins(&chain),
            vec![
                "https://jira.exemplo.com:443".to_string(),
                "https://login.microsoftonline.com:443".to_string(),
                "https://login.live.com:443".to_string(),
            ]
        );
    }

    #[test]
    fn a_chain_step_that_is_not_https_is_not_granted() {
        let chain = vec![
            "http://insecure.exemplo/step".to_string(),
            "not a url".to_string(),
            "https://jira.exemplo.com/done".to_string(),
        ];
        assert_eq!(
            granted_origins(&chain),
            vec!["https://jira.exemplo.com:443".to_string()]
        );
    }

    #[test]
    fn an_empty_chain_grants_nothing() {
        assert!(granted_origins(&[]).is_empty());
    }

    #[test]
    fn the_stored_form_round_trips_both_profiles() {
        assert_eq!(Profile::Project.as_str(), "project");
        assert_eq!(Profile::Ephemeral.as_str(), "ephemeral");
    }
}
