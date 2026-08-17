module nucleostelegram

go 1.26

// Pinned, not left to whatever the machine has. The `go` line above is a language floor and says
// nothing about which stdlib gets compiled in, and the stdlib is where this process's exposure
// lives: it holds a bot token and talks to api.telegram.org over TLS.
//
// govulncheck against go1.26.1, 2026-08-16: 13 vulnerabilities reachable from this module's own
// code, fixed across go1.26.2 through go1.26.6. Zero after.
toolchain go1.26.6
