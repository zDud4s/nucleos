// Package config reads the sidecar's entire configuration from the environment.
//
// Like the web and email sidecars, this process holds no config file of its own. The one thing it
// reads off disk that is not a variable is the owner's Claude credential file, and that is read at
// the point of use by package claude — never copied here, never logged, never persisted (design
// D2).
package config

import (
	"errors"
	"fmt"
	"net"
	"os"
	"strconv"
	"time"
)

type Config struct {
	DaemonURL   string
	DaemonToken string
	// Addr is the loopback address this sidecar serves the núcleo on. Its only inbound surface.
	Addr string
	// FetchTimeout bounds the one outbound call this process makes.
	FetchTimeout time.Duration
	// SuccessTTL and ErrorTTL are how long a good and a failed reading are held before the endpoint
	// is asked again. The núcleo polls on its own cadence; this is the floor that stops a poll
	// storm reaching the vendor.
	SuccessTTL time.Duration
	ErrorTTL   time.Duration
}

// DefaultAddr follows the daemon (8791), echo (8792), email attachments (8793), web (8794) and
// browser (8795). The núcleo's copy of this fact is `QUOTA_ADDR` in `core/src/sidecar.rs`; the two
// must agree, which is why both are constants rather than settings.
const DefaultAddr = "127.0.0.1:8796"

const (
	defaultFetchTimeout = 15 * time.Second
	// 60s on success and 10s on error are not invented here: they are the TTLs the Python dashboard
	// settled on against this same endpoint (`.ai/dashboard/server/usage.py`), and the whole point
	// of a quota reader is that it must not itself become a reason to run out of quota.
	defaultSuccessTTL = 60 * time.Second
	defaultErrorTTL   = 10 * time.Second
)

// Load reads the variables the núcleo injects (see `sidecar::quota_env`).
func Load() (Config, error) {
	daemonURL := os.Getenv("NUCLEOS_DAEMON_URL")
	if daemonURL == "" {
		daemonURL = "http://127.0.0.1:8791"
	}

	token := os.Getenv("NUCLEOS_DAEMON_TOKEN")
	if token == "" {
		return Config{}, errors.New("NUCLEOS_DAEMON_TOKEN is required")
	}

	addr := os.Getenv("QUOTA_ADDR")
	if addr == "" {
		addr = DefaultAddr
	}
	if err := requireLoopback(addr); err != nil {
		return Config{}, err
	}

	// The bounds below are seconds, and must be scaled by time.Second before being passed in: min
	// and max are time.Duration parameters, and a bare untyped constant assigned to a Duration is
	// nanoseconds. Passing plain "60" here once clamped every fetch to 60 NANOSECONDS instead of 60
	// seconds — silently, since a valid QUOTA_FETCH_TIMEOUT_SECS is exactly what triggers the clamp.
	timeout, err := seconds("QUOTA_FETCH_TIMEOUT_SECS", defaultFetchTimeout, 1*time.Second, 60*time.Second)
	if err != nil {
		return Config{}, err
	}
	successTTL, err := seconds("QUOTA_SUCCESS_TTL_SECS", defaultSuccessTTL, 5*time.Second, 3600*time.Second)
	if err != nil {
		return Config{}, err
	}
	errorTTL, err := seconds("QUOTA_ERROR_TTL_SECS", defaultErrorTTL, 1*time.Second, 600*time.Second)
	if err != nil {
		return Config{}, err
	}

	return Config{
		DaemonURL:    daemonURL,
		DaemonToken:  token,
		Addr:         addr,
		FetchTimeout: timeout,
		SuccessTTL:   successTTL,
		ErrorTTL:     errorTTL,
	}, nil
}

// seconds reads a duration variable, clamping rather than failing when it is merely unreasonable.
// A malformed value is still an error: the núcleo writes these, so garbage is a wiring bug.
func seconds(name string, fallback, min, max time.Duration) (time.Duration, error) {
	raw := os.Getenv(name)
	if raw == "" {
		return fallback, nil
	}
	n, err := strconv.Atoi(raw)
	if err != nil {
		return 0, fmt.Errorf("%s is not a number: %w", name, err)
	}
	d := time.Duration(n) * time.Second
	if d < min {
		d = min
	}
	if d > max {
		d = max
	}
	return d, nil
}

// requireLoopback refuses to open this sidecar's listener to anything but this machine.
//
// The núcleo sets the variable, so a bad value is a wiring mistake rather than an attack — but this
// process answers with the owner's usage figures and reaches the vendor with the owner's token in
// hand. A listener off this machine would publish the first and invite abuse of the second.
func requireLoopback(addr string) error {
	host, _, err := net.SplitHostPort(addr)
	if err != nil {
		return fmt.Errorf("QUOTA_ADDR is not host:port: %w", err)
	}
	ip := net.ParseIP(host)
	if host == "localhost" || (ip != nil && ip.IsLoopback()) {
		return nil
	}
	return fmt.Errorf(
		"QUOTA_ADDR must be a loopback address, got %q — this sidecar is never served off this machine",
		addr,
	)
}
