package safe

import (
	"errors"
	"net"
	"testing"
)

// The addresses a request-forgery attempt actually aims at. Each line is a destination somebody
// could reach through `web_read` if this package stopped working, named so a failure says what was
// exposed rather than which case index broke.
func TestCheckIPRefusesEveryNonPublicDestination(t *testing.T) {
	blocked := map[string]string{
		"the daemon itself":       "127.0.0.1",
		"loopback, any port":      "127.63.1.9",
		"IPv6 loopback":           "::1",
		"unspecified v4":          "0.0.0.0",
		"unspecified v6":          "::",
		"home network, 192.168":   "192.168.1.1",
		"home network, 10/8":      "10.0.0.5",
		"home network, 172.16/12": "172.20.11.3",
		"IPv6 unique local":       "fd00::1",
		"cloud metadata service":  "169.254.169.254",
		"link-local v6":           "fe80::1",
		"multicast":               "224.0.0.1",
		"v4-in-v6 loopback":       "::ffff:127.0.0.1",
		"v4-in-v6 home network":   "::ffff:192.168.0.1",
		"v4-in-v6 cloud metadata": "::ffff:169.254.169.254",
	}

	for what, addr := range blocked {
		ip := net.ParseIP(addr)
		if ip == nil {
			t.Fatalf("%s: test fixture %q does not parse", what, addr)
		}
		if err := CheckIP(ip); err == nil {
			t.Errorf("%s (%s) was allowed — web_read is a proxy to it", what, addr)
		} else if !errors.Is(err, ErrBlocked) {
			t.Errorf("%s (%s): refused with %v, which callers cannot match on", what, addr, err)
		}
	}
}

// The other half of the invariant. Without this, "refuse everything" would pass the test above and
// the pillar would be unable to read a single page.
func TestCheckIPAllowsPublicDestinations(t *testing.T) {
	allowed := map[string]string{
		"a public v4":       "93.184.216.34",
		"another public v4": "1.1.1.1",
		"a public v6":       "2606:4700:4700::1111",
		"v4-in-v6, public":  "::ffff:93.184.216.34",
		"just below 10/8":   "9.255.255.255",
		"just above 10/8":   "11.0.0.0",
		"just below 172.16": "172.15.255.255",
		"just above 172.31": "172.32.0.0",
	}

	for what, addr := range allowed {
		if err := CheckIP(net.ParseIP(addr)); err != nil {
			t.Errorf("%s (%s) was refused: %v", what, addr, err)
		}
	}
}

func TestCheckIPRefusesNil(t *testing.T) {
	if err := CheckIP(nil); err == nil {
		t.Fatal("an unparseable address was allowed")
	}
}

func TestCheckURLRefusesNonWebSchemes(t *testing.T) {
	// Every one of these is reachable from a redirect if the scheme is not re-checked on each hop.
	for _, raw := range []string{
		"file:///c:/Users/PC Multimedia/.ssh/id_rsa",
		"ftp://example.com/x",
		"gopher://example.com/x",
		"data:text/html,<script>x</script>",
		"javascript:alert(1)",
		"example.com/no-scheme",
	} {
		if _, err := CheckURL(raw); err == nil {
			t.Errorf("%q was accepted as a web destination", raw)
		}
	}
}

func TestCheckURLRefusesMissingHost(t *testing.T) {
	if _, err := CheckURL("http:///just-a-path"); err == nil {
		t.Fatal("a URL with no host was accepted")
	}
}

func TestCheckURLAcceptsWebURLs(t *testing.T) {
	for _, raw := range []string{
		"http://example.com",
		"https://docs.rs/tokio/latest/tokio/",
		"https://example.com:8443/x?y=z#frag",
	} {
		parsed, err := CheckURL(raw)
		if err != nil {
			t.Errorf("%q was refused: %v", raw, err)
			continue
		}
		if parsed.Hostname() == "" {
			t.Errorf("%q parsed with no hostname", raw)
		}
	}
}

// CheckURL must not judge the host: that is the dialer's job, and a hostname check here would be
// the very time-of-check/time-of-use window the package comment exists to close. A name that
// RESOLVES to loopback has to be accepted at parse time and refused at dial time.
func TestCheckURLDoesNotPrejudgeTheHost(t *testing.T) {
	if _, err := CheckURL("http://localhost:8791/kill"); err != nil {
		t.Fatalf("CheckURL judged the host, which moves the decision to the wrong moment: %v", err)
	}
}

// Control is where the refusal actually has to happen, because it is the only point that sees the
// resolved address for every hop of every redirect.
func TestControlRefusesResolvedPrivateAddresses(t *testing.T) {
	for _, address := range []string{
		"127.0.0.1:8791",
		"[::1]:8791",
		"192.168.1.1:80",
		"169.254.169.254:80",
	} {
		if err := Control("tcp", address, nil); err == nil {
			t.Errorf("dialling %s was permitted", address)
		}
	}
	if err := Control("tcp", "93.184.216.34:443", nil); err != nil {
		t.Errorf("dialling a public address was refused: %v", err)
	}
}

func TestControlRefusesMalformedAddresses(t *testing.T) {
	if err := Control("tcp", "not-host-port", nil); err == nil {
		t.Fatal("a malformed dial address was permitted")
	}
}
