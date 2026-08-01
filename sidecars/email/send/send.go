// Package send hands one outbound message to the person's own submission server.
//
// It is the first thing this sidecar does that a mailbox owner cannot undo. Reading is recoverable
// — a bad poll is re-read — but a message that leaves is gone, so the framing is built here in one
// place, byte for byte, rather than assembled by whatever calls it. The núcleo decides what to say;
// this package only decides how it is put on the wire.
//
// The body never reaches a log. It is the person's own words to someone they know, and a process
// that writes them to disk on the way past has turned a mail client into a transcript.
package send

import (
	"crypto/rand"
	"crypto/tls"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"log"
	"net/http"
	"net/smtp"
	"strings"
	"time"

	"nucleosemail/config"
)

// Message is one outbound mail as the núcleo hands it over: a recipient, a subject, and plain text.
// No attachments and no HTML — what this pillar sends is what a person would have typed.
type Message struct {
	To      string `json:"to"`
	Subject string `json:"subject"`
	Body    string `json:"body"`
}

// Build frames the message as RFC 5322 bytes, ready for DATA.
//
// Pure on purpose: the time and the Message-ID arrive as arguments rather than being read inside,
// so the framing can be asserted against a constant instead of against whatever the clock says.
func Build(from, fromName string, msg Message, now time.Time, id string) ([]byte, error) {
	// A bare CR or LF in any of these is a second header. `To` and `Subject` come from the núcleo
	// relaying a model's output, which makes them the least trustworthy strings here: an accepted
	// newline in `To` is a recipient the sender never saw. The value is left out of the error —
	// whoever logs it should not end up holding the subject line.
	for _, field := range []struct{ name, value string }{
		{"From", from},
		{"From", fromName},
		{"To", msg.To},
		{"Subject", msg.Subject},
	} {
		if strings.ContainsAny(field.value, "\r\n") {
			return nil, fmt.Errorf("%s carries a line break, which would start a header of its own", field.name)
		}
	}

	sender := from
	if fromName != "" {
		sender = fmt.Sprintf("%s <%s>", fromName, from)
	}

	var framed strings.Builder
	header := func(name, value string) {
		framed.WriteString(name)
		framed.WriteString(": ")
		framed.WriteString(value)
		// CRLF, not LF: a lone LF is accepted by a forgiving server and mangled by a strict one,
		// and the difference only shows up in someone else's mailbox.
		framed.WriteString("\r\n")
	}

	header("From", sender)
	header("To", msg.To)
	header("Subject", msg.Subject)
	// RFC1123Z, so the offset is a number. RFC1123 would print the zone's name, which no reader
	// outside this machine can resolve back to an hour.
	header("Date", now.Format(time.RFC1123Z))
	// RFC 5322 wants an addr-spec shape here, and the sender's own domain is the only one this
	// process can honestly claim.
	header("Message-ID", "<"+id+"@"+domainOf(from)+">")
	header("MIME-Version", "1.0")
	header("Content-Type", "text/plain; charset=utf-8")
	framed.WriteString("\r\n")
	framed.WriteString(msg.Body)

	return []byte(framed.String()), nil
}

// domainOf is the right-hand side of an address, for the Message-ID.
func domainOf(address string) string {
	if at := strings.LastIndex(address, "@"); at >= 0 && at+1 < len(address) {
		return address[at+1:]
	}
	// An address with no domain is a wiring mistake upstream. The message still goes out with an
	// identifier that is unique, just not resolvable — better than a header that is absent.
	return "localhost"
}

// Send submits the message over an implicitly encrypted connection and waits for the server to
// accept it. It returns only once the server has taken responsibility for delivery.
func Send(cfg config.Config, msg Message) error {
	id, err := messageID()
	if err != nil {
		return err
	}
	framed, err := Build(cfg.Username, "", msg, time.Now(), id)
	if err != nil {
		return err
	}

	// Encrypted before the first byte of SMTP, so there is no plaintext greeting for anything in
	// between to answer by refusing STARTTLS — a downgrade that a client which retries in the clear
	// would never notice. The person's mail password crosses this socket.
	conn, err := tls.Dial("tcp", cfg.SMTPAddr(), &tls.Config{
		ServerName: cfg.SMTPHost,
		MinVersion: tls.VersionTLS12,
	})
	if err != nil {
		return fmt.Errorf("connecting to %s: %w", cfg.SMTPAddr(), err)
	}
	defer conn.Close()

	client, err := smtp.NewClient(conn, cfg.SMTPHost)
	if err != nil {
		return fmt.Errorf("greeting %s: %w", cfg.SMTPHost, err)
	}
	defer client.Close()

	if err := client.Auth(smtp.PlainAuth("", cfg.Username, cfg.Password, cfg.SMTPHost)); err != nil {
		return fmt.Errorf("authenticating as %s: %w", cfg.Username, err)
	}
	if err := client.Mail(cfg.Username); err != nil {
		return fmt.Errorf("sender rejected: %w", err)
	}
	if err := client.Rcpt(msg.To); err != nil {
		return fmt.Errorf("recipient rejected: %w", err)
	}

	body, err := client.Data()
	if err != nil {
		return fmt.Errorf("opening the message: %w", err)
	}
	if _, err := body.Write(framed); err != nil {
		return fmt.Errorf("writing the message: %w", err)
	}
	// The server's verdict arrives here, not on Write. Closing before Quit is what turns "sent" from
	// a hope into an answer.
	if err := body.Close(); err != nil {
		return fmt.Errorf("the server did not accept the message: %w", err)
	}
	return client.Quit()
}

// messageID is 96 random bits, which is enough that two messages from the same address never
// collide and little enough that the header stays readable.
func messageID() (string, error) {
	var raw [12]byte
	if _, err := rand.Read(raw[:]); err != nil {
		return "", fmt.Errorf("generating a Message-ID: %w", err)
	}
	return hex.EncodeToString(raw[:]), nil
}

// Handler answers the núcleo's request to send one message.
//
// It carries no authentication of its own: the mount in fetch.go puts the daemon's token in front
// of it, so this function is never reached by a request that did not present one.
func Handler(cfg config.Config) http.HandlerFunc {
	return func(w http.ResponseWriter, r *http.Request) {
		if r.Method != http.MethodPost {
			http.Error(w, "send takes POST", http.StatusMethodNotAllowed)
			return
		}

		var msg Message
		if err := json.NewDecoder(r.Body).Decode(&msg); err != nil {
			http.Error(w, "the body is not a message", http.StatusBadRequest)
			return
		}

		// Framed once here purely to refuse a message that cannot be framed at all. Send frames it
		// again with the identifier it will actually carry; the cost is one string, and the gain is
		// that a header injection is answered as the caller's mistake rather than reported as a
		// mail server's failure.
		if _, err := Build(cfg.Username, "", msg, time.Now(), ""); err != nil {
			http.Error(w, "the message cannot be framed", http.StatusBadRequest)
			return
		}

		if cfg.SMTPHost == "" {
			// Reading this mailbox works without a submission server, so this is the shape of the
			// configuration rather than a fault: temporary from the caller's side, and fixed by a
			// person rather than by a retry.
			http.Error(w, "sending is not configured", http.StatusServiceUnavailable)
			return
		}

		if err := Send(cfg, msg); err != nil {
			// The error, never the message: no subject and no body reach this log.
			log.Printf("send failed: %v", err)
			http.Error(w, "the message was not accepted", http.StatusBadGateway)
			return
		}

		w.WriteHeader(http.StatusNoContent)
	}
}
