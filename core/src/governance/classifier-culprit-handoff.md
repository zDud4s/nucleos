# Node Handoff

Node ID: 4cebad54-01cd-4aeb-8f04-a8f970c9114b
Root ID: fcd4f4d7-5087-4843-999b-bf1b93d9fe6a
Status: done
Role: leaf
Outcome: done
Contract used: none

## Summary

Decisions affecting siblings: `Classification` now has `culprit: Option<Culprit>` and `segments: Vec<String>`; any `Classification { .. }` literal must set both (`culprit: None, segments: Vec::new()` for allows). `Segment::Unrecognized` is now `Segment::Unrecognized(String)` (the reason). What is allowed or parked is unchanged; `reason` strings are unchanged. Every park sets a culprit (segment + concrete reason), allows set None; `segments` is filled in `classify()` for shell tools and is empty for other tools. Nothing was compiled: the controller must run the commands below.

## What was done

- Gate fix: the test asserts that compared `x.decision` to a string now read `x.decision.decision` (`Decision` has no Debug/PartialEq); a Grep finds none left. The report lives here because the path hook denies `.nucleos-report.md`.

- classifier.rs: `Culprit` struct, two new fields, `with_culprit`, `parked_tool`, `target_of`, `approval_segment`, `is_directory_change`; concrete reasons on all park paths (unrecognized, cd out of workspace, push/merge/deploy, parse failure, redirect, own settings, governance file, substitution outside quotes, empty line, tool parks).
- paths.rs `classification()` and shadow.rs test helper: new fields added.
- hooks/rules.rs `downgrade_if_unreadable` and hooks/pretool.rs leaf self-check allow literal: new fields added.
- 9 new tests at the end of the classifier tests module.
- `cargo fmt -p nucleos-core` and `check_invariants.py` via heavy.py: no output, no error.

## Decisions

- Widened node paths to core/src/hooks/rules.rs and core/src/hooks/pretool.rs (struct literals there).
- Tool parks use the tool name as segment; reasons name the target path.
- Parse failure: segment is the whole command, reason "line can't be parsed: {parser reason}".
- `downgrade_if_unreadable` (rules.rs) uses an empty segment and the given reason as culprit.
- `.nucleos-report.md` could not be written: the path hook denied it and widening was refused (overlaps sibling `shell-approval-card`). This handoff lives at core/src/governance/classifier-culprit-handoff.md instead.

## Files touched

core/src/governance/classifier.rs, core/src/governance/classifier/paths.rs, core/src/governance/shadow.rs, core/src/hooks/rules.rs, core/src/hooks/pretool.rs, core/src/governance/classifier-culprit-handoff.md (this file)

## Pending deletions

- core/src/governance/classifier-culprit-handoff.md (delete after reading; it is only the handoff).

## Commands for the root

- `cargo check -p nucleos-core --tests`
- `cargo test -p nucleos-core --lib governance::classifier`
- `cargo test -p nucleos-app hooks` (rules.rs and pretool.rs are in the moved hooks module; adjust the package if they build in nucleos-core)
- New tests: `an_unrecognized_command_is_the_culprit_with_its_own_text`, `the_culprit_is_the_one_segment_of_a_chain_that_parks`, `a_directory_change_out_of_the_workspace_names_the_cd`, `push_merge_deploy_names_the_piece_that_pushes`, `a_line_that_cannot_be_parsed_is_its_own_culprit`, `an_output_redirect_names_the_segment_that_redirects`, `a_substitution_outside_quotes_names_its_segment`, `a_tool_park_names_the_tool_and_leaves_segments_empty`, `an_allow_has_no_culprit`

## Open items / next step

- Unverified by compiler: other `Classification { .. }` literals outside those found may need the two fields; expected text in some tests (`cd C:/x`, redirect, `sleep 5 & git status` parse failure) may differ from what the code produces and need adjusting.
- The classifier-allows branch can follow once this is green.
