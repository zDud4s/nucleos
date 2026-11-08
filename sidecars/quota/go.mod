module nucleosquota

go 1.26

// Pinned for the same reason the web sidecar pins it: the `go` line is a language floor and says
// nothing about which stdlib gets compiled in. This process talks to a remote endpoint over TLS and
// parses what comes back, so crypto/tls and encoding/json are its exposure.
toolchain go1.26.6

// No third-party dependency, and that is worth stating rather than leaving as an absence. It reads
// two local files and calls one well-known endpoint, all of which the standard library does. The
// web sidecar needs a readability parser; this one would gain nothing but supply chain. The one
// require below is the in-repo channel kit (its netguard loopback check), not a third party.

require nucleoschannelkit v0.0.0

replace nucleoschannelkit => ../lib/channelkit
