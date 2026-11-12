// Package netguard holds the one loopback check every sidecar applies to the addresses the núcleo
// hands it: a listener address that must stay on this machine, or a daemon URL a token travels to.
//
// The núcleo sets these values, so a bad one is a wiring mistake rather than an attack — but each
// such mistake would put a process that holds the owner's credentials or sessions on the network.
// Only "localhost" and loopback IP literals pass; nothing is resolved through DNS, so a name that
// happens to resolve to 127.0.0.1 today is still refused.
//
// This is the inbound/daemon-address check only. The outbound policies of the web and browser
// sidecars (sidecars/web/safe, sidecars/browser/fence) are deliberately different and live there.
package netguard

import (
	"fmt"
	"net"
	"net/url"
	"strings"
)

// IsLoopbackHost reports whether host is "localhost" (compared case-insensitively, as DNS names
// are) or a loopback IP literal. host carries no port and no IPv6 brackets.
func IsLoopbackHost(host string) bool {
	if strings.EqualFold(host, "localhost") {
		return true
	}
	ip := net.ParseIP(host)
	return ip != nil && ip.IsLoopback()
}

// RequireLoopbackAddr refuses a host:port listener address that is not on this machine. name is
// the environment variable the address came from, and every error names it.
func RequireLoopbackAddr(name, addr string) error {
	host, _, err := net.SplitHostPort(addr)
	if err != nil {
		return fmt.Errorf("%s is not host:port: %w", name, err)
	}
	if !IsLoopbackHost(host) {
		return fmt.Errorf("%s must be a loopback address, got %q", name, addr)
	}
	return nil
}

// RequireLoopbackURL refuses a URL that is not http(s) or whose host is not loopback. The errors
// do not name a variable; callers prefix the one the URL came from.
func RequireLoopbackURL(raw string) error {
	u, err := url.Parse(raw)
	if err != nil {
		return fmt.Errorf("not a valid URL: %w", err)
	}
	if u.Scheme != "http" && u.Scheme != "https" {
		return fmt.Errorf("unsupported scheme %q", u.Scheme)
	}
	host := u.Hostname()
	if host == "" {
		return fmt.Errorf("URL %q has no host", raw)
	}
	if !IsLoopbackHost(host) {
		return fmt.Errorf("must be a loopback address, got host %q", host)
	}
	return nil
}
