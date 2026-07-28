package config

import "testing"

// The núcleo sets this address, so a bad value is a wiring mistake — but the mistake would put a
// service that reads a mailbox on the network.
func TestFetchAddrMustBeLoopback(t *testing.T) {
	for _, addr := range []string{"0.0.0.0:8793", "192.168.1.10:8793", "example.com:8793", "8793"} {
		if err := requireLoopback(addr); err == nil {
			t.Errorf("accepted %q", addr)
		}
	}
	for _, addr := range []string{"127.0.0.1:8793", "localhost:8793", "[::1]:8793"} {
		if err := requireLoopback(addr); err != nil {
			t.Errorf("rejected %q: %v", addr, err)
		}
	}
}
