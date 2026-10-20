package kit

import (
	"errors"
	"fmt"
	"net"
	"net/url"
	"strings"
)

// Environment variables the daemon sets on a channel adapter (spec 5.4, env_for).
const (
	envDaemonURL  = "NUCLEOS_DAEMON_URL"
	envChannelTok = "NUCLEOS_CHANNEL_TOKEN"
	envChannelID  = "NUCLEOS_CHANNEL_ID"
)

// ConfigFromEnv builds the Config of an adapter from its environment. The daemon URL
// and the channel token are required; the channel id is optional here (the adapter
// may set it), and Run insists on it. The URL must point at the local machine: the
// channel token must never travel to another host.
func ConfigFromEnv(getenv func(string) string) (Config, error) {
	raw := getenv(envDaemonURL)
	if raw == "" {
		return Config{}, fmt.Errorf("%s is required", envDaemonURL)
	}
	if err := validateLoopbackURL(raw); err != nil {
		return Config{}, err
	}
	token := getenv(envChannelTok)
	if token == "" {
		return Config{}, fmt.Errorf("%s is required", envChannelTok)
	}
	return Config{BaseURL: raw, Channel: getenv(envChannelID), Token: token}, nil
}

// validateLoopbackURL accepts only an http(s) URL whose host is loopback.
func validateLoopbackURL(raw string) error {
	parsed, err := url.Parse(raw)
	if err != nil {
		return fmt.Errorf("%s %q is not a URL: %w", envDaemonURL, raw, err)
	}
	if parsed.Scheme != "http" && parsed.Scheme != "https" {
		return fmt.Errorf("%s %q must be http or https", envDaemonURL, raw)
	}
	host := parsed.Hostname()
	if host == "" {
		return fmt.Errorf("%s %q has no host", envDaemonURL, raw)
	}
	if !isLoopbackHost(host) {
		return errors.New(envDaemonURL + " " + fmt.Sprintf("%q", raw) +
			" is not loopback: the channel token must not leave this machine")
	}
	return nil
}

func isLoopbackHost(host string) bool {
	if strings.EqualFold(host, "localhost") {
		return true
	}
	ip := net.ParseIP(host)
	return ip != nil && ip.IsLoopback()
}
