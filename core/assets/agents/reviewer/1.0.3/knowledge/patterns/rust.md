Rust review patterns: what to look for in .rs changes (workspace crates: core = nucleos-core, nucleos-base).

Panics and truncation (non-test paths)
- `.unwrap()`, `.expect()`, `v[i]`, `&s[a..b]`, `x / y` on input-derived data. Tell: no prior length/None check.
- `as` casts between int widths or signed/unsigned (`len() as u32`, `i64 as usize`, `f64 as u8`) silently truncate or saturate. Prefer `try_from`.
- Byte vs char: `&s[..n]`, `s.len()` used as char count, `truncate(n)` on String panic off a UTF-8 boundary. Tell: user text, names, paths, log excerpts.
- Arithmetic: `a - b` on usize/u64 underflows (panic in debug, wraps in release). Tell: lengths, counters, timestamps subtracted. Want `checked_`/`saturating_`.

Error handling
- `let _ = fallible()`, `.ok()`, `unwrap_or_default()`, `unwrap_or(0)` hiding a failure that the caller should see or log.
- `?` or `map_err(|_| ..)` dropping the source error or path/id context.
- Early return leaving partial state: file half-written, row inserted without its pair, flag set but never cleared. Check Drop/cleanup guards.

Async and concurrency
- std `Mutex`/`RwLock` guard alive across `.await` (deadlock, non-Send). Tell: `let g = m.lock().unwrap();` followed by `.await` in the same scope.
- Blocking in async: `std::fs`, `std::thread::sleep`, `Command::output()`, heavy CPU, sync rusqlite. Want `spawn_blocking` or tokio equivalents.
- Two locks taken in different orders across functions.
- `tokio::spawn` with the JoinHandle dropped: panics and errors vanish, task outlives its owner. Tell: no abort/join/shutdown path.
- Cancellation: an `.await` between two dependent state changes; if the future is dropped, state is inconsistent (select!, timeout).
- Unbounded channels, Vecs, or maps that grow per event with no cap or eviction.

unsafe and API surface
- `unsafe` with no `// SAFETY:` stating the invariant.
- New enum variant or changed `pub` signature: Grep other crates (core, nucleos-base) for `match` on that enum and for callers; wildcard arms hide the miss.

serde and persistence
- New field on a persisted/wire struct without `#[serde(default)]` breaks reading old data. `deny_unknown_fields` breaks reading newer data.
- `rename`, `rename_all`, or variant renames change the wire/disk format; Grep for stored fixtures and the other side.
- SQL: `format!` into queries (injection, quoting); multi-statement changes outside one transaction; error paths that skip rollback; transaction held across `.await`.

Tests
- `#[test]` with no assertion, new `#[ignore]`, `sleep`-based timing, shared global/env/static state, fixed ports, shared temp paths.

Windows
- Hard-coded `/` or `\\` in paths (use `Path::join`); `\\?\` verbatim prefixes breaking `starts_with`/string comparisons; case-insensitive paths compared as strings.
- Replacing or deleting a running exe or open file fails (os error 5/32); delete/rename without retry.
- Cannot run tools: name the check, e.g. `cargo test -p nucleos-core <name>`.
