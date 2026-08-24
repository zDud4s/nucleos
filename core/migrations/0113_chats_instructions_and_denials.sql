-- What a conversation is told before it starts, and what it may not reach for.

-- ── system_prompt ───────────────────────────────────────────────────────────────────────────────
-- `--append-system-prompt`. Standing instructions for one conversation: appended to the CLI's own
-- system prompt rather than replacing it, which is the whole difference between this flag and
-- `--system-prompt`. The replacing one is deliberately NOT stored — it would drop the CLI's tool
-- descriptions and safety framing on the floor, and a conversation that lost them would look like
-- a conversation whose model had got worse.
--
-- Per conversation and not per project, for the reason the helpers of 0112 are: the project's own
-- answer is `CLAUDE.md`, this daemon has conversations with no project at all, and a file in
-- somebody's tree is a file in somebody's history.
--
-- Sent on EVERY turn, not just the first. The flag is per invocation and this daemon spawns one per
-- turn, so anything else would make the instructions apply to the opening message and quietly stop
-- mattering — the failure mode that is hardest to notice, because the first answer is right.
--
-- NULL is no added instructions, which is what every conversation has had since the table was made.
ALTER TABLE chats ADD COLUMN system_prompt TEXT;

-- ── denied_tools ────────────────────────────────────────────────────────────────────────────────
-- `--disallowedTools`, as a JSON array of built-in tool names.
--
-- A DENY list and not an allow list, and that is the load-bearing choice. `--allowedTools` does not
-- restrict anything — it GRANTS permission on top of what is already allowed (see `BUILTIN_TOOLS`
-- in `runner.rs`, which measured exactly this) — so an allow list here would be a control that
-- reads as a restriction and is not one. Everything in this column can only ever take something
-- away, which is the only direction a window should be able to move a tool surface in.
--
-- Names only, never the CLI's `Bash(git *)` patterns. A pattern is a rule language, and a rule
-- language behind a row of checkboxes is a place to write something that matches nothing — which
-- the CLI reports as one line on stderr that nobody reading this app will ever see.
--
-- Merged with what `ToolPolicy` already denies into ONE flag, because `--disallowedTools` is
-- variadic and a second occurrence REPLACES the first rather than adding to it. Two flags here
-- would mean a per-conversation preference silently undoing a safety property.
--
-- NULL is no denials of its own. It is not "allow everything": what a conversation may reach is
-- still decided by `tool_policy_for` and, in a wired project, by the classifier hook.
ALTER TABLE chats ADD COLUMN denied_tools TEXT;
