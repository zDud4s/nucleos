// Package fetch serves one attachment's bytes to the núcleo, on demand.
//
// It exists because attachments are deliberately not stored (spec §1.5, migration 0021): the núcleo
// keeps a name, a type and a size, and the file itself stays in the mailbox until a person asks for
// it. Something has to go and get it at that moment, and IMAP lives on this side — duplicating an
// IMAP client in Rust would mean two implementations of the read-only invariant instead of one.
//
// This is the sidecar's only inbound surface, so the one outbound route — /send — is mounted here
// too rather than opening a second port. It binds to loopback and requires the daemon's token on
// every route without exception. The mailbox is still never written: the read-only invariant the
// poll loop keeps is untouched, and what /send changes is that this listener now also relays a
// message outbound over SMTP.
package fetch

import (
	"crypto/subtle"
	"encoding/base64"
	"encoding/json"
	"errors"
	"fmt"
	"log"
	"net/http"
	"net/url"
	"strconv"
	"time"

	imapv2 "github.com/emersion/go-imap/v2"

	"nucleosemail/config"
	"nucleosemail/daemon"
	"nucleosemail/extract"
	"nucleosemail/imap"
	"nucleosemail/send"
)

// HeaderTimeout bounds how long a client may take to send its headers. Small, because the only
// legitimate client is on the same machine.
const HeaderTimeout = 10 * time.Second

// Serve blocks, serving attachment requests until the process ends.
func Serve(cfg config.Config) error {
	mux := http.NewServeMux()
	mux.HandleFunc("/attachment", guarded(cfg, handler(cfg)))
	mux.HandleFunc("/attachments", guarded(cfg, allHandler(cfg)))
	mux.HandleFunc("/send", guarded(cfg, send.Handler(cfg)))
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

// bulkAttachment is one entry of the all-at-once answer. The content is base64 because the payload
// is JSON; it costs a third more over loopback and saves N-1 fetches of the whole message.
type bulkAttachment struct {
	Position  int    `json:"position"`
	Filename  string `json:"filename,omitempty"`
	MimeType  string `json:"mime_type,omitempty"`
	SizeBytes int64  `json:"size_bytes"`
	Content   string `json:"content_base64"`
}

// allHandler answers with every attachment of one message, read in a single pass.
//
// Worth its own route rather than a loop over the single one: that loop would fetch the entire
// message once per attachment, so a message carrying eight files was downloaded eight times over
// eight TLS connections to deliver the same bytes.
func allHandler(cfg config.Config) http.HandlerFunc {
	return func(w http.ResponseWriter, r *http.Request) {
		uid, err := requestUID(r)
		if err != nil {
			http.Error(w, err.Error(), http.StatusBadRequest)
			return
		}

		described, contents, err := readAll(cfg, uid)
		if err != nil {
			log.Printf("attachments of uid %d: %v", uid, err)
			http.Error(w, "could not read the attachments", http.StatusBadGateway)
			return
		}

		payload := make([]bulkAttachment, 0, len(described))
		for _, attachment := range described {
			content := contents[attachment.Position]
			payload = append(payload, bulkAttachment{
				Position:  attachment.Position,
				Filename:  attachment.Filename,
				MimeType:  attachment.MimeType,
				SizeBytes: attachment.SizeBytes,
				Content:   base64.StdEncoding.EncodeToString(content),
			})
		}

		w.Header().Set("Content-Type", "application/json")
		if err := json.NewEncoder(w).Encode(payload); err != nil {
			log.Printf("writing attachments of uid %d: %v", uid, err)
		}
	}
}

func readAll(cfg config.Config, uid imapv2.UID) ([]daemon.Attachment, map[int][]byte, error) {
	conn, err := imap.Dial(cfg.Addr(), cfg.Username, cfg.Password)
	if err != nil {
		return nil, nil, err
	}
	defer conn.Close()

	if _, err := conn.Select(cfg.Mailbox); err != nil {
		return nil, nil, err
	}
	raw, err := conn.Fetch(uid)
	if err != nil {
		return nil, nil, err
	}
	return extract.AllAttachments(raw.Body)
}

// guarded refuses anything not carrying the daemon's token before the handler behind it sees the
// request. One wrapper at the mux rather than a check repeated inside each handler: the repeated
// version is one route away from being forgotten, and the route that forgets it is the one that
// reads someone's mail or sends from their address.
func guarded(cfg config.Config, next http.HandlerFunc) http.HandlerFunc {
	return func(w http.ResponseWriter, r *http.Request) {
		if !authorized(r, cfg.DaemonToken) {
			http.Error(w, "unauthorized", http.StatusUnauthorized)
			return
		}
		next(w, r)
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
	uid, err := requestUID(r)
	if err != nil {
		return 0, 0, err
	}
	position, err := strconv.Atoi(r.URL.Query().Get("position"))
	if err != nil || position < 0 {
		return 0, 0, fmt.Errorf("position must be zero or more")
	}
	return uid, position, nil
}

func requestUID(r *http.Request) (imapv2.UID, error) {
	uid, err := strconv.ParseUint(r.URL.Query().Get("uid"), 10, 32)
	if err != nil || uid == 0 {
		return 0, fmt.Errorf("uid must be a positive number")
	}
	return imapv2.UID(uid), nil
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
