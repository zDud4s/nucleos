// Package fetch retrieves one page from a destination somebody else chose.
//
// Every value in this package is a limit. That is the whole design: the caller controls the URL, so
// the only things this side controls are how far it will follow, how long it will wait, how much it
// will read, and what it will accept. See `safe` for where the destination itself is judged.
package fetch

import (
	"context"
	"errors"
	"fmt"
	"io"
	"net"
	"net/http"
	"strings"
	"time"

	"nucleosweb/safe"
)

// MaxRedirects bounds the chain. Ten is what browsers settle on; the number matters less than
// having one, because a redirect loop with no cap is a hang, and a hang holds a slot.
const MaxRedirects = 10

// ErrTooLarge is returned when a page exceeds the byte ceiling.
//
// It is an ERROR and not a truncation, and that is a decision with a precedent in this codebase:
// `transcribe.rs` errors rather than clipping for the same reason. A page cut in half is
// indistinguishable from a page that ended there, and a confident summary of half a document is
// worse than an honest failure.
var ErrTooLarge = errors.New("page exceeds the size ceiling")

// ErrNotHTML is returned for anything that is not a web page. Checked from the response header
// before the body is read, so a PDF or a video costs one round trip and not a download.
var ErrNotHTML = errors.New("response is not HTML or plain text")

// Page is one successful read.
type Page struct {
	// FinalURL is where the content actually came from, after every redirect. The trust decision in
	// the núcleo is made over THIS, never over what was requested — see the redirect trap, spec §10.1.
	FinalURL string
	// RequestedURL is what the caller asked for. Kept so the núcleo can see that they differ.
	RequestedURL string
	ContentType  string
	Body         string
}

// Client fetches pages. It cannot be constructed without the SSRF guard: the dialer is built in
// New, not passed in, so there is no configuration in which this type exists without it.
type Client struct {
	http     *http.Client
	maxBytes int64
}

// New builds the guarded client.
func New(timeout time.Duration, maxBytes int64) *Client {
	transport := &http.Transport{
		DialContext: (&net.Dialer{
			Timeout: 10 * time.Second,
			// The guard. Runs after DNS resolution, for every connection, including each redirect
			// hop — which is what makes it immune to a name that resolves differently the second
			// time it is asked.
			Control: safe.Control,
		}).DialContext,
		TLSHandshakeTimeout:   10 * time.Second,
		ResponseHeaderTimeout: timeout,
		// A page is fetched once and cached by the núcleo, so pooled connections buy little and
		// keep sockets open to hosts chosen by somebody else.
		DisableKeepAlives: true,
	}

	client := &http.Client{
		Transport: transport,
		Timeout:   timeout,
		CheckRedirect: func(request *http.Request, via []*http.Request) error {
			if len(via) >= MaxRedirects {
				return fmt.Errorf("stopped after %d redirects", MaxRedirects)
			}
			// The scheme is re-checked on every hop. The dialer covers the address; nothing else
			// covers a 302 into `file://`, which never reaches a dialer at all.
			if _, err := safe.CheckURL(request.URL.String()); err != nil {
				return err
			}
			return nil
		},
	}

	return &Client{http: client, maxBytes: maxBytes}
}

// Fetch reads one page.
func (c *Client) Fetch(ctx context.Context, rawURL string) (Page, error) {
	parsed, err := safe.CheckURL(rawURL)
	if err != nil {
		return Page{}, err
	}

	request, err := http.NewRequestWithContext(ctx, http.MethodGet, parsed.String(), nil)
	if err != nil {
		return Page{}, err
	}
	// A real, honest user agent. Not a disguise: this process identifies itself, and a site that
	// does not want to be read by it is entitled to say so.
	request.Header.Set("User-Agent", UserAgent)
	request.Header.Set("Accept", "text/html,application/xhtml+xml,text/plain;q=0.9")
	request.Header.Set("Accept-Language", "en,pt;q=0.8")

	response, err := c.http.Do(request)
	if err != nil {
		return Page{}, fmt.Errorf("fetch: %w", err)
	}
	defer response.Body.Close()

	if response.StatusCode != http.StatusOK {
		return Page{}, fmt.Errorf("fetch: %s returned %d", parsed.Host, response.StatusCode)
	}

	contentType := response.Header.Get("Content-Type")
	if !isReadable(contentType) {
		return Page{}, fmt.Errorf("%w: %q", ErrNotHTML, contentType)
	}

	// One byte over the ceiling is read on purpose: reading exactly the limit cannot distinguish a
	// page that is exactly at the limit from one that was cut off there.
	body, err := io.ReadAll(io.LimitReader(response.Body, c.maxBytes+1))
	if err != nil {
		return Page{}, fmt.Errorf("fetch: reading body: %w", err)
	}
	if int64(len(body)) > c.maxBytes {
		return Page{}, fmt.Errorf("%w: over %d bytes", ErrTooLarge, c.maxBytes)
	}

	return Page{
		FinalURL:     response.Request.URL.String(),
		RequestedURL: rawURL,
		ContentType:  contentType,
		Body:         string(body),
	}, nil
}

// UserAgent identifies this process to the sites it reads.
const UserAgent = "NucleOS/1.0 (+https://github.com/nucleos; personal assistant; one user)"

// isReadable decides from the Content-Type header alone, before the body is read.
func isReadable(contentType string) bool {
	media := strings.TrimSpace(strings.ToLower(contentType))
	if index := strings.IndexByte(media, ';'); index >= 0 {
		media = strings.TrimSpace(media[:index])
	}
	switch media {
	case "text/html", "application/xhtml+xml", "text/plain", "application/xml", "text/xml":
		return true
	case "":
		// A server that says nothing is not a server that said HTML. Guessing here would mean
		// handing arbitrary bytes to an HTML parser on the strength of an absent header.
		return false
	default:
		return false
	}
}
