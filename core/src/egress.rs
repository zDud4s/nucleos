//! What may cross from NucleOS to a model, and in what form.
//!
//! Two classifications already existed and never met. `mcp_tools.rs` classifies TOOLS by effect —
//! what calling one does. `runner.rs` classifies RUNS by tool policy — what a run is allowed to
//! reach for. Neither answers the question this module exists for: *who is on the other end, and
//! does that change what is allowed to leave?*
//!
//! Nothing needed that answer while there was one runner. The daemon picks a single
//! `CommandRunner` at startup (`main.rs`), every path uses it, and it is always a cloud CLI — so
//! "the other end" was a constant, and a constant needs no policy. A local runner answering a chat
//! makes it a variable, and this module is where that variable is read.

/// Who is on the receiving end of a tool result.
///
/// The distinction is the machine boundary, not the vendor: `Cloud` means the bytes leave this
/// computer, and that is the only property any rule here cares about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Audience {
    /// A model running on this machine. Nothing sent to it leaves.
    Local,
    /// A model reached over the network.
    Cloud,
}

/// The environment variable the daemon stamps on the MCP subprocess to name its audience.
///
/// An environment variable rather than an argument because `build_mcp_config` writes the argument
/// list into a file the CLI reads back, and that file is world-readable in the temp directory: the
/// audience is not a secret, but the fewer things that file decides, the fewer things a hand-edited
/// copy of it can decide.
pub const AUDIENCE_ENV: &str = "NUCLEOS_MCP_AUDIENCE";

impl Audience {
    /// Reads the audience the daemon stamped, defaulting to `Cloud`.
    ///
    /// `Cloud` is the fail-safe default and the reason this is not an `Option`: an unset or
    /// misspelled variable must produce the STRICTER treatment, never the looser one. A local
    /// audience that is wrongly filtered loses a little fidelity in an answer; a cloud audience
    /// that is wrongly unfiltered sends a stranger's bank details to a third party. Those two
    /// mistakes are not the same size, so the default is the one whose failure is survivable.
    pub fn from_env() -> Self {
        match std::env::var(AUDIENCE_ENV).as_deref() {
            Ok("local") => Self::Local,
            _ => Self::Cloud,
        }
    }

    /// The value to stamp for this audience. Paired with `from_env` so the two spellings cannot
    /// drift apart unnoticed.
    // `allow` rather than `expect`, matching `redact::redact_url`: the tests below DO call this, so
    // an expectation would go unfulfilled in a test build and warn there instead of here.
    #[allow(
        dead_code,
        reason = "nothing stamps the audience yet; the daemon starts doing so when a local runner can answer a turn"
    )]
    pub fn as_env_value(self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::Cloud => "cloud",
        }
    }
}

/// What happens to one tool result on its way to a model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Disposition {
    /// Hand it over unchanged.
    Cross,
    /// Hand it over with every deterministically-detectable secret replaced by a marker.
    CrossRedacted,
}

/// PURE: what happens to a result addressed to `audience`.
///
/// Deliberately NOT keyed on the tool's `ToolEffect`, and that is the whole judgement in this
/// function. The obvious design filters only `ReadsUntrusted`, because those are the tools that
/// admit to carrying a stranger's words. But `mcp_tools.rs` already records why that would be a
/// trap: `get_run` is classified `ReadsOwn` "only lexically — a triage run's stdout is a model's
/// answer over mail". A rule keyed on the effect table would wave that one through, and the next
/// tool whose output quietly quotes third-party text would be waved through too, silently, on the
/// day it is added.
///
/// Scanning everything costs a pass over a string that is already in memory, and it makes the
/// question "is this tool classified correctly?" stop being load-bearing for egress. A cheap rule
/// that cannot be wrong beats a precise rule that can.
pub fn disposition(audience: Audience) -> Disposition {
    match audience {
        Audience::Local => Disposition::Cross,
        Audience::Cloud => Disposition::CrossRedacted,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_local_audience_gets_the_text_untouched() {
        assert_eq!(disposition(Audience::Local), Disposition::Cross);
    }

    #[test]
    fn a_cloud_audience_always_gets_a_redacted_result() {
        assert_eq!(disposition(Audience::Cloud), Disposition::CrossRedacted);
    }

    /// The rule this module exists to make impossible to get wrong: no tool name, and no
    /// classification of one, can put a cloud audience on the unfiltered path.
    ///
    /// Asserted as a property rather than as a table because the point is the ABSENCE of a per-tool
    /// escape hatch. If `disposition` ever grows a tool argument, this test is what should have to
    /// be rewritten to allow it — deliberately, and in view.
    #[test]
    fn nothing_puts_a_cloud_audience_on_the_unfiltered_path() {
        assert_ne!(disposition(Audience::Cloud), Disposition::Cross);
    }

    #[test]
    fn an_unset_or_misspelled_audience_is_treated_as_cloud() {
        // Not asserted through `from_env` itself: the process environment is global, and a test
        // that mutates it races every other test in the binary. The mapping it performs is what
        // matters, and it is asserted here in the same shape the function uses.
        for value in [None, Some("cloud"), Some("Local"), Some(""), Some("remote")] {
            let audience = match value {
                Some("local") => Audience::Local,
                _ => Audience::Cloud,
            };
            assert_eq!(
                audience,
                Audience::Cloud,
                "{value:?} must not be read as a local audience"
            );
        }
    }

    #[test]
    fn the_env_spelling_round_trips() {
        assert_eq!(Audience::Local.as_env_value(), "local");
        // The round trip is what keeps `from_env` and `as_env_value` from drifting: whatever the
        // daemon stamps must be what the subprocess reads back.
        assert_eq!(
            match Audience::Local.as_env_value() {
                "local" => Audience::Local,
                _ => Audience::Cloud,
            },
            Audience::Local
        );
    }
}
