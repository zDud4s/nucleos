package config

import (
	"strings"
	"testing"
)

func TestSentMailboxIsOptional(t *testing.T) {
	t.Setenv("NUCLEOS_DAEMON_TOKEN", "token")
	t.Setenv("EMAIL_IMAP_HOST", "imap.example.com")
	t.Setenv("EMAIL_IMAP_USERNAME", "me@example.com")
	t.Setenv("EMAIL_IMAP_PASSWORD", "password")
	t.Setenv("EMAIL_IMAP_PORT", "")
	t.Setenv("EMAIL_MAILBOX", "")
	t.Setenv("EMAIL_POLL_INTERVAL_SECS", "")
	t.Setenv("EMAIL_FETCH_ADDR", "")

	t.Run("unconfigured", func(t *testing.T) {
		t.Setenv("EMAIL_SENT_MAILBOX", "")

		cfg, err := Load()
		if err != nil {
			t.Fatalf("Load() returned an error without EMAIL_SENT_MAILBOX: %v", err)
		}
		if cfg.SentMailbox != "" {
			t.Fatalf("SentMailbox = %q, want empty", cfg.SentMailbox)
		}
	})

	t.Run("configured", func(t *testing.T) {
		const mailbox = "[Gmail]/Sent Mail"
		t.Setenv("EMAIL_SENT_MAILBOX", mailbox)

		cfg, err := Load()
		if err != nil {
			t.Fatalf("Load() returned an error with EMAIL_SENT_MAILBOX: %v", err)
		}
		if cfg.SentMailbox != mailbox {
			t.Fatalf("SentMailbox = %q, want %q", cfg.SentMailbox, mailbox)
		}
	})
}

// The núcleo sets this address, so a bad value is a wiring mistake — but the mistake would put a
// service that reads a mailbox on the network.
func TestFetchAddrMustBeLoopback(t *testing.T) {
	t.Setenv("NUCLEOS_DAEMON_TOKEN", "token")
	t.Setenv("EMAIL_IMAP_HOST", "imap.example.com")
	t.Setenv("EMAIL_IMAP_USERNAME", "me@example.com")
	t.Setenv("EMAIL_IMAP_PASSWORD", "password")
	t.Setenv("EMAIL_IMAP_PORT", "")
	t.Setenv("EMAIL_POLL_INTERVAL_SECS", "")
	for _, addr := range []string{"0.0.0.0:8793", "192.168.1.10:8793", "example.com:8793", "8793"} {
		t.Setenv("EMAIL_FETCH_ADDR", addr)
		if _, err := Load(); err == nil {
			t.Errorf("accepted %q", addr)
		}
	}
	for _, addr := range []string{"127.0.0.1:8793", "localhost:8793", "[::1]:8793"} {
		t.Setenv("EMAIL_FETCH_ADDR", addr)
		if _, err := Load(); err != nil {
			t.Errorf("rejected %q: %v", addr, err)
		}
	}
}

// The daemon URL is where the sidecar sends its token, so it must stay on this machine.
func TestDaemonURLMustBeLoopback(t *testing.T) {
	t.Setenv("NUCLEOS_DAEMON_TOKEN", "token")
	t.Setenv("EMAIL_IMAP_HOST", "imap.example.com")
	t.Setenv("EMAIL_IMAP_USERNAME", "me@example.com")
	t.Setenv("EMAIL_IMAP_PASSWORD", "password")
	t.Setenv("EMAIL_IMAP_PORT", "")
	t.Setenv("EMAIL_POLL_INTERVAL_SECS", "")
	t.Setenv("EMAIL_FETCH_ADDR", "")
	for _, raw := range []string{"http://example.com:8791", "http://10.0.0.2:8791", "ftp://127.0.0.1:8791"} {
		t.Setenv("NUCLEOS_DAEMON_URL", raw)
		if _, err := Load(); err == nil || !strings.Contains(err.Error(), "NUCLEOS_DAEMON_URL") {
			t.Errorf("accepted %q (err = %v)", raw, err)
		}
	}
	for _, raw := range []string{"http://127.0.0.1:8791", "http://[::1]:8791", "http://localhost:8791"} {
		t.Setenv("NUCLEOS_DAEMON_URL", raw)
		if _, err := Load(); err != nil {
			t.Errorf("rejected %q: %v", raw, err)
		}
	}
}
