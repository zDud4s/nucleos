//! What a run's prompt cost us, split into the only two parts that can honestly be told apart.
//!
//! # Why there are two numbers here and not five
//!
//! The question this module was written to answer is the one `/context` answers in the editor: of
//! everything the model read before it said a word, how much was the MCP tool schemas, how much was
//! the appended system prompt, how much was the CLAUDE.md. Two thirds of that is answerable and one
//! third is not, and the shape of this module is the shape of that fact.
//!
//! **The stream gives no breakdown.** `runner::extract_usage` reads four totals — `input_tokens`,
//! `cache_read_input_tokens`, `cache_creation_input_tokens`, `num_turns` — and
//! `context_fill_from_line` reads two of them again. The `init` event's `tools` field is a list of
//! NAMES. There is no per-component split anywhere in what the CLI reports and no way to derive one
//! from what it does report.
//!
//! **CLAUDE.md cannot be priced from here, ever, and that is not a gap to be filled later.** This
//! daemon never sends it. The only place this process reads a CLAUDE.md at all is
//! `hooks::project_guidance()`, capped at 4,000 characters, and what that feeds is the auto-judge's
//! own prompt — a different model call, on a different budget, that has nothing to do with the run
//! being measured. Whatever the CLI loads from CLAUDE.md, it loads on its own, from a path it chose,
//! at a size we never see. Sizing it from here would mean reading a file the CLI may not have read,
//! at a moment it may not have read it, and reporting the answer as if it were a measurement.
//!
//! So: **what we authored, itemised — and one residual for everything else.** Splitting the residual
//! is the move to refuse. If a later change adds a third line to this module saying "CLAUDE.md was N
//! of the residual", that line is wrong however carefully it is computed, because the daemon has no
//! access to the input it would need. The residual is named for what it is — the CLI's own — and it
//! keeps the tool schemas out of it precisely so that it can be honest about containing everything
//! else undivided.
//!
//! # The ruler
//!
//! Four characters to the token. There is no tokenizer in this repository and this module did not
//! add one. Every field and every label downstream says `estimate` and never `tokens`, for the
//! reason `sessions::Conversation::context_estimate` states: *"Rough on purpose, and named so. Four
//! characters to the token is wrong in both directions and wrong by tens of percent; what it has to
//! be right about is the order of magnitude."* That is a sufficient ruler for the decision this
//! feeds — whether the part of the prompt we control is worth attacking at all — and an insufficient
//! one for anything that looks like billing.

/// The four things this daemon writes into one CLI run's prompt, in characters.
///
/// In characters and not in tokens, all the way to the database column, because characters are what
/// was actually measured. The division by four lives in [`Self::total_estimate`] and in the SQL that
/// reads the column back, so the ruler stays in one place and a row written by an older build is not
/// stuck with an older ruler.
///
/// The four are exactly what `runner::cli_args` puts on the command line, and are read from the same
/// values it reads. A second source of truth for any of them would be a number that drifts the day
/// somebody changes a flag, silently, in the direction of looking better.
///
/// What is deliberately NOT here: `--allowedTools`, `--disallowedTools`, `--model`, and the rest of
/// the flag/value pairs. They are tens of characters between them — under a rounding error of one
/// tool schema — and counting them would suggest a precision this has nowhere near.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AuthoredPrompt {
    /// The MCP tool schemas that were actually SENT in this run's prompt, from
    /// `mcp_tools::NucleosTools::advertised_schema_chars`.
    ///
    /// **Two different facts arrive here as zero, and neither is an unknown.** A run launched
    /// without `--mcp-config` is offered no tools by this daemon at all, so nothing was announced
    /// and there is no schema block to pay for. A run whose schemas are DEFERRED was offered tools
    /// and announced them — but the CLI keeps `ToolSearch`, advertises the tools by name, and fetches
    /// a schema only when the model asks for one, so the block is not in the prompt either.
    /// `runner::authored_prompt` holds the rule and `runner::schemas_are_deferred` the measurements
    /// behind it; both zeros mean "this prompt did not carry the schemas", which is the only claim
    /// this field makes.
    ///
    /// What the deferred run carries instead — the CLI's own by-name listing of those tools — lands
    /// in the residual, with everything else the CLI wrote. **This module does not size it**, and
    /// that is the header's argument rather than a gap. The only figure anyone has for it is a
    /// difference between two whole-prompt totals (850 tokens for 48 deferred tools, measured), and
    /// spreading a two-point difference over a tool count is a fit, not a character measurement.
    /// This daemon never writes that listing and never sees it, so a number for it here would be
    /// exactly the kind of invented split the module header refuses — and it would shrink the
    /// residual by something nobody measured.
    pub schema_chars: usize,
    /// `--append-system-prompt`, appended to the CLI's own system prompt on EVERY turn of the run.
    /// Capped at `http::INSTRUCTIONS_CEILING` (8,000 characters) where it is stored.
    pub system_prompt_chars: usize,
    /// The `--agents` JSON, one argv element holding an object. Capped at
    /// `http::AGENTS_JSON_CEILING` (8,000 characters) at the door.
    pub agents_chars: usize,
    /// The prompt itself — the instruction this run was launched with. It travels as a positional
    /// argument or on stdin depending on `steerable`, and costs the same either way, which is why
    /// this field does not care which.
    pub prompt_chars: usize,
}

impl AuthoredPrompt {
    /// Everything above, added up. Saturating, because a total that wrapped would report an enormous
    /// prompt as a tiny one and no input here can plausibly reach `usize::MAX` anyway.
    pub fn total_chars(&self) -> usize {
        self.schema_chars
            .saturating_add(self.system_prompt_chars)
            .saturating_add(self.agents_chars)
            .saturating_add(self.prompt_chars)
    }

    /// The same total under the four-characters-to-the-token ruler.
    ///
    /// `estimate` and never `tokens`, in the name and in every label that renders it. See the module
    /// header: the number is right about its order of magnitude and about nothing finer.
    ///
    /// **No production caller today, and that is the design rather than an oversight.** The launch
    /// path stores `total_chars()` and the read path divides with [`estimate_from_chars`], so the
    /// ruler is applied once, at the far end, to rows written by every past build. This is the same
    /// arithmetic offered on the struct for a caller that has the pieces in hand and never went
    /// through the database — the tests below being the only one so far.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn total_estimate(&self) -> i64 {
        estimate_from_chars(self.total_chars() as i64)
    }
}

/// The four-characters-to-the-token ruler, in the one place it is allowed to live.
///
/// The database stores CHARACTERS (`runs.authored_prompt_chars`), so every reader has to divide, and
/// a division written out at each reader is a ruler that can be improved in one place and not the
/// others. Trivial arithmetic behind a named function on purpose: the name is what makes a reader
/// look up why it is four, and the module header is what answers.
pub fn estimate_from_chars(chars: i64) -> i64 {
    chars / 4
}

/// Everything in the prompt that this daemon did not write, as an estimate: the CLI's own.
///
/// `None` if either side is unknown, and the two ways of being unknown are both ordinary. A run that
/// reported no usage — the local model reports none at all — has no total to subtract from; a run
/// launched before `runs.authored_prompt_chars` existed recorded nothing to subtract. Neither is a
/// run whose CLI-side prompt was empty, and answering `0` for either would say exactly that. This is
/// the same discipline `token_efficiency::Measures` keeps for the four totals it reads: *"a run that
/// reported nothing must not read back as a run that measured zero"*.
///
/// **Clamped at zero, and the clamp is expected to fire.** Four characters to the token overshoots
/// wherever the text is dense — JSON schemas are largely punctuation and short keys, which tokenise
/// worse than prose — so on a small run the authored estimate can genuinely exceed the whole
/// reported prompt. That is slack in our own ruler, not a fault in the CLI's accounting, and a
/// negative residual on screen would say the opposite: it would read as the daemon catching the CLI
/// under-reporting. Zero says the honest thing instead — that as far as this ruler can tell, the
/// prompt was ours.
///
/// What this must never become is a split of the residual. See the module header: the residual holds
/// the CLI's system prompt, its built-in tool definitions, whatever it loaded from CLAUDE.md, and the
/// conversation itself, and this daemon can distinguish exactly none of those.
pub fn residual_estimate(total_prompt_tokens: Option<i64>, authored: Option<i64>) -> Option<i64> {
    let total = total_prompt_tokens?;
    let authored = authored?;
    Some(total.saturating_sub(authored).max(0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_four_authored_pieces_add_up_and_the_estimate_is_a_quarter_of_them() {
        let authored = AuthoredPrompt {
            schema_chars: 40_000,
            system_prompt_chars: 800,
            agents_chars: 1_200,
            prompt_chars: 2_000,
        };

        assert_eq!(authored.total_chars(), 44_000);
        assert_eq!(authored.total_estimate(), 11_000);
    }

    /// The whole point of the `Option` on both sides. A run answered by the local model reports no
    /// usage at all, so there is no total for the authored part to be subtracted from — and a
    /// residual of zero there would say the model read nothing but us.
    #[test]
    fn a_run_that_reported_no_tokens_has_no_residual_rather_than_a_large_one() {
        assert_eq!(residual_estimate(None, Some(11_000)), None);
        assert_eq!(residual_estimate(Some(50_000), None), None);
        assert_eq!(residual_estimate(None, None), None);
        assert_eq!(residual_estimate(Some(50_000), Some(11_000)), Some(39_000));
    }

    /// Chars/4 overshoots on schema JSON, so this is reachable on any small run that carries the
    /// full tool surface. Zero, never a negative — a negative would read as the CLI under-reporting
    /// rather than as slack in a ruler this module admits is rough.
    #[test]
    fn an_estimate_that_overshoots_the_report_is_no_residual_and_never_a_negative_one() {
        assert_eq!(residual_estimate(Some(9_000), Some(11_000)), Some(0));
        assert_eq!(residual_estimate(Some(0), Some(11_000)), Some(0));
    }

    /// The saturating adds are not decoration: `total_chars` feeds a display, and a wrap would show
    /// the largest prompt this daemon ever built as the smallest.
    #[test]
    fn a_total_that_could_not_be_added_saturates_instead_of_wrapping() {
        let absurd = AuthoredPrompt {
            schema_chars: usize::MAX,
            system_prompt_chars: usize::MAX,
            agents_chars: 0,
            prompt_chars: 0,
        };

        assert_eq!(absurd.total_chars(), usize::MAX);
    }
}
