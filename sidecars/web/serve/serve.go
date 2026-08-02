// Package serve is this sidecar's only inbound surface.
//
// It binds to loopback, requires the daemon's token, and writes nothing anywhere. The núcleo asks
// for a search or for a page; this process answers and forgets. No cursor, no cache, no state — all
// of that lives in the database, which this process has never been able to open (spec §4, §5).
package serve

import (
	"crypto/subtle"
	"encoding/json"
	"errors"
	"log"
	"net/http"
	"strings"
	"time"

	"nucleosweb/config"
	"nucleosweb/extract"
	"nucleosweb/fetch"
	"nucleosweb/safe"
	"nucleosweb/search"
)

// HeaderTimeout bounds how long a client may take to send its headers. Small, because the only
// legitimate client is on the same machine.
const HeaderTimeout = 10 * time.Second

// Serve blocks, answering the núcleo until the process ends.
func Serve(cfg config.Config, provider search.Provider, fetcher *fetch.Client) error {
	mux := http.NewServeMux()
	mux.HandleFunc("/search", authorized(cfg.DaemonToken, searchHandler(provider)))
	mux.HandleFunc("/fetch", authorized(cfg.DaemonToken, fetchHandler(fetcher)))

	server := &http.Server{
		Addr:              cfg.Addr,
		Handler:           mux,
		ReadHeaderTimeout: HeaderTimeout,
	}
	log.Printf("serving web on %s with provider %s", cfg.Addr, provider.Name())
	return server.ListenAndServe()
}

// SearchRequest is what the núcleo posts to /search.
type SearchRequest struct {
	Query string `json:"query"`
	Limit int    `json:"limit"`
}

// SearchResponse carries destinations and nothing else — no content, because content must not be
// fetched before the núcleo has decided the trust of the URL it came from (spec §3.2).
type SearchResponse struct {
	Provider string          `json:"provider"`
	Results  []search.Result `json:"results"`
}

// FetchRequest is what the núcleo posts to /fetch.
type FetchRequest struct {
	URL string `json:"url"`
	// Render asks for a real browser. It is the seam of spec §3.5 and answers 501 in v1: one field,
	// so the CDP path has somewhere to land without any caller changing.
	Render bool `json:"render"`
}

// FetchResponse is one extracted page.
type FetchResponse struct {
	RequestedURL string         `json:"requested_url"`
	FinalURL     string         `json:"final_url"`
	Title        string         `json:"title"`
	Byline       string         `json:"byline"`
	Markdown     string         `json:"markdown"`
	Status       extract.Status `json:"status"`
	Bytes        int            `json:"bytes"`
}

func searchHandler(provider search.Provider) http.HandlerFunc {
	return func(w http.ResponseWriter, r *http.Request) {
		if r.Method != http.MethodPost {
			http.Error(w, "method not allowed", http.StatusMethodNotAllowed)
			return
		}
		var request SearchRequest
		if err := json.NewDecoder(http.MaxBytesReader(w, r.Body, 64<<10)).Decode(&request); err != nil {
			http.Error(w, "bad request body", http.StatusBadRequest)
			return
		}
		if strings.TrimSpace(request.Query) == "" {
			http.Error(w, "query is required", http.StatusBadRequest)
			return
		}

		results, err := provider.Search(r.Context(), request.Query, request.Limit)
		if err != nil {
			if errors.Is(err, search.ErrNotConfigured) {
				// 503 and not 500: nothing is broken, something was never set up, and the daemon's
				// health readout tells those apart.
				http.Error(w, err.Error(), http.StatusServiceUnavailable)
				return
			}
			log.Printf("search failed: %v", err)
			http.Error(w, "search failed", http.StatusBadGateway)
			return
		}

		writeJSON(w, SearchResponse{Provider: provider.Name(), Results: results})
	}
}

func fetchHandler(fetcher *fetch.Client) http.HandlerFunc {
	return func(w http.ResponseWriter, r *http.Request) {
		if r.Method != http.MethodPost {
			http.Error(w, "method not allowed", http.StatusMethodNotAllowed)
			return
		}
		var request FetchRequest
		if err := json.NewDecoder(http.MaxBytesReader(w, r.Body, 64<<10)).Decode(&request); err != nil {
			http.Error(w, "bad request body", http.StatusBadRequest)
			return
		}
		if request.Render {
			http.Error(
				w,
				"render is not implemented: v1 reads pages over HTTP without a browser (spec §3.5)",
				http.StatusNotImplemented,
			)
			return
		}

		page, err := fetcher.Fetch(r.Context(), request.URL)
		if err != nil {
			status := http.StatusBadGateway
			switch {
			case errors.Is(err, safe.ErrBlocked):
				// 403 and not 502: the destination was refused by policy, and a caller that saw a
				// gateway error would reasonably retry a URL that must never be tried again.
				status = http.StatusForbidden
			case errors.Is(err, fetch.ErrTooLarge), errors.Is(err, fetch.ErrNotHTML):
				status = http.StatusUnprocessableEntity
			}
			http.Error(w, err.Error(), status)
			return
		}

		article, err := extract.Extract(page.Body)
		if err != nil {
			if errors.Is(err, extract.ErrEmpty) {
				// The fetch worked and there was nothing readable in it. 422, so the núcleo does not
				// cache this as a successful read — and so the message can suggest the one thing
				// that would actually help.
				http.Error(
					w,
					"no readable content: the page may need a browser to render (spec §3.5)",
					http.StatusUnprocessableEntity,
				)
				return
			}
			log.Printf("extract failed for %s: %v", page.FinalURL, err)
			http.Error(w, "extraction failed", http.StatusUnprocessableEntity)
			return
		}

		writeJSON(w, FetchResponse{
			RequestedURL: page.RequestedURL,
			FinalURL:     page.FinalURL,
			Title:        article.Title,
			Byline:       article.Byline,
			Markdown:     article.Markdown,
			Status:       article.Status,
			Bytes:        len(page.Body),
		})
	}
}

// authorized wraps a handler with the bearer check. Constant-time, like the email sidecar's.
func authorized(token string, next http.HandlerFunc) http.HandlerFunc {
	return func(w http.ResponseWriter, r *http.Request) {
		if !hasToken(r, token) {
			http.Error(w, "unauthorized", http.StatusUnauthorized)
			return
		}
		next(w, r)
	}
}

func hasToken(r *http.Request, token string) bool {
	const prefix = "Bearer "
	header := r.Header.Get("Authorization")
	if !strings.HasPrefix(header, prefix) {
		return false
	}
	presented := strings.TrimPrefix(header, prefix)
	return subtle.ConstantTimeCompare([]byte(presented), []byte(token)) == 1
}

func writeJSON(w http.ResponseWriter, payload any) {
	w.Header().Set("Content-Type", "application/json")
	if err := json.NewEncoder(w).Encode(payload); err != nil {
		log.Printf("writing response: %v", err)
	}
}
