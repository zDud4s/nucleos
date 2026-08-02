// Package config reads the sidecar's entire configuration from the environment.
//
// Like the email sidecar, this process holds no config file of its own and stores nothing on disk.
// The owner's settings live in `.ai/web.yaml`, are read by the núcleo, and arrive here as variables
// — so there is exactly one place where the trust allowlist and the provider choice are written,
// and it is the one the classifier guards (spec §9).
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
	// Provider is "brave" or "searxng".
	Provider   string
	BraveKey   string
	SearxngURL string
	// FetchTimeout bounds one page read end to end.
	FetchTimeout time.Duration
	// MaxPageBytes is the ceiling a page may not exceed. Exceeding it is an error, never a
	// truncation — see fetch.ErrTooLarge.
	MaxPageBytes int64
}

// DefaultAddr follows the daemon (8791), echo (8792) and email attachments (8793).
const DefaultAddr = "127.0.0.1:8794"

const (
	defaultFetchTimeout = 20 * time.Second
	defaultMaxPageBytes = int64(2_000_000)
)

// Load reads the variables the núcleo injects (see `sidecar::web_env`).
func Load() (Config, error) {
	daemonURL := os.Getenv("NUCLEOS_DAEMON_URL")
	if daemonURL == "" {
		daemonURL = "http://127.0.0.1:8791"
	}

	token := os.Getenv("NUCLEOS_DAEMON_TOKEN")
	if token == "" {
		return Config{}, errors.New("NUCLEOS_DAEMON_TOKEN is required")
	}

	addr := os.Getenv("WEB_ADDR")
	if addr == "" {
		addr = DefaultAddr
	}
	if err := requireLoopback(addr); err != nil {
		return Config{}, err
	}

	provider := os.Getenv("WEB_SEARCH_PROVIDER")
	if provider == "" {
		provider = "brave"
	}

	timeout := defaultFetchTimeout
	if raw := os.Getenv("WEB_FETCH_TIMEOUT_SECS"); raw != "" {
		seconds, err := strconv.Atoi(raw)
		if err != nil {
			return Config{}, fmt.Errorf("WEB_FETCH_TIMEOUT_SECS is not a number: %w", err)
		}
		if seconds < 1 {
			seconds = 1
		}
		if seconds > 120 {
			// A fetch holds a request from the daemon open. Two minutes is already long past the
			// point where the answer is useful to whoever asked.
			seconds = 120
		}
		timeout = time.Duration(seconds) * time.Second
	}

	maxBytes := defaultMaxPageBytes
	if raw := os.Getenv("WEB_MAX_PAGE_BYTES"); raw != "" {
		parsed, err := strconv.ParseInt(raw, 10, 64)
		if err != nil {
			return Config{}, fmt.Errorf("WEB_MAX_PAGE_BYTES is not a number: %w", err)
		}
		if parsed < 1024 {
			return Config{}, fmt.Errorf("WEB_MAX_PAGE_BYTES is %d, which no page fits in", parsed)
		}
		maxBytes = parsed
	}

	return Config{
		DaemonURL:    daemonURL,
		DaemonToken:  token,
		Addr:         addr,
		Provider:     provider,
		BraveKey:     os.Getenv("WEB_BRAVE_KEY"),
		SearxngURL:   os.Getenv("WEB_SEARXNG_URL"),
		FetchTimeout: timeout,
		MaxPageBytes: maxBytes,
	}, nil
}

// requireLoopback refuses to open this sidecar's listener to anything but this machine.
//
// The núcleo sets the variable, so a bad value is a wiring mistake rather than an attack — but the
// mistake would put a service that fetches arbitrary URLs on the network, which turns a personal
// assistant into an open proxy. One check is cheaper than that day.
func requireLoopback(addr string) error {
	host, _, err := net.SplitHostPort(addr)
	if err != nil {
		return fmt.Errorf("WEB_ADDR is not host:port: %w", err)
	}
	ip := net.ParseIP(host)
	if host == "localhost" || (ip != nil && ip.IsLoopback()) {
		return nil
	}
	return fmt.Errorf(
		"WEB_ADDR must be a loopback address, got %q — this sidecar is never served off this machine",
		addr,
	)
}
