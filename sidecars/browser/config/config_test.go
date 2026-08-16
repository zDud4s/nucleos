package config

import (
	"strings"
	"testing"
)

func TestRequireLoopback(t *testing.T) {
	cases := []struct {
		addr string
		ok   bool
	}{
		{"127.0.0.1:8795", true},
		{"localhost:8795", true},
		{"[::1]:8795", true},
		{"0.0.0.0:8795", false},
		{"192.168.1.135:8795", false},
		{"example.com:8795", false},
		{"127.0.0.1", false}, // no port
	}
	for _, tc := range cases {
		t.Run(tc.addr, func(t *testing.T) {
			err := requireLoopback(tc.addr)
			if tc.ok && err != nil {
				t.Fatalf("expected %q to be allowed, got %v", tc.addr, err)
			}
			if !tc.ok && err == nil {
				t.Fatalf("expected %q to be refused", tc.addr)
			}
		})
	}
}

// TestLoadRequiresAToken. This sidecar drives logged-in profiles; starting it with no token would
// leave those reachable by any local process that guessed the port.
func TestLoadRequiresAToken(t *testing.T) {
	t.Setenv("NUCLEOS_DAEMON_TOKEN", "")
	if _, err := Load(); err == nil {
		t.Fatal("Load succeeded with no token")
	}
}

func TestLoadDefaults(t *testing.T) {
	t.Setenv("NUCLEOS_DAEMON_TOKEN", "tok")
	cfg, err := Load()
	if err != nil {
		t.Fatalf("load: %v", err)
	}
	if cfg.Addr != DefaultAddr {
		t.Errorf("addr: got %q, want %q", cfg.Addr, DefaultAddr)
	}
	if cfg.DaemonURL != "http://127.0.0.1:8791" {
		t.Errorf("daemon url: got %q", cfg.DaemonURL)
	}
	if cfg.MaxSessions < 1 {
		t.Errorf("max sessions must leave room to browse, got %d", cfg.MaxSessions)
	}
	if cfg.OpenTimeout <= 0 {
		t.Errorf("open timeout: got %v", cfg.OpenTimeout)
	}
}

func TestLoadRefusesANonLoopbackAddr(t *testing.T) {
	t.Setenv("NUCLEOS_DAEMON_TOKEN", "tok")
	t.Setenv("BROWSER_ADDR", "0.0.0.0:8795")
	_, err := Load()
	if err == nil {
		t.Fatal("Load accepted a public listener")
	}
	if !strings.Contains(err.Error(), "loopback") {
		t.Fatalf("the error should say why: %v", err)
	}
}

func TestLoadRejectsNonsenseNumbers(t *testing.T) {
	t.Setenv("NUCLEOS_DAEMON_TOKEN", "tok")
	t.Setenv("BROWSER_MAX_SESSIONS", "0")
	if _, err := Load(); err == nil {
		t.Fatal("zero sessions should be refused, not silently clamped")
	}
}
