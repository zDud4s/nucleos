-- The spend ceiling of one relay chain. Decision of 2026-10-11 (relay with a full shell, behind
-- three brakes): a turn handed over by another conversation may get the tools, and what bounds a
-- chain of such turns is a ceiling of its own, separate from the house budget beside it.
--
-- On `autopilot_global`, beside the budget it sits next to, because it is one house-wide setting
-- and not a property of any conversation. NOT NULL with a default, unlike `budget_limit_usd`: a
-- relay chain is unattended spend by construction, so there is no "off" — a brake that ships
-- disabled is not a brake. Five dollars is a first guess, changed through `POST /autopilot/budget`.
ALTER TABLE autopilot_global ADD COLUMN relay_chain_limit_usd REAL NOT NULL DEFAULT 5.0;
