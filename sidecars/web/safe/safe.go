// Package safe decides which network destinations this sidecar is allowed to reach.
//
// It exists because this process is the only one in NucleOS that fetches a URL chosen by somebody
// else. An agent asks for a page; a page redirects somewhere; a hostname resolves to whatever its
// owner decided this second. Without a guard here, `web_read` is a request-forgery proxy pointed at
// the owner's own machine and home network: the daemon on 127.0.0.1:8791, the router, the printer,
// a cloud metadata endpoint on 169.254.169.254.
//
// The guard is deliberately placed at DIAL time rather than at parse time. Checking the hostname
// before the request is the obvious design and it is the one that loses: between the check and the
// connection the name can resolve to something else (DNS rebinding), and every redirect hop
// resolves again anyway. `Control` runs after resolution, with the exact address about to be
// connected, on every hop — so there is no window between deciding and dialling, and a redirect
// cannot smuggle a destination past a check that already happened.
package safe

import (
	"errors"
	"fmt"
	"net"
	"net/url"
	"syscall"
)

// ErrBlocked is the class every refusal here belongs to. Callers match on it rather than on message
// text, because the message names the address and the address came from somebody else.
var ErrBlocked = errors.New("blocked destination")

// CheckURL validates what can be judged before any name is resolved: the scheme and the presence of
// a host. It deliberately does NOT judge the host — that is the dialer's job, for the reason in the
// package comment.
func CheckURL(raw string) (*url.URL, error) {
	parsed, err := url.Parse(raw)
	if err != nil {
		return nil, fmt.Errorf("%w: not a URL: %v", ErrBlocked, err)
	}
	if parsed.Scheme != "http" && parsed.Scheme != "https" {
		// file:, ftp:, gopher: and data: are all reachable from a redirect if left unchecked, and
		// none of them is a web page. An empty scheme lands here too, which is correct: a relative
		// reference is not a destination.
		return nil, fmt.Errorf("%w: scheme %q is not http or https", ErrBlocked, parsed.Scheme)
	}
	if parsed.Hostname() == "" {
		return nil, fmt.Errorf("%w: no host in %q", ErrBlocked, raw)
	}
	return parsed, nil
}

// CheckIP refuses every address that is not a public internet destination.
//
// The list is written as "what is allowed to be refused" rather than "what is allowed", because the
// failure mode of forgetting an entry has to be a refusal that someone notices, never a private
// range that quietly stays reachable.
func CheckIP(ip net.IP) error {
	switch {
	case ip == nil:
		return fmt.Errorf("%w: unparseable address", ErrBlocked)
	case ip.IsLoopback():
		// 127.0.0.0/8 and ::1 — the daemon, the other sidecars, and Ollama all live here.
		return fmt.Errorf("%w: %s is loopback", ErrBlocked, ip)
	case ip.IsUnspecified():
		// 0.0.0.0 and :: reach loopback on several platforms.
		return fmt.Errorf("%w: %s is unspecified", ErrBlocked, ip)
	case ip.IsPrivate():
		// 10/8, 172.16/12, 192.168/16, and fc00::/7 — the home network the owner is sitting on.
		return fmt.Errorf("%w: %s is a private address", ErrBlocked, ip)
	case ip.IsLinkLocalUnicast(), ip.IsLinkLocalMulticast():
		// 169.254/16 carries cloud metadata services, which hand out credentials to anyone who asks.
		return fmt.Errorf("%w: %s is link-local", ErrBlocked, ip)
	case ip.IsMulticast(), ip.IsInterfaceLocalMulticast():
		return fmt.Errorf("%w: %s is multicast", ErrBlocked, ip)
	}
	// An IPv4 address carried inside an IPv6 one (::ffff:127.0.0.1, and the 6to4/Teredo shapes) is
	// the classic way past a check that only looked at the 16-byte form. Unmap and judge again.
	if mapped := ip.To4(); mapped != nil && len(ip) == net.IPv6len {
		return CheckIP(mapped)
	}
	return nil
}

// Control is the hook for net.Dialer.Control. It receives the address the connection is about to be
// made to, already resolved, which is the only moment at which the answer cannot go stale.
func Control(_, address string, _ syscall.RawConn) error {
	host, _, err := net.SplitHostPort(address)
	if err != nil {
		return fmt.Errorf("%w: %q is not host:port", ErrBlocked, address)
	}
	return CheckIP(net.ParseIP(host))
}
