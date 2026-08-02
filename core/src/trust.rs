//! Whether a page's text may reach a privileged agent as written, or must be summarised first.
//!
//! Pure: no I/O, no database, no knowledge of run state. The same split `classifier.rs` and
//! `priority.rs` have from the modules that call them, and for the same reason — this is the
//! security decision of the web pillar, so it is a table of cases rather than a branch buried in a
//! handler.
//!
//! The rule (spec §5.2) is a CONJUNCTION, and both halves matter:
//!
//! 1. The owner asked, in the foreground. Not a cron run at three in the morning. The difference
//!    between "somebody is looking at this" and "this happened by itself" is the difference between
//!    a mistake that gets caught and one that gets discovered.
//! 2. Both the requested host and the final host are on the allowlist.
//!
//! Everything else is quarantined, and the DEFAULT is quarantine: there is no denylist, so a host
//! nobody has heard of is never trusted by omission.

use url::Url;

/// What may be done with a page's text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trust {
    /// The extracted Markdown goes to the agent as written, inside a neutralised fence.
    Raw,
    /// The local model reads it and the agent sees only a structured summary.
    Quarantined,
}

impl Trust {
    /// How this is stored in `web_pages.trust_at_fetch` and reported over the API.
    pub fn as_str(self) -> &'static str {
        match self {
            Trust::Raw => "raw",
            Trust::Quarantined => "quarantined",
        }
    }
}

/// Who asked for the page.
///
/// Deliberately two values and not a boolean. `is_owner: bool` reads fine at the definition and
/// terribly at the call site, where `decide(url, url, true, list)` is a coin flip for the reader —
/// and this is the argument that decides whether a stranger's text reaches a tool-holding agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Requester {
    /// A person, in the foreground, who typed or clicked this. `attention.rs` is what answers this
    /// question in production; this module only consumes the answer.
    Owner,
    /// A scheduled run, a repo trigger, a pillar enriching a row — anything with nobody watching.
    Autonomous,
}

/// The verdict and the rule that produced it.
///
/// The rule travels with the verdict for the same reason it does in `priority.rs`: "quarantined"
/// alone cannot be audited, and the interesting question when something goes wrong is always which
/// condition failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Decision {
    pub trust: Trust,
    pub rule: &'static str,
}

/// Rule names. Constants because the shell renders them and a test asserts them.
pub const RULE_OWNER_ALLOWLISTED: &str = "owner-allowlisted";
pub const RULE_AUTONOMOUS: &str = "autonomous";
pub const RULE_NOT_ALLOWLISTED: &str = "not-allowlisted";
pub const RULE_REDIRECTED_OUT: &str = "redirected-out-of-allowlist";
pub const RULE_UNPARSEABLE: &str = "unparseable-url";

/// Decide what may be done with one page's text.
///
/// `requested` is what the caller asked for; `final_url` is where the content actually came from
/// after every redirect. Both are needed, and passing the same string twice is only correct when
/// nothing redirected.
pub fn decide(
    requested: &str,
    final_url: &str,
    requester: Requester,
    allowlist: &[String],
) -> Decision {
    // Quarantine first, unconditionally, for the case that fails to parse. A URL this module cannot
    // read is not a URL it can vouch for.
    let (Some(requested_host), Some(final_host)) = (host_of(requested), host_of(final_url)) else {
        return Decision {
            trust: Trust::Quarantined,
            rule: RULE_UNPARSEABLE,
        };
    };

    // The requester is checked before the allowlist so the rule name says the *first* thing that
    // was wrong. A cron run reading MDN is refused for being a cron run, which is the honest
    // reason; reporting "not-allowlisted" would send someone to edit a list that is already right.
    if requester != Requester::Owner {
        return Decision {
            trust: Trust::Quarantined,
            rule: RULE_AUTONOMOUS,
        };
    }

    if !allowed(&requested_host, allowlist) {
        return Decision {
            trust: Trust::Quarantined,
            rule: RULE_NOT_ALLOWLISTED,
        };
    }

    // The redirect trap (spec §10.1), and the reason the final URL is a parameter at all.
    //
    // Trust NEVER travels up. An allowlisted host that redirects out of the allowlist loses it —
    // otherwise an open redirect on a trusted domain launders any destination into `Raw`. The
    // reverse matters just as much and is easier to miss: an unknown host that redirects INTO the
    // allowlist must not gain trust either, because the owner chose the first URL and not the
    // second. The check above, on `requested_host`, is what covers that direction.
    if !allowed(&final_host, allowlist) {
        return Decision {
            trust: Trust::Quarantined,
            rule: RULE_REDIRECTED_OUT,
        };
    }

    Decision {
        trust: Trust::Raw,
        rule: RULE_OWNER_ALLOWLISTED,
    }
}

/// The host of a URL, lowercased and without a trailing root dot.
///
/// `Url::host_str` is used rather than any amount of string handling, because the shapes that break
/// a hand-rolled extractor are exactly the shapes an attacker reaches for: `https://docs.rs@evil.com/`
/// has host `evil.com`, `https://evil.com/?x=docs.rs` has host `evil.com`, and
/// `https://docs.rs.evil.com/` has host `docs.rs.evil.com`.
fn host_of(raw: &str) -> Option<String> {
    let parsed = Url::parse(raw).ok()?;
    if parsed.scheme() != "http" && parsed.scheme() != "https" {
        // A `file://` or `data:` URL has no business being trusted, and some of them do parse a
        // host. The sidecar refuses these too; this module refuses them independently, because a
        // security decision that relies on another process having already checked is one check.
        return None;
    }
    let host = parsed
        .host_str()?
        .trim_end_matches('.')
        .to_ascii_lowercase();
    if host.is_empty() { None } else { Some(host) }
}

/// Whether a host is covered by an allowlist entry.
///
/// An entry covers itself and its subdomains, and nothing else. The dot in the suffix check is
/// load-bearing: without it `docs.rs` would also cover `evil-docs.rs`, which anybody can register.
fn allowed(host: &str, allowlist: &[String]) -> bool {
    allowlist.iter().any(|entry| {
        let entry = entry.trim().trim_end_matches('.').to_ascii_lowercase();
        if entry.is_empty() {
            // An empty line in the YAML would otherwise match every host, turning a formatting slip
            // into a system that trusts the entire internet.
            return false;
        }
        host == entry || host.ends_with(&format!(".{entry}"))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn allowlist() -> Vec<String> {
        vec!["docs.rs".to_string(), "developer.mozilla.org".to_string()]
    }

    #[test]
    fn the_owner_reading_an_allowlisted_host_gets_the_text_as_written() {
        let decision = decide(
            "https://docs.rs/tokio/latest/tokio/",
            "https://docs.rs/tokio/latest/tokio/",
            Requester::Owner,
            &allowlist(),
        );
        assert_eq!(decision.trust, Trust::Raw);
        assert_eq!(decision.rule, RULE_OWNER_ALLOWLISTED);
    }

    #[test]
    fn a_subdomain_of_an_allowlisted_host_is_covered() {
        let url = "https://blog.developer.mozilla.org/post";
        assert_eq!(
            decide(url, url, Requester::Owner, &allowlist()).trust,
            Trust::Raw
        );
    }

    /// The dot in the suffix check. `evil-docs.rs` and `docs.rs.evil.com` are both registrable by
    /// anybody, and a prefix or substring test hands them the allowlist.
    #[test]
    fn a_host_that_merely_resembles_an_allowlisted_one_is_not_covered() {
        for url in [
            "https://evil-docs.rs/x",
            "https://docs.rs.evil.com/x",
            "https://notdocs.rs/x",
            "https://xdocs.rs/x",
        ] {
            let decision = decide(url, url, Requester::Owner, &allowlist());
            assert_eq!(
                decision.trust,
                Trust::Quarantined,
                "{url} was trusted by resemblance"
            );
        }
    }

    /// The conjunction of spec §5.2. An allowlisted host is not enough on its own.
    #[test]
    fn an_autonomous_run_never_gets_raw_text_even_from_an_allowlisted_host() {
        let url = "https://docs.rs/tokio/latest/tokio/";
        let decision = decide(url, url, Requester::Autonomous, &allowlist());
        assert_eq!(decision.trust, Trust::Quarantined);
        assert_eq!(
            decision.rule, RULE_AUTONOMOUS,
            "the reported reason should be the run, not the list"
        );
    }

    /// The redirect trap, downward: an open redirect on a trusted domain must not launder a
    /// destination into `Raw`.
    #[test]
    fn trust_is_lost_when_an_allowlisted_host_redirects_out_of_the_allowlist() {
        let decision = decide(
            "https://docs.rs/go?to=elsewhere",
            "https://evil.example/payload",
            Requester::Owner,
            &allowlist(),
        );
        assert_eq!(decision.trust, Trust::Quarantined);
        assert_eq!(decision.rule, RULE_REDIRECTED_OUT);
    }

    /// The same trap upward, which is the direction a naive implementation gets wrong: deciding
    /// over the final URL alone would trust this, and the owner never chose the final URL.
    #[test]
    fn trust_is_not_gained_by_redirecting_into_the_allowlist() {
        let decision = decide(
            "https://evil.example/bounce",
            "https://docs.rs/anything",
            Requester::Owner,
            &allowlist(),
        );
        assert_eq!(
            decision.trust,
            Trust::Quarantined,
            "a redirect into the allowlist upgraded trust"
        );
        assert_eq!(decision.rule, RULE_NOT_ALLOWLISTED);
    }

    /// `https://docs.rs@evil.com/` is the shape every hand-rolled host extractor gets wrong at
    /// least once. The host is `evil.com`.
    #[test]
    fn userinfo_does_not_smuggle_an_allowlisted_host() {
        for url in [
            "https://docs.rs@evil.com/x",
            "https://user:docs.rs@evil.com/x",
            "https://evil.com/?next=https://docs.rs/",
            "https://evil.com/docs.rs",
            "https://evil.com/#docs.rs",
        ] {
            assert_eq!(
                decide(url, url, Requester::Owner, &allowlist()).trust,
                Trust::Quarantined,
                "{url} smuggled an allowlisted host past the check"
            );
        }
    }

    #[test]
    fn host_matching_ignores_case_and_a_trailing_root_dot() {
        for url in [
            "https://DOCS.RS/x",
            "https://docs.rs./x",
            "https://Docs.Rs./x",
        ] {
            assert_eq!(
                decide(url, url, Requester::Owner, &allowlist()).trust,
                Trust::Raw,
                "{url} should be the same host as docs.rs"
            );
        }
    }

    /// A formatting slip in `.ai/web.yaml` must not become a system that trusts everything.
    #[test]
    fn an_empty_allowlist_entry_matches_nothing() {
        let sloppy = vec![String::new(), "  ".to_string(), ".".to_string()];
        let url = "https://anything.example/x";
        assert_eq!(
            decide(url, url, Requester::Owner, &sloppy).trust,
            Trust::Quarantined
        );
    }

    #[test]
    fn an_empty_allowlist_quarantines_everything() {
        let url = "https://docs.rs/x";
        assert_eq!(
            decide(url, url, Requester::Owner, &[]).trust,
            Trust::Quarantined
        );
    }

    /// The default is quarantine, and it is reached by omission rather than by a denylist.
    #[test]
    fn an_unknown_host_is_quarantined_without_anyone_listing_it() {
        let url = "https://a-host-nobody-has-heard-of.example/x";
        let decision = decide(url, url, Requester::Owner, &allowlist());
        assert_eq!(decision.trust, Trust::Quarantined);
        assert_eq!(decision.rule, RULE_NOT_ALLOWLISTED);
    }

    #[test]
    fn a_url_that_does_not_parse_is_quarantined_rather_than_rejected() {
        for url in ["not a url", "", "http://", "///x"] {
            let decision = decide(url, url, Requester::Owner, &allowlist());
            assert_eq!(decision.trust, Trust::Quarantined, "{url}");
            assert_eq!(decision.rule, RULE_UNPARSEABLE, "{url}");
        }
    }

    /// A scheme that is not the web gets no trust here either, independently of the sidecar having
    /// already refused it. A security decision that relies on another process having checked is one
    /// check, not two.
    #[test]
    fn a_non_web_scheme_is_never_trusted() {
        for url in [
            "file:///c:/windows/win.ini",
            "data:text/html,hello",
            "ftp://docs.rs/x",
        ] {
            assert_eq!(
                decide(url, url, Requester::Owner, &allowlist()).trust,
                Trust::Quarantined,
                "{url}"
            );
        }
    }

    #[test]
    fn the_stored_form_round_trips_the_two_verdicts() {
        assert_eq!(Trust::Raw.as_str(), "raw");
        assert_eq!(Trust::Quarantined.as_str(), "quarantined");
    }
}
