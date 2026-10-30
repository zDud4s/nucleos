//! The node checklist close rule: a node closes only with all items `[x]` or one `[!]`.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_sorts_both_bullets_and_every_mark_in_document_order() {
        let c = parse("- [ ] first open\n* [ ] second open\n- [x] lower done\n* [X] upper done\n- [!] dash blocked\n* [!] star blocked\n");

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
