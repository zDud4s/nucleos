// §spec pilar-de-browser

// Package config reads the sidecar's entire configuration from the environment.
//
// Like the web and email sidecars this process holds no config file of its own and stores nothing on
// disk. The owner's settings live in `~/.nucleos/browser.yaml`, are read by the núcleo, and arrive here as
// variables — so there is exactly one place where the pillar is configured, and it is the one the
// classifier guards (spec §3.5, §8).
package config

import (
	"errors"
	"fmt"
	"net"
	"os"
	"path/filepath"
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

	// Root is the install root of spec §5.6 — the pinned Chromium and the profiles, side by side.
	Root string
	// ExecutablePath overrides which browser is launched. Empty means the pinned one under Root,
	// which is the only thing the daemon ever sets; the override exists because a machine that has
	// not downloaded 300MB yet can still be developed on, and because the gate runs against a system
	// Chrome of the same major version.
	ExecutablePath string
	// CacheMB, MaxProfiles and DiskBudgetMB are spec §8's ceilings, read by the núcleo from
	// `~/.nucleos/browser.yaml`. They arrive here as numbers because this process has no config file: one
	// place is configured, and it is the one the classifier guards.
	CacheMB      int
	MaxProfiles  int
	DiskBudgetMB int64
}

// DefaultAddr follows the daemon (8791), echo (8792), email attachments (8793) and web (8794).
const DefaultAddr = "127.0.0.1:8795"

// The defaults match the `~/.nucleos/browser.yaml` spec §8 ships, so a sidecar started with nothing but a
// token behaves the way the documented configuration says it does.
const (
	defaultOpenTimeout  = 30 * time.Second
	defaultMaxSessions  = 3
	defaultCacheMB      = 100
	defaultMaxProfiles  = 20
	defaultDiskBudgetMB = 3000
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

	root := os.Getenv("BROWSER_ROOT")
	if root == "" {
		root = defaultRoot()
	}

	cacheMB, err := positive("BROWSER_CACHE_MB", defaultCacheMB)
	if err != nil {
		return Config{}, err
	}
	maxProfiles, err := positive("BROWSER_MAX_PROFILES", defaultMaxProfiles)
	if err != nil {
		return Config{}, err
	}
	diskBudgetMB, err := positive("BROWSER_DISK_BUDGET_MB", defaultDiskBudgetMB)
	if err != nil {
		return Config{}, err
	}

	return Config{
		DaemonURL:      daemonURL,
		DaemonToken:    token,
		Addr:           addr,
		Driver:         driver,
		OpenTimeout:    timeout,
		MaxSessions:    maxSessions,
		Root:           root,
		ExecutablePath: os.Getenv("BROWSER_EXECUTABLE"),
		CacheMB:        cacheMB,
		MaxProfiles:    maxProfiles,
		DiskBudgetMB:   int64(diskBudgetMB),
	}, nil
}

// defaultRoot is spec §5.6's layout. LOCALAPPDATA rather than APPDATA: these are hundreds of
// megabytes of browser and cache, and a roaming profile that carried them between machines would
// copy the owner's cookie jars along with them.
func defaultRoot() string {
	if local := os.Getenv("LOCALAPPDATA"); local != "" {
		return filepath.Join(local, "NucleOS", "browser")
	}
	// Off Windows the same argument picks the user CACHE directory (~/Library/Caches on macOS,
	// XDG_CACHE_HOME or ~/.cache on Linux): the pinned Chromium can be downloaded again. The old
	// ~/.local/share fallback ignored XDG_DATA_HOME and was the wrong place on macOS.
	if cache, err := os.UserCacheDir(); err == nil {
		return filepath.Join(cache, "nucleos", "browser")
	}
	// Deliberately relative and deliberately named: a root that silently became the working
	// directory would put profile directories wherever the daemon happened to be started from.
	return "nucleos-browser-root"
}

// positive reads a ceiling. A ceiling of zero would mean "no ceiling" to profile.Limits, and a
// negative one is a typo; both are refused here rather than turned into an unbounded disk.
func positive(name string, fallback int) (int, error) {
	raw := os.Getenv(name)
	if raw == "" {
		return fallback, nil
	}
	parsed, err := strconv.Atoi(raw)
	if err != nil {
		return 0, fmt.Errorf("%s is not a number: %w", name, err)
	}
	if parsed < 1 {
		return 0, fmt.Errorf("%s is %d, which is not a ceiling", name, parsed)
	}
	return parsed, nil
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
