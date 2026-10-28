// Package config reads the remote gateway's environment.
//
// The daemon writes this environment (core/src/sidecar.rs, remote_env) and the control token is
// never part of it: the gateway holds only its own `remote` key. Everything here fails closed. A
// gateway that cannot prove it is bound to loopback and talking to a loopback daemon does not start.
package config

import (
	"errors"
	"fmt"
	"net"
	"net/url"
	"os"
	"strconv"
	"strings"
	"time"
)

// Environment variable names, fixed by the contract.
const (
	EnvDaemonURL    = "NUCLEOS_DAEMON_URL"
	EnvDaemonToken  = "NUCLEOS_DAEMON_TOKEN"
	EnvAddr         = "REMOTE_ADDR"
	EnvPublicOrigin = "REMOTE_PUBLIC_ORIGIN"
	EnvDistDir      = "REMOTE_DIST_DIR"
	EnvStreamMax    = "REMOTE_STREAM_MAX_SECONDS"
	EnvCookieMaxAge = "REMOTE_COOKIE_MAX_AGE_SECONDS"
	EnvMaxBody      = "REMOTE_MAX_BODY_BYTES"
)

// Defaults of the contract.
const (
	DefaultAddr         = "127.0.0.1:8797"
	DefaultStreamMax    = 120
	DefaultCookieMaxAge = 2592000 // 30 days
	DefaultMaxBody      = 64 << 20
	minStreamMax        = 10
	maxStreamMax        = 900
)

// Config is the validated gateway configuration.
type Config struct {
	// Addr is the listen address; always loopback.
	Addr string
	// DaemonURL is the núcleo's base URL, without a trailing slash; always loopback.
	DaemonURL string
	// DaemonToken is the `remote` service key. It is the only credential the gateway holds.
	DaemonToken string
	// PublicOrigin is the exact origin browsers use (https, no path), e.g. https://box.tail1234.ts.net.
	PublicOrigin string
	// PublicHost is the host[:port] part of PublicOrigin, what the Host header must equal.
	PublicHost string
	// DistDir is the built web shell served to browsers.
	DistDir string
	// StreamMax caps the lifetime of a proxied response of unknown length.
	StreamMax time.Duration
	// CookieMaxAge is the session cookie's Max-Age in seconds.
	CookieMaxAge int
	// MaxBodyBytes caps a request body.
	MaxBodyBytes int64
}

// Load reads the process environment.
func Load() (Config, error) { return LoadFrom(os.Getenv) }

// LoadFrom reads configuration through getenv, so tests need no process environment.
func LoadFrom(getenv func(string) string) (Config, error) {
	var cfg Config
	cfg.Addr = strings.TrimSpace(getenv(EnvAddr))
	if cfg.Addr == "" {
		cfg.Addr = DefaultAddr
	}
	if err := requireLoopbackAddr(cfg.Addr); err != nil {
		return Config{}, fmt.Errorf("%s: %w", EnvAddr, err)
	}

	daemon := strings.TrimRight(strings.TrimSpace(getenv(EnvDaemonURL)), "/")
	if daemon == "" {
		return Config{}, fmt.Errorf("%s is required", EnvDaemonURL)
	}
	if err := requireLoopbackURL(daemon); err != nil {
		return Config{}, fmt.Errorf("%s: %w", EnvDaemonURL, err)
	}
	cfg.DaemonURL = daemon

	cfg.DaemonToken = strings.TrimSpace(getenv(EnvDaemonToken))
	if cfg.DaemonToken == "" {
		return Config{}, fmt.Errorf("%s is required", EnvDaemonToken)
	}

	origin, host, err := parsePublicOrigin(getenv(EnvPublicOrigin))
	if err != nil {
		return Config{}, fmt.Errorf("%s: %w", EnvPublicOrigin, err)
	}
	cfg.PublicOrigin, cfg.PublicHost = origin, host

	cfg.DistDir = strings.TrimSpace(getenv(EnvDistDir))
	if cfg.DistDir == "" {
		return Config{}, fmt.Errorf("%s is required", EnvDistDir)
	}

	streamSecs, err := intVar(getenv, EnvStreamMax, DefaultStreamMax)
	if err != nil {
		return Config{}, err
	}
	if streamSecs < minStreamMax || streamSecs > maxStreamMax {
		return Config{}, fmt.Errorf("%s must be within %d..=%d", EnvStreamMax, minStreamMax, maxStreamMax)
	}
	cfg.StreamMax = time.Duration(streamSecs) * time.Second

	cfg.CookieMaxAge, err = intVar(getenv, EnvCookieMaxAge, DefaultCookieMaxAge)
	if err != nil {
		return Config{}, err
	}
	if cfg.CookieMaxAge <= 0 {
		return Config{}, fmt.Errorf("%s must be positive", EnvCookieMaxAge)
	}

	body, err := intVar(getenv, EnvMaxBody, DefaultMaxBody)
	if err != nil {
		return Config{}, err
	}
	if body <= 0 {
		return Config{}, fmt.Errorf("%s must be positive", EnvMaxBody)
	}
	cfg.MaxBodyBytes = int64(body)
	return cfg, nil
}

func intVar(getenv func(string) string, name string, def int) (int, error) {
	raw := strings.TrimSpace(getenv(name))
	if raw == "" {
		return def, nil
	}
	n, err := strconv.Atoi(raw)
	if err != nil {
		return 0, fmt.Errorf("%s: not a whole number: %q", name, raw)
	}
	return n, nil
}

// isLoopbackHost accepts "localhost" and loopback IP literals, nothing resolved through DNS.
func isLoopbackHost(host string) bool {
	if strings.EqualFold(host, "localhost") {
		return true
	}
	ip := net.ParseIP(host)
	return ip != nil && ip.IsLoopback()
}

func requireLoopbackAddr(addr string) error {
	host, _, err := net.SplitHostPort(addr)
	if err != nil {
		return fmt.Errorf("not a host:port address: %q", addr)
	}
	if !isLoopbackHost(host) {
		return fmt.Errorf("must bind loopback only, got %q", addr)
	}
	return nil
}

func requireLoopbackURL(raw string) error {
	u, err := url.Parse(raw)
	if err != nil || u.Host == "" {
		return errors.New("not a valid URL")
	}
	if u.Scheme != "http" && u.Scheme != "https" {
		return fmt.Errorf("unsupported scheme %q", u.Scheme)
	}
	if !isLoopbackHost(u.Hostname()) {
		return fmt.Errorf("must be a loopback address, got host %q", u.Hostname())
	}
	return nil
}

// parsePublicOrigin accepts only `https://host[:port]`: no path, query, fragment or userinfo.
func parsePublicOrigin(raw string) (origin, host string, err error) {
	raw = strings.TrimSpace(raw)
	if raw == "" {
		return "", "", errors.New("is required")
	}
	u, perr := url.Parse(raw)
	if perr != nil || u.Host == "" {
		return "", "", errors.New("not a valid URL")
	}
	if u.Scheme != "https" {
		return "", "", errors.New("must use https")
	}
	if (u.Path != "" && u.Path != "/") || u.RawQuery != "" || u.Fragment != "" || u.User != nil || u.Opaque != "" {
		return "", "", errors.New("must be a bare origin without path, query or userinfo")
	}
	host = strings.ToLower(u.Host)
	return "https://" + host, host, nil
}
