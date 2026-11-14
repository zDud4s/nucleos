//! The message a checkpoint commit carries: item, verdict, run, tree and a recognisable trailer.

/// What a checkpoint commit needs to say about itself: which item, which run, which tree, and
/// whether a gate agreed with it.
#[derive(Debug, Clone)]
pub struct CheckpointContext {
    pub id: String,
    pub title: String,
    pub run_id: Option<i64>,
    pub tree_id: Option<String>,
    /// `true` when a gate agreed with the commit; `false` for an item no gate measured.
    pub gated: bool,
}

/// The trailer every checkpoint commit ends with. `is_checkpoint_message` reads it.
const CHECKPOINT_TRAILER_KEY: &str = "Nucleos-Checkpoint:";
/// Subjects checkpoints carried before the trailer existed; history still holds them.
const LEGACY_CHECKPOINT_SUBJECTS: [&str; 2] = [
    "nucleos: checkpoint a gate agreed with",
    "nucleos: checkpoint an item no gate measured",
];
const CHECKPOINT_TITLE_CHARS: usize = 60;

fn checkpoint_trailer(gated: bool) -> String {
    format!(
        "{CHECKPOINT_TRAILER_KEY} {}",
        if gated { "gated" } else { "ungated" }
    )
}

fn checkpoint_verdict(gated: bool) -> &'static str {
    if gated {
        "a gate agreed with this"
    } else {
        "no gate measured this"
    }
}

/// The message used when the caller has no item to name (`checkpoint`'s own).
pub(super) fn fallback_checkpoint_message(gated: bool) -> String {
    format!(
        "nucleos: checkpoint\n\nVerdict: {}\n\n{}",
        checkpoint_verdict(gated),
        checkpoint_trailer(gated)
    )
}

/// `nucleos(<id>): <title>` subject, then verdict, run and tree lines, then the trailer.
///
/// The title is flattened to one line and cut on a character boundary at 60 characters.
pub fn checkpoint_message(ctx: &CheckpointContext) -> String {
    let flat = ctx.title.split_whitespace().collect::<Vec<_>>().join(" ");
    let title = if flat.chars().count() > CHECKPOINT_TITLE_CHARS {
        let cut: String = flat.chars().take(CHECKPOINT_TITLE_CHARS - 1).collect();
        format!("{}…", cut.trim_end())
    } else {
        flat
    };
    let mut msg = format!("nucleos({}): {title}\n\n", ctx.id);
    msg.push_str(&format!("Verdict: {}\n", checkpoint_verdict(ctx.gated)));
    if let Some(run) = ctx.run_id {
        msg.push_str(&format!("Run: {run}\n"));
    }
    if let Some(tree) = &ctx.tree_id {
        msg.push_str(&format!("Tree: {tree}\n"));
    }
    msg.push('\n');
    msg.push_str(&checkpoint_trailer(ctx.gated));
    msg
}

/// Whether a commit message is a checkpoint: it ends in the trailer, or is one of the old subjects.
pub fn is_checkpoint_message(msg: &str) -> bool {
    let trimmed = msg.trim();
    if let Some(last) = trimmed.lines().last()
        && let Some(value) = last.trim().strip_prefix(CHECKPOINT_TRAILER_KEY)
        && matches!(value.trim(), "gated" | "ungated")
    {
        return true;
    }
    trimmed
        .lines()
        .next()
        .is_some_and(|subject| LEGACY_CHECKPOINT_SUBJECTS.contains(&subject.trim()))
}
