-- Optional permissions on what a project declares that an agent can invoke (spec §6): the roles
-- allowed and a depth ceiling. NULL in both means no restriction, which is today's policy.
--
-- `allowed_roles` is a JSON array of lowercase role names; `max_depth` is the deepest tree level
-- that may use it.
ALTER TABLE project_commands ADD COLUMN allowed_roles TEXT;
ALTER TABLE project_commands ADD COLUMN max_depth INTEGER;
ALTER TABLE project_git_ops ADD COLUMN allowed_roles TEXT;
ALTER TABLE project_git_ops ADD COLUMN max_depth INTEGER;
