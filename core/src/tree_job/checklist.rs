//! The node checklist close rule: a node closes only with all items `[x]` or one `[!]`.

/// The items of a node checklist, each list in document order, text trimmed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Checklist {
    pub open: Vec<String>,
    pub done: Vec<String>,
    pub blocked: Vec<String>,
}

/// Reads the items out of `text`, in the format of `spec_loop.parse` (spec_loop.py:65): one per
/// line, as `<indent>(-|*)<space>[ |x|X|!]<space><text>`. Any other line is ignored, including a
/// `status:` line, which belongs to the spec_loop hook and has no say over a node.
pub fn parse(text: &str) -> Checklist {
    let mut checklist = Checklist::default();
    for (mark, item) in text.lines().filter_map(parse_item) {
        let list = match mark {
            ' ' => &mut checklist.open,
            'x' | 'X' => &mut checklist.done,
            _ => &mut checklist.blocked,
        };
        list.push(item.to_string());
    }
    checklist
}

/// The mark and trimmed text of `line`, or `None` when the line is not an item.
fn parse_item(line: &str) -> Option<(char, &str)> {
    let after_bullet = line.trim_start().strip_prefix(['-', '*'])?;
    let marked = after_bullet.trim_start();
    if marked.len() == after_bullet.len() {
        return None;
    }
    let body = marked.strip_prefix('[')?;
    let mark = body
        .chars()
        .next()
        .filter(|c| matches!(c, ' ' | 'x' | 'X' | '!'))?;
    let after_mark = body[mark.len_utf8()..].strip_prefix(']')?;
    let item = after_mark.trim();
    if item.is_empty() || !after_mark.starts_with(char::is_whitespace) {
        return None;
    }
    Some((mark, item))
}

/// Decides whether a node with this checklist may close. A `[!]` closes it whatever else is
/// pending. Otherwise every item must be `[x]`, and there must be at least one: a node that never
/// listed its work cannot close. The `Err` is the message returned to the agent.
pub fn may_close(text: &str) -> Result<(), String> {
    let checklist = parse(text);
    if !checklist.blocked.is_empty() {
        return Ok(());
    }
    if !checklist.open.is_empty() {
        return Err(format!(
            "The node cannot close, these checklist items are still pending: {}. Finish each one \
             and mark it `[x]`, or mark what stops you `[!]`.",
            checklist.open.join("; ")
        ));
    }
    if checklist.done.is_empty() {
        return Err(
            "The node cannot close, its checklist has no items. List the node's work as `- [ ]` \
             items first."
                .to_string(),
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_sorts_both_bullets_and_every_mark_in_document_order() {
        let c = parse(
            "- [ ] first open\n* [ ] second open\n- [x] lower done\n* [X] upper done\n- [!] dash blocked\n* [!] star blocked\n",
        );

        assert_eq!(c.open, ["first open", "second open"]);
        assert_eq!(c.done, ["lower done", "upper done"]);
        assert_eq!(c.blocked, ["dash blocked", "star blocked"]);
    }

    #[test]
    fn parse_accepts_items_indented_with_spaces_or_tabs() {
        let c = parse("    - [ ] spaces\n\t* [x] tab\n \t - [!] mixed\n");

        assert_eq!(c.open, ["spaces"]);
        assert_eq!(c.done, ["tab"]);
        assert_eq!(c.blocked, ["mixed"]);
    }

    #[test]
    fn parse_trims_item_text_but_keeps_its_inner_spacing() {
        let c = parse("-    [x]    keeps   inner   \n- [ ] crlf line\r\n- [!] last, no newline  ");

        assert_eq!(c.done, ["keeps   inner"]);
        assert_eq!(c.open, ["crlf line"]);
        assert_eq!(c.blocked, ["last, no newline"]);
    }

    #[test]
    fn parse_ignores_every_line_that_is_not_an_item() {
        let c = parse(
            "# - [x] heading\n\
             status: active\n\
             prose before - [x] mid line\n\
             - plain bullet\n\
             [x] no bullet\n\
             + [x] plus bullet\n\
             1. [x] numbered\n\
             -[x] no space after bullet\n\
             - [x]no space after mark\n\
             - [] empty mark\n\
             - [y] unknown mark\n\
             - [xx] long mark\n",
        );

        assert!(c.open.is_empty(), "open: {:?}", c.open);
        assert!(c.done.is_empty(), "done: {:?}", c.done);
        assert!(c.blocked.is_empty(), "blocked: {:?}", c.blocked);
    }

    #[test]
    fn may_close_accepts_a_checklist_with_every_item_done() {
        assert_eq!(may_close("intro\n- [x] one\n* [X] two\n"), Ok(()));
    }

    #[test]
    fn may_close_accepts_a_blocked_item_even_with_others_pending() {
        assert_eq!(may_close("- [!] needs the owner\n"), Ok(()));
        assert_eq!(may_close("- [x] one\n- [!] needs the owner\n"), Ok(()));
        assert_eq!(
            may_close("- [ ] still open\n- [x] one\n- [!] needs the owner\n"),
            Ok(())
        );
    }

    #[test]
    fn may_close_error_names_every_pending_item_and_no_finished_one() {
        let err = may_close("- [x] finished work\n- [ ] write the parser\n* [ ] wire the route\n")
            .expect_err("a pending item with no [!] must refuse the close");

        assert!(err.contains("write the parser"), "error: {err}");
        assert!(err.contains("wire the route"), "error: {err}");
        assert!(!err.contains("finished work"), "error: {err}");
    }

    #[test]
    fn may_close_refuses_pending_item_whatever_the_status_line_says() {
        // R10 decides on the items alone; `status:` belongs to the spec_loop hook, not to a node.
        let err = may_close("status: done\n- [ ] not done at all\n")
            .expect_err("a status line must not override a pending item");

        assert!(err.contains("not done at all"), "error: {err}");
    }

    #[test]
    fn may_close_refuses_a_checklist_with_no_items() {
        // Pinned choice: no items cannot close. spec_loop.stop_decision treats `no_items` like
        // open work and pushes back ("has no items. List the spec's deliverables"), and vacuous
        // truth would let a node close without ever having listed its work.
        for text in ["", "\n\n", "status: done\nJust prose, nothing to tick.\n"] {
            let err = may_close(text).expect_err("no items must refuse the close");

            assert!(err.to_lowercase().contains("no items"), "error: {err}");
        }
    }
}
