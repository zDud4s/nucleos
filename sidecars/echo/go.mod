module nucleos/sidecars/echo

go 1.26.1

// Pinned, not left to whatever the machine has. The `go` line above is a language floor and says
// nothing about which stdlib gets compiled in, and the stdlib is where a sidecar's exposure lives.
//
// govulncheck against go1.26.1, 2026-08-16: 11 vulnerabilities reachable from this module's own
// code, fixed across go1.26.2 through go1.26.6. Zero after. Echo is the smallest of the five and
// still had eleven, which is the argument for pinning here rather than only where it looks
// dangerous: the count follows net/http and crypto/tls, not how important the sidecar is.
toolchain go1.26.6
