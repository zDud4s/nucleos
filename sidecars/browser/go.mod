module nucleosbrowser

go 1.26

// Pinned, not left to whatever the machine has. `go 1.26` above is a language floor and says nothing
// about which stdlib gets compiled in, and the stdlib is where this process's exposure lives:
// `fence.Proxy.forward` and `launch.HTTPFetcher.Fetch` put net/http, crypto/tls and crypto/x509 on
// the path of every request a hostile page causes.
//
// govulncheck against go1.26.1, 2026-08-16: 14 vulnerabilities reachable from this module's own
// code, fixed across go1.26.2 (crypto/x509 name-constraint auth bypass, TLS 1.3 KeyUpdate DoS),
// go1.26.3 (HTTP/2 infinite loop on a bad SETTINGS_MAX_FRAME_SIZE) and up to go1.26.6. Zero after.
toolchain go1.26.6
