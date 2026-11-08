package config

import (
	"testing"
	"time"
)

// This sidecar answers with the owner's usage figures and reaches the vendor with the owner's
// token in hand, so a QUOTA_ADDR off this machine's loopback must never be accepted. Load is the
// one place QUOTA_ADDR reaches the check (netguard.RequireLoopbackAddr, tested there); this covers
// that wiring rather than the check itself.
func load_refuses_a_quota_addr_off_loopback(t *testing.T) {
	t.Setenv("NUCLEOS_DAEMON_TOKEN", "tok")
	t.Setenv("QUOTA_ADDR", "0.0.0.0:8796")

	if _, err := Load(); err == nil {
		t.Fatal("Load() = nil error for a QUOTA_ADDR off loopback, want an error")
	}
}

// The núcleo always sets this, so its absence is a wiring bug — reported clearly rather than
// leaving the sidecar to fail confusingly at its first outbound call.
func load_requires_a_daemon_token(t *testing.T) {
	t.Setenv("QUOTA_ADDR", DefaultAddr)

	if _, err := Load(); err == nil {
		t.Fatal("Load() = nil error with no NUCLEOS_DAEMON_TOKEN, want an error")
	}
}

// A malformed TTL is a wiring bug too — the núcleo writes these — and must fail rather than be
// silently clamped like a merely-unreasonable one.
func load_rejects_a_malformed_ttl_rather_than_clamping_it(t *testing.T) {
	t.Setenv("NUCLEOS_DAEMON_TOKEN", "tok")
	t.Setenv("QUOTA_SUCCESS_TTL_SECS", "not-a-number")

	if _, err := Load(); err == nil {
		t.Fatal("Load() = nil error for a malformed TTL, want an error")
	}
}

// An out-of-range but well-formed TTL is a different case: clamped to the nearest bound rather
// than refused, because the value itself is not evidence of a wiring mistake.
func load_clamps_an_out_of_range_ttl_rather_than_rejecting_it(t *testing.T) {
	t.Setenv("NUCLEOS_DAEMON_TOKEN", "tok")
	t.Setenv("QUOTA_ERROR_TTL_SECS", "999999")

	cfg, err := Load()
	if err != nil {
		t.Fatalf("Load() = %v, want a clamped value instead of an error", err)
	}
	if cfg.ErrorTTL != 600*time.Second { // the documented max
		t.Errorf("ErrorTTL = %v, want the 600s ceiling", cfg.ErrorTTL)
	}
}

// The nanoseconds bug (see the comment on the seconds() call sites in config.go) survives exactly
// the case the out-of-range test above cannot exercise: a well-formed, IN-RANGE value passed as a
// bare untyped constant would come back as that many nanoseconds, which for FetchTimeout's [1s, 60s]
// clamp still parses as "a number" and would even fail loudly (clamped up to 1s) rather than
// silently — except that 60 nanoseconds is what QUOTA_FETCH_TIMEOUT_SECS=15 became historically, and
// nothing here checks the actual unit of what comes back. Comparing against a real seconds value,
// for FetchTimeout and both TTLs, is the only thing that would have caught it.
func load_passes_in_range_values_through_as_seconds(t *testing.T) {
	t.Setenv("NUCLEOS_DAEMON_TOKEN", "tok")
	t.Setenv("QUOTA_FETCH_TIMEOUT_SECS", "15")
	t.Setenv("QUOTA_SUCCESS_TTL_SECS", "90")
	t.Setenv("QUOTA_ERROR_TTL_SECS", "20")

	cfg, err := Load()
	if err != nil {
		t.Fatalf("Load() = %v, want no error for in-range values", err)
	}
	if cfg.FetchTimeout != 15*time.Second {
		t.Errorf("FetchTimeout = %v, want 15s", cfg.FetchTimeout)
	}
	if cfg.SuccessTTL != 90*time.Second {
		t.Errorf("SuccessTTL = %v, want 90s", cfg.SuccessTTL)
	}
	if cfg.ErrorTTL != 20*time.Second {
		t.Errorf("ErrorTTL = %v, want 20s", cfg.ErrorTTL)
	}
}

func TestConfig(t *testing.T) {
	t.Run("Load refuses a QUOTA_ADDR off loopback", load_refuses_a_quota_addr_off_loopback)
	t.Run("Load requires a daemon token", load_requires_a_daemon_token)
	t.Run("Load rejects a malformed TTL rather than clamping it", load_rejects_a_malformed_ttl_rather_than_clamping_it)
	t.Run("Load clamps an out-of-range TTL rather than rejecting it", load_clamps_an_out_of_range_ttl_rather_than_rejecting_it)
	t.Run("Load passes in-range values through as seconds", load_passes_in_range_values_through_as_seconds)
}
