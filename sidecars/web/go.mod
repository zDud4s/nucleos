module nucleosweb

go 1.26

// Pinned, not left to whatever the machine has. The `go` line above is a language floor and says
// nothing about which stdlib gets compiled in, and the stdlib is where this process's exposure
// lives: it fetches URLs nobody vetted and parses what comes back.
//
// govulncheck against go1.26.1, 2026-08-16: 21 vulnerabilities reachable from this module's own
// code, fixed across go1.26.2 through go1.26.6. Zero after.
toolchain go1.26.6

require github.com/mackee/go-readability v0.3.1

// v0.39.0 -> v0.55.0, and this one is not about the toolchain. Five of the vulnerabilities above
// were in golang.org/x/net/html, reached by extract.Extract -> readability.ParseHTML -> html.Parse:
// this sidecar's whole job is running that parser over HTML a stranger wrote, so a parser DoS is
// the one class of bug it is guaranteed to be handed.
require golang.org/x/net v0.55.0 // indirect
