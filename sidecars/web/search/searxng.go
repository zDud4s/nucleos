// §spec pilar-de-web

package search

import (
	"context"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"net/url"
	"strings"
)

const searxngBodyLimit = 1 << 20

// Searxng is the no-API-key provider: a metasearch aggregator the owner runs themselves, very often
// on loopback. It is not the default for two reasons written down in the spec §3.3 — it is another
// process to supervise on Windows, against `sidecars/AGENTS.md`'s static-binary rule, and it breaks
// on its own when an upstream engine changes its HTML or rate-limits the house IP.
type Searxng struct {
	base   string
	client *http.Client
}

func NewSearxng(base string, client *http.Client) (*Searxng, error) {
	if base == "" {
		return nil, fmt.Errorf("%w: searxng needs a base URL", ErrNotConfigured)
	}
	return &Searxng{base: strings.TrimRight(base, "/"), client: client}, nil
}

func (s *Searxng) Name() string { return "searxng" }

func (s *Searxng) Search(ctx context.Context, query string, limit int) ([]Result, error) {
	if query == "" {
		return nil, fmt.Errorf("empty query")
	}
	params := url.Values{}
	params.Set("q", query)
	params.Set("format", "json")

	request, err := http.NewRequestWithContext(ctx, http.MethodGet, s.base+"/search?"+params.Encode(), nil)
	if err != nil {
		return nil, err
	}
	request.Header.Set("Accept", "application/json")

	response, err := s.client.Do(request)
	if err != nil {
		return nil, fmt.Errorf("searxng: %w", err)
	}
	defer response.Body.Close()

	if response.StatusCode != http.StatusOK {
		// A stock SearXNG answers 403 to `format=json` until `search.formats` lists it, and that is
		// the single most likely thing to be wrong with a fresh install. Naming it here saves the
		// owner an afternoon.
		if response.StatusCode == http.StatusForbidden {
			return nil, fmt.Errorf(
				"searxng: returned 403 — add \"json\" to search.formats in its settings.yml",
			)
		}
		return nil, fmt.Errorf("searxng: search returned %d", response.StatusCode)
	}

	var payload struct {
		Results []struct {
			Title   string `json:"title"`
			URL     string `json:"url"`
			Content string `json:"content"`
		} `json:"results"`
	}
	if err := json.NewDecoder(io.LimitReader(response.Body, searxngBodyLimit)).Decode(&payload); err != nil {
		return nil, fmt.Errorf("searxng: unreadable response: %w", err)
	}

	capped := ClampLimit(limit)
	results := make([]Result, 0, capped)
	for _, raw := range payload.Results {
		if raw.URL == "" {
			continue
		}
		if len(results) == capped {
			// SearXNG has no count parameter — it returns a page of whatever the engines gave it, so
			// the limit is applied here or not at all.
			break
		}
		results = append(results, Result{Title: raw.Title, URL: raw.URL, Snippet: raw.Content})
	}
	return results, nil
}
