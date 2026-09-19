module nucleosquota

go 1.26

// Pinned for the same reason the web sidecar pins it: the `go` line is a language floor and says
// nothing about which stdlib gets compiled in. This process talks to a remote endpoint over TLS and
// parses what comes back, so crypto/tls and encoding/json are its exposure.
toolchain go1.26.6

// No `require` block, and that is worth stating rather than leaving as an absence: this sidecar has
// no third-party dependency at all. It reads two local files and calls one well-known endpoint, all
// of which the standard library does. The web sidecar needs a readability parser; this one would
// gain nothing but supply chain.
