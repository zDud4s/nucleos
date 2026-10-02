// §spec pilar-de-browser

package fence

import (
	"context"
	"errors"
	"fmt"
	"net"
	"net/http"
	"time"
)

// Resolver is the part of [net.Resolver] the guarded dialer uses, named so a test can answer with
// the addresses an attacker's DNS would.
type Resolver interface {
	LookupIPAddr(ctx context.Context, host string) ([]net.IPAddr, error)
}

// DialRefused is the error a guarded dial returns when the address a name RESOLVED to is one the
// fence does not let a page reach. Its own type so a caller can answer it as a refusal rather than
// as a network that is down.
type DialRefused struct {
	Host    string
	Address net.IP
}

func (e *DialRefused) Error() string {
	return fmt.Sprintf("fence: %s resolves to %s, which a page may not reach", e.Host, e.Address)
}

// Dialer is the only way this process opens a connection on a page's behalf.
//
// # Why the name check is not enough
//
// [isLoopbackHost] reads the NAME a page asked for. A name is whatever its DNS says it is:
// `127.0.0.1.nip.io` is 127.0.0.1, and a rebinding domain answers a public address to the check and
// 127.0.0.1 to the connect a second later. Either one reaches the núcleo's API and the other
// sidecars from a page the policy said could not. So the decision is taken again HERE, on the
// addresses the name actually resolved to, and the connection goes to the address that was vetted
// — never back through the resolver, which is the gap a rebind lives in.
//
// # What it refuses
//
//   - Loopback and unspecified, unless the page asked for this machine BY NAME (`localhost`,
//     `127.0.0.1`, …). That case has already been through the policy's Loopback list, which is the
//     one place a person admits a local service; a public name that turns out to mean this machine
//     has not been, and cannot be admitted by it.
//   - Link-local (169.254/16, fe80::/10), always. Nothing a page legitimately addresses lives there,
//     and the cloud metadata endpoint does.
//
// Private ranges stay reachable, for the reason [isLoopbackHost] gives: an intranet is what a
// browser is for as often as it is an attack.
type Dialer struct {
	// Resolver answers the names. Nil means [net.DefaultResolver].
	Resolver Resolver
	// Timeout bounds one connect. Zero means ten seconds.
	Timeout time.Duration
}

// DialContext resolves, vets every answer, and connects to a vetted address.
func (g *Dialer) DialContext(ctx context.Context, network, address string) (net.Conn, error) {
	host, port, err := net.SplitHostPort(address)
	if err != nil {
		return nil, err
	}
	addresses, err := g.vet(ctx, host)
	if err != nil {
		return nil, err
	}
	timeout := g.Timeout
	if timeout == 0 {
		timeout = 10 * time.Second
	}
	dialer := &net.Dialer{Timeout: timeout}
	var last error
	for _, ip := range addresses {
		conn, dialErr := dialer.DialContext(ctx, network, net.JoinHostPort(ip.String(), port))
		if dialErr == nil {
			return conn, nil
		}
		last = dialErr
	}
	return nil, last
}

// vet resolves host and refuses the whole answer if ANY address in it is forbidden. Any, not the
// first: the resolver's order is the attacker's to choose, and so is which address a fallback tries.
func (g *Dialer) vet(ctx context.Context, host string) ([]net.IP, error) {
	askedForLoopback := isLoopbackHost(host)
	var addresses []net.IP
	if literal := net.ParseIP(trimBrackets(host)); literal != nil {
		addresses = []net.IP{literal}
	} else {
		resolver := g.Resolver
		if resolver == nil {
			resolver = net.DefaultResolver
		}
		answers, err := resolver.LookupIPAddr(ctx, host)
		if err != nil {
			return nil, err
		}
		for _, answer := range answers {
			addresses = append(addresses, answer.IP)
		}
	}
	if len(addresses) == 0 {
		return nil, fmt.Errorf("fence: %s resolved to nothing", host)
	}
	for _, ip := range addresses {
		if forbiddenAddress(ip, askedForLoopback) {
			return nil, &DialRefused{Host: host, Address: ip}
		}
	}
	return addresses, nil
}

func forbiddenAddress(ip net.IP, askedForLoopback bool) bool {
	if ip.IsLinkLocalUnicast() || ip.IsLinkLocalMulticast() {
		return true
	}
	if ip.IsLoopback() || ip.IsUnspecified() {
		return !askedForLoopback
	}
	return false
}

func trimBrackets(host string) string {
	if len(host) >= 2 && host[0] == '[' && host[len(host)-1] == ']' {
		return host[1 : len(host)-1]
	}
	return host
}

// IsDialRefused reports whether err, however wrapped, is a guarded dial refusing an address.
func IsDialRefused(err error) bool {
	var refused *DialRefused
	return errors.As(err, &refused)
}

// NewTransport is an [http.Transport] that dials only through d and never through a proxy the
// environment names: a transport that honoured HTTP_PROXY would send fenced traffic to whatever
// that says, which is both a way around the dial check and a way out of the fence nobody wrote down.
func NewTransport(d *Dialer) *http.Transport {
	return &http.Transport{
		Proxy:               nil,
		DialContext:         d.DialContext,
		TLSHandshakeTimeout: 10 * time.Second,
		ForceAttemptHTTP2:   true,
	}
}
