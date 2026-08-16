// Package config reads the sidecar's entire configuration from the environment.
//
// Like the web and email sidecars this process holds no config file of its own and stores nothing on
// disk. The owner's settings live in `.ai/browser.yaml`, are read by the núcleo, and arrive here as
// variables — so there is exactly one place where the pillar is configured, and it is the one the
// classifier guards (spec §3.5, §8).
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
	// Driver names the implementation behind browser.Driver. Left deliberately open: the spike of
	// 2026-08-15 changed the answer twice without touching the contract, and it is not settled
	// until the second gate of spec §14.1a runs.
	Driver string
	// OpenTimeout bounds one navigation end to end.
	OpenTimeout time.Duration
	// MaxSessions caps how many browsers may be alive at once. A browser is hundreds of megabytes
	// of RAM and a GPU consumer; spec §9.6 wants a ceiling rather than a machine on its knees.
	MaxSessions int
}

// DefaultAddr follows the daemon (8791), echo (8792), email attachments (8793) and web (8794).
const DefaultAddr = "127.0.0.1:8795"

const (
	defaultOpenTimeout = 30 * time.Second
	defaultMaxSessions = 3
)

// Load reads the variables the núcleo injects (see `sidecar::browser_env`).
func Load() (Config, error) {
	daemonURL := os.Getenv("NUCLEOS_DAEMON_URL")
	if daemonURL == "" {
		daemonURL = "http://127.0.0.1:8791"
	}

	token := os.Getenv("NUCLEOS_DAEMON_TOKEN")
	if token == "" {
		return Config{}, errors.New("NUCLEOS_DAEMON_TOKEN is required")
	}

	addr := os.Getenv("BROWSER_ADDR")
	if addr == "" {
		addr = DefaultAddr
	}
	if err := requireLoopback(addr); err != nil {
		return Config{}, err
	}

	driver := os.Getenv("BROWSER_DRIVER")
	if driver == "" {
		driver = "fake"
	}

	timeout := defaultOpenTimeout
	if raw := os.Getenv("BROWSER_OPEN_TIMEOUT_SECS"); raw != "" {
		seconds, err := strconv.Atoi(raw)
		if err != nil {
			return Config{}, fmt.Errorf("BROWSER_OPEN_TIMEOUT_SECS is not a number: %w", err)
		}
		if seconds < 1 {
			seconds = 1
		}
		if seconds > 180 {
			seconds = 180
		}
		timeout = time.Duration(seconds) * time.Second
	}

	maxSessions := defaultMaxSessions
	if raw := os.Getenv("BROWSER_MAX_SESSIONS"); raw != "" {
		parsed, err := strconv.Atoi(raw)
		if err != nil {
			return Config{}, fmt.Errorf("BROWSER_MAX_SESSIONS is not a number: %w", err)
		}
		if parsed < 1 {
			return Config{}, fmt.Errorf("BROWSER_MAX_SESSIONS is %d, which allows no browsing at all", parsed)
		}
		maxSessions = parsed
	}

	return Config{
		DaemonURL:   daemonURL,
		DaemonToken: token,
		Addr:        addr,
		Driver:      driver,
		OpenTimeout: timeout,
		MaxSessions: maxSessions,
	}, nil
}

// requireLoopback refuses to open this sidecar's listener to anything but this machine.
//
// The web sidecar has the same check for the same reason, and here the stake is higher: this
// process drives browsers that hold the owner's logged-in sessions. A listener on the network would
// hand those to whoever asked.
func requireLoopback(addr string) error {
	host, _, err := net.SplitHostPort(addr)
	if err != nil {
		return fmt.Errorf("BROWSER_ADDR is not host:port: %w", err)
	}
	ip := net.ParseIP(host)
	if host == "localhost" || (ip != nil && ip.IsLoopback()) {
		return nil
	}
	return fmt.Errorf(
		"BROWSER_ADDR must be a loopback address, got %q — this sidecar drives logged-in profiles and is never served off this machine",
		addr,
	)
}
