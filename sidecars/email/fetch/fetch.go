// Package fetch serves one attachment's bytes to the núcleo, on demand.
//
// It exists because attachments are deliberately not stored (spec §1.5, migration 0021): the núcleo
// keeps a name, a type and a size, and the file itself stays in the mailbox until a person asks for
// it. Something has to go and get it at that moment, and IMAP lives on this side — duplicating an
// IMAP client in Rust would mean two implementations of the read-only invariant instead of one.
//
// This is the sidecar's only inbound surface. It binds to loopback, requires the daemon's token,
// and never writes anything: the mailbox stays read-only exactly as the poll loop leaves it.
package fetch

import (
	"crypto/subtle"
	"errors"
	"fmt"
	"log"
	"net/http"
	"net/url"
	"strconv"
	"time"

	imapv2 "github.com/emersion/go-imap/v2"

	"nucleosemail/config"
	"nucleosemail/extract"
	"nucleosemail/imap"
)

// HeaderTimeout bounds how long a client may take to send its headers. Small, because the only
// legitimate client is on the same machine.
const HeaderTimeout = 10 * time.Second

// Serve blocks, serving attachment requests until the process ends.
func Serve(cfg config.Config) error {
	mux := http.NewServeMux()
	mux.HandleFunc("/attachment", handler(cfg))
	server := &http.Server{
		Addr:              cfg.FetchAddr,
		Handler:           mux,
		ReadHeaderTimeout: HeaderTimeout,
	}
	log.Printf("serving attachments on %s", cfg.FetchAddr)
	return server.ListenAndServe()
}

func handler(cfg config.Config) http.HandlerFunc {
	return func(w http.ResponseWriter, r *http.Request) {
		if !authorized(r, cfg.DaemonToken) {
			http.Error(w, "unauthorized", http.StatusUnauthorized)
			return
		}
		uid, position, err := request(r)
		if err != nil {
			http.Error(w, err.Error(), http.StatusBadRequest)
			return
		}

		attachment, content, err := read(cfg, uid, position)
		if errors.Is(err, extract.ErrNoSuchAttachment) {
			// The stored description and the live message disagree — the message was replaced or
			// removed. A 404 rather than an empty file, which a caller would happily save.
			http.Error(w, "no attachment at that position", http.StatusNotFound)
			return
		}
		if err != nil {
			log.Printf("attachment %d of uid %d: %v", position, uid, err)
			http.Error(w, "could not read the attachment", http.StatusBadGateway)
			return
		}

		// ALWAYS octet-stream, never the type the sender declared. The bytes and the label both come
		// from a stranger, and a message that says `text/html` would otherwise be handed to a
		// browser as a page to run rather than a file to save. The declared type is shown in the UI
		// as information; it is not honoured here.
		w.Header().Set("Content-Type", "application/octet-stream")
		// Percent-encoded so a filename carrying CR or LF cannot inject a header of its own — the
		// name arrives from the sender and is the least trustworthy string in the exchange.
		w.Header().Set("X-Attachment-Filename", url.PathEscape(attachment.Filename))
		w.Header().Set("Content-Length", strconv.Itoa(len(content)))
		if _, err := w.Write(content); err != nil {
			log.Printf("writing attachment %d of uid %d: %v", position, uid, err)
		}
	}
}

// authorized compares in constant time: a token checked with `==` leaks its prefix to anything that
// can time the loopback interface.
func authorized(r *http.Request, token string) bool {
	const prefix = "Bearer "
	header := r.Header.Get("Authorization")
	if len(header) <= len(prefix) || header[:len(prefix)] != prefix {
		return false
	}
	return subtle.ConstantTimeCompare([]byte(header[len(prefix):]), []byte(token)) == 1
}

func request(r *http.Request) (imapv2.UID, int, error) {
	query := r.URL.Query()
	uid, err := strconv.ParseUint(query.Get("uid"), 10, 32)
	if err != nil || uid == 0 {
		return 0, 0, fmt.Errorf("uid must be a positive number")
	}
	position, err := strconv.Atoi(query.Get("position"))
	if err != nil || position < 0 {
		return 0, 0, fmt.Errorf("position must be zero or more")
	}
	return imapv2.UID(uid), position, nil
}

// read opens its own connection rather than borrowing the poll loop's.
//
// A TLS handshake per download is the cheaper mistake: an IMAP connection carries protocol state
// per command, and sharing one between a background poll and an interactive request would
// interleave two conversations on one socket.
func read(cfg config.Config, uid imapv2.UID, position int) (attachment, []byte, error) {
	conn, err := imap.Dial(cfg.Addr(), cfg.Username, cfg.Password)
	if err != nil {
		return attachment{}, nil, err
	}
	defer conn.Close()

	if _, err := conn.Select(cfg.Mailbox); err != nil {
		return attachment{}, nil, err
	}
	raw, err := conn.Fetch(uid)
	if err != nil {
		return attachment{}, nil, err
	}

	described, content, err := extract.Attachment(raw.Body, position)
	if err != nil {
		return attachment{}, nil, err
	}
	return attachment{Filename: described.Filename}, content, nil
}

// attachment is the sliver of the description this package needs on the way out.
type attachment struct {
	Filename string
}
