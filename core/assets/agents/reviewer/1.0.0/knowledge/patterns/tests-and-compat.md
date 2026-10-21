Test coverage and compatibility review by reading only: map each changed behavior to a test that would fail if it broke, then find every consumer of changed contracts.

Tests (you cannot run them):
- For each changed behavior, Grep test names, fixtures and call sites to find the covering test. Name the test in the finding, or say none was found.
- A test counts only if it fails when the behavior is broken: asserts the outcome (values, state, error kind), not just "does not panic" or "returns Ok"; covers the error and edge path, not only the happy path.
- Red flags in the diff: assertions removed or loosened; tests deleted; `#[ignore]`, `skip`, `.only`, `t.Skip` added; mocks that return exactly what the assertion expects (tautological); a new branch with no case; test and code changed together so the test only mirrors the code (check the expected value is independently justified, e.g. by spec); sleeps/timing asserts that are flaky.
- Missing coverage is a medium finding naming the missing case ("no test for empty list / failure of X / old-format file").
- If a claim needs a run, do not state the result. Put the exact command in the finding text, e.g. `cargo test -p <crate> <name>` for the controller to run, and mark it unverified.

Compatibility: for any change to a public API, HTTP route, CLI flag, file format, schema, event shape, or MCP tool schema, Grep all consumers: other crates, Go sidecars, the shell (TS), scripts, docs, fixtures, packaged assets.
- Renamed or removed fields; changed defaults; changed units.
- Stricter validation that rejects previously valid data.
- Changed error codes or status codes that callers match on.
- Enum variants added where other code matches exhaustively or switches on strings (including TS unions and Go switches without default).
- Serialized persisted data: old rows, files, configs and events must still load (serde defaults, optional new fields, migration present and idempotent, down/forward behavior on rollback).
- Version bumps and installed-copy hash rules: a packaged or versioned file (agent assets, manifests) changed in place without a version bump will not reach installed copies or will trip hash checks.
- Wire compatibility between components of different versions (daemon vs sidecar vs shell updated at different times).

Upgrade path: judge existing installs and existing data, not only fresh ones. Ask: what happens on first start after upgrade with old state on disk? Is a migration needed, and is it tested with an old-format fixture?

Report only what changes a merge decision; list the consumers you checked so the author can verify.
