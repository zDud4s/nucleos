How to read a repository and match its conventions before editing.

# Order of reading

1. The task text: goal, stated scope, files named, acceptance criteria.
2. Project instructions and decisions already in your context. They override your habits.
3. `CLAUDE.md`, `AGENTS.md`, `CONTRIBUTING*` at the repo root. Then look for nested ones (`CLAUDE.md`, `AGENTS.md`, `README`, `.editorconfig`) in every directory containing a file you will touch, walking up to the root. The nearest one wins on conflict. Obey them over your own preferences, including language of comments and commit-style rules.
4. The target files: whole if small, otherwise the relevant range plus the imports at the top and the surrounding function or type.
5. Neighbours: callers of what you change, tests covering it, sibling files in the same directory.
6. Existing similar code. Find the closest analogue (grep for a comparable function, route, component, command) and copy its pattern: structure, error path, logging, test shape.

Stop reading when you can state the change in two sentences. Do not read whole trees.

# Verify, never assume

You cannot compile or run anything. Every name you write must be confirmed by grep or read:
- functions, methods, types, fields, enum variants, and their exact signatures (argument order, async or sync, return type, error type)
- imports and module paths, including visibility (pub, export)
- file paths, config keys, CLI flags, environment variables, i18n keys
- dependencies: check the manifest (`Cargo.toml`, `package.json`, `go.mod`, `pyproject.toml`) only to learn what is already available. Do not add a dependency; use what exists. Lockfiles are read-only evidence.

Never invent an API. If grep finds nothing, the thing does not exist: use what does, or report the gap.

# Match the local style

Before writing, note and then reproduce:
- naming (case style, prefixes, test names)
- formatting: indentation, quote style, line width, trailing commas, brace placement
- error handling: how errors are created, wrapped, logged, surfaced
- comment language and density, doc-comment format
- import ordering and grouping
- test style: framework, helpers, fixtures, file placement
- user-facing strings: follow the file beside yours (hard-coded vs i18n key, tone, capitalisation). If an i18n catalogue exists, add keys the way existing ones are added.

If a formatter config exists, write code it would leave unchanged.

# Preserve bytes you do not mean to change

- Line endings: detect CRLF vs LF in the file and keep it. A new line in a CRLF file ends in CRLF. Mixed endings: match the surrounding lines.
- Keep a BOM if present; do not add one.
- Keep the trailing-newline state of the file.
- Keep encoding (UTF-8 stays UTF-8); do not re-escape or normalise non-ASCII characters.
- Keep indentation characters (tabs vs spaces).
A whole-file diff caused by line endings is a failed edit.

# Read before edit, re-read after

- Read the exact region right before editing it; files may have changed since you first looked.
- Edit with enough unique context to hit one place only.
- After editing, re-read the changed region (and the lines around it) to confirm the result: balanced braces, correct indentation, no duplicated or lost lines, endings preserved.
- If you made several edits to one file, re-read between them when they are close together.
