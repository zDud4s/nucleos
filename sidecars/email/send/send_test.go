package send

import (
	"crypto/subtle"
	"net/http"
	"net/http/httptest"
	"os"
	"strings"
	"testing"
	"time"

	"nucleosemail/config"
)

// The framing is asserted byte for byte rather than header by header, because a message is one
// string on the wire: a header in the wrong order, a lone LF where CRLF belongs, or a missing blank
// line between headers and body are all invisible to a per-header assertion and all break delivery
// at a real server. The user calibrates this against their own mailbox — a message that leaves
// malformed is one they have to apologise for.
func TestMessageFramingAndTLSRequirement(t *testing.T) {
	// Fixed inputs, so the golden below is a constant rather than something recomputed alongside the
	// code it checks. The zone is deliberately not UTC: RFC1123Z prints "+0100" here where RFC1123
	// would print "WEST", so the golden fails if the wrong one of the two is used.
	now := time.Date(2026, time.August, 1, 9, 30, 0, 0, time.FixedZone("WEST", 60*60))
	const id = "0b1f2c3d4e5f"

	t.Run("framing", func(t *testing.T) {
		framed, err := Build("ana@example.com", "Ana Silva", Message{
			To:      "bruno@example.com",
			Subject: "Revised quote",
			Body:    "Sent from the sidecar.",
		}, now, id)
		if err != nil {
			t.Fatalf("Build: %v", err)
		}

		want := "From: Ana Silva <ana@example.com>\r\n" +
			"To: bruno@example.com\r\n" +
			"Subject: Revised quote\r\n" +
			"Date: Sat, 01 Aug 2026 09:30:00 +0100\r\n" +
			// RFC 5322 wants an addr-spec shape here, and the sender's own domain is the only one
			// this process can honestly claim.
			"Message-ID: <0b1f2c3d4e5f@example.com>\r\n" +
			"MIME-Version: 1.0\r\n" +
			"Content-Type: text/plain; charset=utf-8\r\n" +
			"\r\n" +
			"Sent from the sidecar."
		if string(framed) != want {
			t.Fatalf("Build framed\n%q\nwant\n%q", framed, want)
		}
	})

	// A bare CR or LF inside a header value is a second header. Both of these fields arrive from the
	// núcleo relaying a model's output, which makes them the least trustworthy strings in the
	// exchange: an accepted newline in `To` is a silent Bcc the person never saw.
	t.Run("header injection", func(t *testing.T) {
		for _, injection := range []string{"\r", "\n", "\r\n"} {
			for _, field := range []struct {
				name string
				msg  Message
			}{
				{"To", Message{
					To:      "bruno@example.com" + injection + "Bcc: quiet@example.com",
					Subject: "Revised quote",
					Body:    "Sent from the sidecar.",
				}},
				{"Subject", Message{
					To:      "bruno@example.com",
					Subject: "Revised quote" + injection + "Bcc: quiet@example.com",
					Body:    "Sent from the sidecar.",
				}},
			} {
				if _, err := Build("ana@example.com", "Ana Silva", field.msg, now, id); err == nil {
					t.Errorf("%s accepted %q", field.name, injection)
				}
			}
		}
	})

	// Asserted against the source rather than against a server, the same way the mailbox's read-only
	// invariant is (imap/readonly_test.go). A fake SMTP server would only prove that today's call
	// path negotiates TLS; this fails the moment a plaintext dial appears anywhere in the package,
	// including on a path no test happens to exercise. The password crosses this socket.
	t.Run("implicit TLS only", func(t *testing.T) {
		source, err := os.ReadFile("send.go")
		if err != nil {
			t.Fatal(err)
		}
		text := string(source)

		if !strings.Contains(text, "tls.Dial(") {
			t.Error("the connection must be opened with tls.Dial: implicit TLS, no negotiation to downgrade")
		}
		if strings.Contains(text, "smtp.Dial(") {
			t.Error("smtp.Dial( opens the socket in plaintext, and there is no fallback to it here")
		}
	})
}

// guarded mirrors the wrapper fetch.go puts in front of this route: a constant-time bearer check
// that refuses before the handler ever sees the request. It is written here because that wrapper is
// unexported in its own package, and the shape is deliberately identical to fetch.go's `authorized`
// so the requests this composition refuses are the requests the real mount refuses.
func guarded(cfg config.Config, next http.HandlerFunc) http.HandlerFunc {
	return func(w http.ResponseWriter, r *http.Request) {
		const prefix = "Bearer "
		header := r.Header.Get("Authorization")
		if len(header) <= len(prefix) || header[:len(prefix)] != prefix ||
			subtle.ConstantTimeCompare([]byte(header[len(prefix):]), []byte(cfg.DaemonToken)) != 1 {
			http.Error(w, "unauthorized", http.StatusUnauthorized)
			return
		}
		next(w, r)
	}
}

// This route sends mail from a person's own address. Every way of arriving without the daemon's
// token has to be a refusal that never reaches the handler, and a request that arrives with the
// token but without a message the sidecar can read has to stop here rather than at a mail server.
func TestTheSendRouteRefusesAnUnauthenticatedOrMalformedRequest(t *testing.T) {
	cfg := config.Config{
		DaemonToken: "secret",
		// Non-empty on purpose: an unconfigured SMTPHost answers 503, which would mask the 400 the
		// malformed body is supposed to produce.
		SMTPHost: "smtp.example.com",
		SMTPPort: 465,
	}

	reached := false
	route := guarded(cfg, func(w http.ResponseWriter, r *http.Request) {
		reached = true
		Handler(cfg)(w, r)
	})

	const wellFormed = `{"to":"bruno@example.com","subject":"Revised quote","body":"Sent from the sidecar."}`
	for _, header := range []string{
		"",
		"Bearer ",
		"Bearer wrong",
		"Bearer secre",   // a prefix of the real one
		"Bearer secrets", // the real one with more after it
		"secret",         // no scheme
		"bearer secret",  // the scheme is case-sensitive here by choice
		"Basic secret",
	} {
		reached = false
		recorder := httptest.NewRecorder()
		r := httptest.NewRequest(http.MethodPost, "/send", strings.NewReader(wellFormed))
		if header != "" {
			r.Header.Set("Authorization", header)
		}
		route(recorder, r)

		if recorder.Code != http.StatusUnauthorized {
			t.Errorf("%q answered %d, want 401", header, recorder.Code)
		}
		// The body was valid, so anything past the guard would have tried to send this mail.
		if reached {
			t.Errorf("%q reached the handler", header)
		}
	}

	reached = false
	recorder := httptest.NewRecorder()
	r := httptest.NewRequest(http.MethodPost, "/send", strings.NewReader("{not json"))
	r.Header.Set("Authorization", "Bearer secret")
	route(recorder, r)

	if !reached {
		t.Fatal("the guard refused a request carrying the daemon's token")
	}
	if recorder.Code != http.StatusBadRequest {
		t.Fatalf("a body that does not decode answered %d, want 400", recorder.Code)
	}
}
