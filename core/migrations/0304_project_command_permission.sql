-- Spec 2026-11-09 §6: the permission field of a declared command. `roles` is a JSON list of role
-- names, `max_depth` the deepest tree depth that may invoke it. Both NULL means the current policy
-- applies. An unreadable `roles` fails closed (see `role_policy::Permission::from_columns`).
ALTER TABLE project_commands ADD COLUMN roles TEXT;
ALTER TABLE project_commands ADD COLUMN max_depth INTEGER;
