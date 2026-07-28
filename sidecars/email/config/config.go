// Package config reads the sidecar's entire configuration from the environment.
//
// Everything arrives from the núcleo (spec §3.4): this process holds no config file of its own and
// stores no state on disk, which is what keeps the cursor a property of the database rather than a
// convention shared between two programs.
package config

import (
	"errors"
	"fmt"
	"os"
	"strconv"
	"time"
)

type Config struct {
	DaemonURL    string
	DaemonToken  string
	Host         string
	Port         int
	Username     string
	Password     string
	Mailbox      string
	PollInterval time.Duration
}

// Load reads the eight variables the núcleo injects (see `sidecar::email_env`). A missing
// credential is an error rather than a default: a sidecar that starts without one would sit in a
// restart loop against a real mail server.
func Load() (Config, error) {
	daemonURL := os.Getenv("NUCLEOS_DAEMON_URL")
	if daemonURL == "" {
		daemonURL = "http://127.0.0.1:8791"
	}

	token := os.Getenv("NUCLEOS_DAEMON_TOKEN")
	if token == "" {
		return Config{}, errors.New("NUCLEOS_DAEMON_TOKEN is required")
	}

	host := os.Getenv("EMAIL_IMAP_HOST")
	if host == "" {
		return Config{}, errors.New("EMAIL_IMAP_HOST is required")
	}

	username := os.Getenv("EMAIL_IMAP_USERNAME")
	if username == "" {
		return Config{}, errors.New("EMAIL_IMAP_USERNAME is required")
	}

	password := os.Getenv("EMAIL_IMAP_PASSWORD")
	if password == "" {
		return Config{}, errors.New("EMAIL_IMAP_PASSWORD is required")
	}

	port := 993
	if raw := os.Getenv("EMAIL_IMAP_PORT"); raw != "" {
		parsed, err := strconv.Atoi(raw)
		if err != nil {
			return Config{}, fmt.Errorf("EMAIL_IMAP_PORT is not a number: %w", err)
		}
		port = parsed
	}

	mailbox := os.Getenv("EMAIL_MAILBOX")
	if mailbox == "" {
		mailbox = "INBOX"
	}

	interval := 300 * time.Second
	if raw := os.Getenv("EMAIL_POLL_INTERVAL_SECS"); raw != "" {
		seconds, err := strconv.Atoi(raw)
		if err != nil {
			return Config{}, fmt.Errorf("EMAIL_POLL_INTERVAL_SECS is not a number: %w", err)
		}
		if seconds < 30 {
			// A mailbox is not a queue to spin on, and the núcleo's own batching already smooths
			// arrival. A floor here stops a typo from turning into a rate-limit ban.
			seconds = 30
		}
		interval = time.Duration(seconds) * time.Second
	}

	return Config{
		DaemonURL:    daemonURL,
		DaemonToken:  token,
		Host:         host,
		Port:         port,
		Username:     username,
		Password:     password,
		Mailbox:      mailbox,
		PollInterval: interval,
	}, nil
}

// Addr is the dial target for the IMAP connection.
func (c Config) Addr() string {
	return fmt.Sprintf("%s:%d", c.Host, c.Port)
}
