package config

import (
	"strings"
	"testing"
	"time"
)

func withRequired(t *testing.T) {
	t.Helper()
	t.Setenv("NUCLEOS_DAEMON_TOKEN", "token")
}

func TestTokenIsRequired(t *testing.T) {
	t.Setenv("NUCLEOS_DAEMON_TOKEN", "")
	if _, err := Load(); err == nil {
		t.Fatal("a sidecar started with no daemon token")
	}
}

func TestDefaults(t *testing.T) {
	withRequired(t)
	cfg, err := Load()
	if err != nil {
		t.Fatalf("loading with only the token failed: %v", err)
	}
	if cfg.Addr != DefaultAddr {
		t.Errorf("addr = %q, want %q", cfg.Addr, DefaultAddr)
	}
	if cfg.Provider != "brave" {
		t.Errorf("provider = %q, want brave", cfg.Provider)
	}
	if cfg.FetchTimeout != defaultFetchTimeout {
		t.Errorf("timeout = %v, want %v", cfg.FetchTimeout, defaultFetchTimeout)
	}
	if cfg.MaxPageBytes != defaultMaxPageBytes {
		t.Errorf("max bytes = %d, want %d", cfg.MaxPageBytes, defaultMaxPageBytes)
	}
}

// The núcleo sets this, so a bad value is a wiring mistake — but the mistake would put a service
// that fetches arbitrary URLs on the network, which is an open proxy with the owner's IP on it.
func TestAddrMustBeLoopback(t *testing.T) {
	withRequired(t)
	for _, addr := range []string{"0.0.0.0:8794", "192.168.1.10:8794", "[::]:8794", "example.com:8794"} {
		t.Setenv("WEB_ADDR", addr)
		if _, err := Load(); err == nil {
			t.Errorf("WEB_ADDR=%q was accepted — the sidecar would be reachable off this machine", addr)
		}
	}

	for _, addr := range []string{"127.0.0.1:8794", "localhost:9000", "[::1]:8794"} {
		t.Setenv("WEB_ADDR", addr)
		if _, err := Load(); err != nil {
			t.Errorf("WEB_ADDR=%q was refused: %v", addr, err)
		}
	}
}

func TestMalformedNumbersAreErrorsAndNotDefaults(t *testing.T) {
	withRequired(t)

	t.Setenv("WEB_FETCH_TIMEOUT_SECS", "soon")
	if _, err := Load(); err == nil {
		t.Error("a non-numeric timeout was silently defaulted")
	}
	t.Setenv("WEB_FETCH_TIMEOUT_SECS", "")

	t.Setenv("WEB_MAX_PAGE_BYTES", "lots")
	if _, err := Load(); err == nil {
		t.Error("a non-numeric page ceiling was silently defaulted")
	}
}

func TestTimeoutIsClamped(t *testing.T) {
	withRequired(t)

	t.Setenv("WEB_FETCH_TIMEOUT_SECS", "0")
	cfg, err := Load()
	if err != nil {
		t.Fatalf("load failed: %v", err)
	}
	if cfg.FetchTimeout < time.Second {
		t.Errorf("timeout = %v, want at least a second", cfg.FetchTimeout)
	}

	t.Setenv("WEB_FETCH_TIMEOUT_SECS", "9000")
	cfg, err = Load()
	if err != nil {
		t.Fatalf("load failed: %v", err)
	}
	if cfg.FetchTimeout > 120*time.Second {
		t.Errorf("timeout = %v, want it capped — a fetch holds a daemon request open", cfg.FetchTimeout)
	}
}

// A ceiling no page fits in is a configuration that makes the pillar silently useless. Better to
// refuse to start than to answer every read with "too large".
func TestAnAbsurdCeilingIsRefused(t *testing.T) {
	withRequired(t)
	t.Setenv("WEB_MAX_PAGE_BYTES", "10")
	_, err := Load()
	if err == nil {
		t.Fatal("a 10-byte page ceiling was accepted")
	}
	if !strings.Contains(err.Error(), "10") {
		t.Errorf("the error does not name the value that was wrong: %v", err)
	}
}
