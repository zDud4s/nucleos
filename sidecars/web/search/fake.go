package search

import "context"

// Fake is the provider tests use. It exists in the shipped binary rather than behind a build tag
// because `serve` is tested against a real HTTP server, and a search that needs the internet to be
// tested is a search that is not tested.
type Fake struct {
	Results []Result
	Err     error
	// Queries records what was asked, in order. A test that asserts the limit was clamped needs to
	// see what the provider actually received, not what the caller believed it sent.
	Queries []string
	Limits  []int
}

func (f *Fake) Name() string { return "fake" }

func (f *Fake) Search(_ context.Context, query string, limit int) ([]Result, error) {
	f.Queries = append(f.Queries, query)
	f.Limits = append(f.Limits, ClampLimit(limit))
	if f.Err != nil {
		return nil, f.Err
	}
	capped := ClampLimit(limit)
	if len(f.Results) > capped {
		return f.Results[:capped], nil
	}
	return f.Results, nil
}

// Unavailable stands in when a provider was selected but could not be built — no API key, no
// SearXNG URL, an unknown name.
//
// It is a provider rather than a nil check at every call site because the alternative is a nil
// interface travelling through `serve`, and a nil that has to be remembered at each use is a
// panic waiting for the one path nobody tested. This one answers honestly, every time.
type Unavailable struct {
	Reason error
}

func (u Unavailable) Name() string { return "unavailable" }

func (u Unavailable) Search(context.Context, string, int) ([]Result, error) {
	if u.Reason != nil {
		return nil, u.Reason
	}
	return nil, ErrNotConfigured
}
