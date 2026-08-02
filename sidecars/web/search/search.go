// Package search turns a query into a list of destinations, without fetching any of them.
//
// The whole pillar is built on this separation (spec §3.2): a search returns {title, url, snippet}
// — structured data, no DOM, no JavaScript — and the núcleo decides the trust of each URL BEFORE
// anything is fetched. A provider that navigated to a results page instead would hand back
// attacker-shaped HTML and would make the trust decision arrive too late to mean anything.
//
// # Two clients, on purpose
//
// This package dials destinations chosen by the OWNER's configuration: an API endpoint, or a
// self-hosted SearXNG that is very often on loopback or the home LAN. `fetch` dials destinations
// chosen by whoever asked — an agent, a search result, a redirect.
//
// Those are different trust classes and they therefore get different HTTP clients. This one has no
// SSRF guard, because pointing it at 127.0.0.1:8888 is a supported configuration; `fetch`'s has one
// it can never be built without. Merging them "to remove duplication" would either break
// self-hosting or silently turn `web_read` into a proxy for the home network. If a future change
// makes them look mergeable, read this paragraph again.
package search

import (
	"context"
	"errors"
	"fmt"
	"net/http"
	"time"
)

// Result is one destination. Deliberately three fields: anything richer would be content, and
// content belongs to `fetch`, downstream of a trust decision this type exists to make possible.
type Result struct {
	Title   string `json:"title"`
	URL     string `json:"url"`
	Snippet string `json:"snippet"`
}

// Provider is the seam the spec's §3.3 names. Brave is what ships; SearXNG is for an installation
// that wants no API key; Fake is for tests. When the browser arrives, "search by driving it" is a
// fourth implementation of this interface rather than a rewrite of everything above it.
type Provider interface {
	// Search returns at most `limit` results. An empty slice is a valid answer and not an error:
	// "nothing found" is information, and turning it into a failure would make the caller retry.
	Search(ctx context.Context, query string, limit int) ([]Result, error)
	// Name identifies the provider in logs and in the daemon's health readout.
	Name() string
}

// ErrNotConfigured is returned when a provider was selected but cannot run — no API key, no
// SearXNG URL. It is distinct from a network failure on purpose: one is a setup problem the owner
// can fix, the other is a bad minute on the internet, and a health readout that conflated them
// would send someone to check their wiring over a timeout.
var ErrNotConfigured = errors.New("search provider is not configured")

// MaxLimit caps what any caller can ask for. A search is an input to a context window, not a
// crawl; asking for 200 results wastes the provider's quota and the model's attention alike.
const MaxLimit = 20

// DefaultLimit is what a caller that expresses no preference gets.
const DefaultLimit = 8

// ClampLimit brings a caller's request into range. Zero and negative mean "no preference" rather
// than "no results" — a provider asked for zero results would answer nothing, which reads as a
// broken search rather than an unset parameter.
func ClampLimit(limit int) int {
	if limit <= 0 {
		return DefaultLimit
	}
	if limit > MaxLimit {
		return MaxLimit
	}
	return limit
}

// NewClient builds the HTTP client this package uses. See the package comment for why it has no
// SSRF guard and `fetch`'s client does.
func NewClient(timeout time.Duration) *http.Client {
	return &http.Client{Timeout: timeout}
}

// Select builds the configured provider. An unknown name is an error rather than a fallback to the
// default: silently searching somewhere other than where the owner wrote would be a privacy
// surprise, and the query is the thing leaving the machine.
func Select(name, braveKey, searxngURL string, client *http.Client) (Provider, error) {
	switch name {
	case "brave":
		return NewBrave(braveKey, client)
	case "searxng":
		return NewSearxng(searxngURL, client)
	default:
		return nil, fmt.Errorf("unknown search provider %q: expected \"brave\" or \"searxng\"", name)
	}
}
