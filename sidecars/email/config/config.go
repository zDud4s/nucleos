// §spec email-pillar

// Package config reads the sidecar's entire configuration from the environment.
//
// Everything arrives from the núcleo (spec §3.4): this process holds no config file of its own and
// stores no state on disk, which is what keeps the cursor a property of the database rather than a
// convention shared between two programs.
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
	DaemonURL    string
	DaemonToken  string
	Host         string
	Port         int
	Username     string
	Password     string
	Mailbox      string
	SentMailbox  string
	PollInterval time.Duration
	// FetchAddr is the loopback address this sidecar serves attachments on.
	FetchAddr string
	// SMTPHost is empty when the person has not configured sending. Reading a mailbox and sending
	// from it are two separate grants, and the first has always worked without the second — so an
	// empty value here is a configuration, not a failure: the send route answers 503 and the poll
	// loop carries on.
	SMTPHost string
	SMTPPort int
}

// Load reads the variables the núcleo injects (see `sidecar::email_env`). A missing
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
	sentMailbox := os.Getenv("EMAIL_SENT_MAILBOX")

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

	smtpHost := os.Getenv("EMAIL_SMTP_HOST")
	// Unlike the IMAP port, an unreadable value here is not fatal: mail still arrives without a
	// submission server, so the whole process is not worth killing over one variable. 465 is the
	// submission port that is TLS from the first byte, which is the only shape send/ speaks.
	smtpPort := DefaultSMTPPort
	if raw := os.Getenv("EMAIL_SMTP_PORT"); raw != "" {
		if parsed, err := strconv.Atoi(raw); err == nil {
			smtpPort = parsed
		}
	}

	fetchAddr := os.Getenv("EMAIL_FETCH_ADDR")
	if fetchAddr == "" {
		fetchAddr = DefaultFetchAddr
	}
	if err := requireLoopback(fetchAddr); err != nil {
		return Config{}, err
	}

	return Config{
		DaemonURL:    daemonURL,
		DaemonToken:  token,
		Host:         host,
		Port:         port,
		Username:     username,
		Password:     password,
		Mailbox:      mailbox,
		SentMailbox:  sentMailbox,
		PollInterval: interval,
		FetchAddr:    fetchAddr,
		SMTPHost:     smtpHost,
		SMTPPort:     smtpPort,
	}, nil
}

// DefaultFetchAddr is where attachments are served when the núcleo does not say otherwise. 8793
// follows the daemon (8791) and the echo sidecar (8792).
const DefaultFetchAddr = "127.0.0.1:8793"

// DefaultSMTPPort is submission over implicit TLS. Not 587: that one starts in plaintext and asks
// for TLS afterwards, and the credential this process holds is the person's own mail password.
const DefaultSMTPPort = 465

// requireLoopback refuses to open the attachment listener to anything but this machine.
//
// The núcleo sets this variable, so a bad value is a wiring mistake rather than an attack — but the
// mistake would put a service that reads a person's mailbox on the network, which is worth one
// check rather than one day of trust.
func requireLoopback(addr string) error {
	host, _, err := net.SplitHostPort(addr)
	if err != nil {
		return fmt.Errorf("EMAIL_FETCH_ADDR is not host:port: %w", err)
	}
	ip := net.ParseIP(host)
	if host == "localhost" || (ip != nil && ip.IsLoopback()) {
		return nil
	}
	return fmt.Errorf(
		"EMAIL_FETCH_ADDR must be a loopback address, got %q — attachments are never served off this machine",
		addr,
	)
}

// Addr is the dial target for the IMAP connection.
func (c Config) Addr() string {
	return fmt.Sprintf("%s:%d", c.Host, c.Port)
}

// SMTPAddr is the dial target for the submission connection. Meaningless while SMTPHost is empty,
// which is why every caller checks that first.
func (c Config) SMTPAddr() string {
	return fmt.Sprintf("%s:%d", c.SMTPHost, c.SMTPPort)
}
