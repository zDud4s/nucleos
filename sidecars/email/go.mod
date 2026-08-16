module nucleosemail

go 1.26

// Pinned, not left to whatever the machine has. The `go` line above is a language floor and says
// nothing about which stdlib gets compiled in, and the stdlib is where this process's exposure
// lives: it holds the owner's mailbox credentials and does IMAP over TLS against a server it does
// not control.
//
// govulncheck against go1.26.1, 2026-08-16: 17 vulnerabilities reachable from this module's own
// code — the highest of the five — fixed across go1.26.2 through go1.26.6. Zero after.
toolchain go1.26.6

require github.com/emersion/go-imap/v2 v2.0.0-beta.7

require github.com/emersion/go-message v0.18.2 // indirect

require github.com/emersion/go-sasl v0.0.0-20231106173351-e73c9f7bad43 // indirect
