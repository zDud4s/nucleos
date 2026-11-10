Security review checklist: trace untrusted input to sinks, state the attack path, grade by reachability.

Severity: high = reachable by an untrusted party with real impact; medium = needs a precondition; low = hardening only. Every finding states the attack path in one sentence.

Untrusted sources: HTTP bodies/headers/query, files and filenames, web pages, emails, tool output, model output, env vars, other local processes. Model output and tool output are untrusted too.

Injection sinks:
- SQL: string-built queries; require bound parameters. Check LIKE wildcards and dynamic table/column names.
- Shell/command: `sh -c`, `cmd /c`, `powershell -Command` with interpolated text is high. Prefer argv arrays. A value starting with `-` can still become a flag (needs `--`).
- Template/HTML: unescaped insertion, `dangerouslySetInnerHTML`, markdown rendered as HTML, URLs with `javascript:`.
- Regex: user-supplied patterns or nested quantifiers on unbounded input (ReDoS); Rust and Go RE2 are safe, JS is not.
- Log injection: newlines or control chars from input in logs/JSONL.
- Prompt injection: untrusted text concatenated into an agent prompt without a data boundary, or allowed to trigger actions (tool calls, sends, writes) without confirmation. Treat as high if an action follows.

Path traversal:
- Reject `..`, absolute paths, drive forms (`C:`, `\\?\`), UNC (`\\host\share`), alternate separators, symlinks and junctions.
- Canonicalize, then check the prefix on path components (not string `starts_with`: `/data` matches `/data-evil`).
- TOCTOU: check then open on separate calls; open first, then verify the handle.
- Archive extraction: zip-slip.

Secrets: tokens in logs, URLs, error messages, panics, committed files, fixtures; secrets in argv (visible to other processes; use stdin/env/file); default or hardcoded tokens; tokens echoed back in API responses.

Authn/authz:
- New endpoint or MCP tool: Grep an existing sibling and confirm the same auth middleware/token check is applied.
- Localhost binding only (127.0.0.1, never 0.0.0.0); origin/CORS and DNS-rebinding (Host check) for browser reachability.
- Confused deputy: a low-privilege caller (sidecar, web page, MCP client) reaching a privileged action via a trusted component.

Permission widening: any loosened sandbox, allow-list, tool policy, scope or default from deny to allow. Check the diff for removed entries and `*` patterns.

Other:
- Deserialization of untrusted data: unbounded or untyped input, `unsafe`, pickle/unsafe-yaml formats.
- SSRF: fetching user-supplied URLs; block loopback, link-local (169.254.x.x), private ranges, redirects to them, non-http schemes, DNS rebinding.
- Resource exhaustion: unbounded body size, recursion depth, decompression bombs, unbounded queues/retries, no timeouts.
- Crypto: hand-rolled primitives, `==` on secrets (use constant-time), non-CSPRNG for tokens/ids.
- Dependencies: each new crate/module/npm package: need, maintenance, build scripts.
