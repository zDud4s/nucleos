package search

import (
	"context"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"net/url"
	"strconv"
)

// BraveEndpoint is the web-search resource. Fixed rather than configurable: a configurable search
// endpoint is a configurable place for the owner's queries to go.
const BraveEndpoint = "https://api.search.brave.com/res/v1/web/search"

// braveBodyLimit bounds what is read from the response. A search answer is a few kilobytes; a
// megabyte means something is wrong upstream and reading it all would be the wrong reaction.
const braveBodyLimit = 1 << 20

// Brave is the shipped provider. Chosen for three properties, none of which is price: an index of
// its own rather than Google or Bing resold, no logging of API queries, and a free tier that covers
// a single-person installation without a card on file.
type Brave struct {
	key    string
	client *http.Client
}

func NewBrave(key string, client *http.Client) (*Brave, error) {
	if key == "" {
		return nil, fmt.Errorf("%w: brave needs an API key", ErrNotConfigured)
	}
	return &Brave{key: key, client: client}, nil
}

func (b *Brave) Name() string { return "brave" }

func (b *Brave) Search(ctx context.Context, query string, limit int) ([]Result, error) {
	if query == "" {
		return nil, fmt.Errorf("empty query")
	}
	params := url.Values{}
	params.Set("q", query)
	params.Set("count", strconv.Itoa(ClampLimit(limit)))

	request, err := http.NewRequestWithContext(ctx, http.MethodGet, BraveEndpoint+"?"+params.Encode(), nil)
	if err != nil {
		return nil, err
	}
	request.Header.Set("Accept", "application/json")
	request.Header.Set("X-Subscription-Token", b.key)

	response, err := b.client.Do(request)
	if err != nil {
		return nil, fmt.Errorf("brave: %w", err)
	}
	defer response.Body.Close()

	if response.StatusCode != http.StatusOK {
		// The body is not included in the error. A provider error page is text this process did not
		// write, and every error string here ends up somewhere a person or a model reads.
		return nil, fmt.Errorf("brave: search returned %d", response.StatusCode)
	}

	var payload struct {
		Web struct {
			Results []struct {
				Title       string `json:"title"`
				URL         string `json:"url"`
				Description string `json:"description"`
			} `json:"results"`
		} `json:"web"`
	}
	if err := json.NewDecoder(io.LimitReader(response.Body, braveBodyLimit)).Decode(&payload); err != nil {
		return nil, fmt.Errorf("brave: unreadable response: %w", err)
	}

	results := make([]Result, 0, len(payload.Web.Results))
	for _, raw := range payload.Web.Results {
		if raw.URL == "" {
			continue
		}
		results = append(results, Result{Title: raw.Title, URL: raw.URL, Snippet: raw.Description})
	}
	return results, nil
}
