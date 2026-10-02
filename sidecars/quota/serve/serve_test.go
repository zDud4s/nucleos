package serve

import (
	"context"
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"testing"
	"time"

	"nucleosquota/config"
	"nucleosquota/reading"
)

// stubReaders answers with fixed providers, counting how many times each was actually called so a
// test can tell a fresh read from a cache hit without a network or a home directory.
func stubReaders(claudeCalls, codexCalls *int, claudeReading, codexReading reading.Provider) Readers {
	return Readers{
		Claude: func(ctx context.Context, now time.Time) reading.Provider {
			*claudeCalls++
			return claudeReading
		},
		Codex: func(now time.Time) reading.Provider {
			*codexCalls++
			return codexReading
		},
	}
}

func official(name string, at time.Time) reading.Provider {
	return reading.Provider{Name: name, Fidelity: reading.Official, ReadAt: at}
}

func doGet(t *testing.T, handler http.HandlerFunc, token string) (*httptest.ResponseRecorder, Response) {
	t.Helper()
	req := httptest.NewRequest(http.MethodGet, "/quota", nil)
	if token != "" {
		req.Header.Set("Authorization", "Bearer "+token)
	}
	rec := httptest.NewRecorder()
	handler(rec, req)

	var body Response
	if rec.Code == http.StatusOK {
		if err := json.Unmarshal(rec.Body.Bytes(), &body); err != nil {
			t.Fatalf("decoding response: %v (body: %s)", err, rec.Body.String())
		}
	}
	return rec, body
}

// A request with no bearer, or the wrong one, must never reach the readers — this process holds
// the owner's usage figures and a route to the vendor, and the daemon's token is the only gate.
func a_missing_or_wrong_bearer_gets_401(t *testing.T) {
	var claudeCalls, codexCalls int
	readers := stubReaders(&claudeCalls, &codexCalls, official("claude", time.Now()), official("codex", time.Now()))
	handler := authorized("the-real-token", quotaHandler(config.Config{SuccessTTL: time.Minute, ErrorTTL: time.Second}, readers, &cache{}, time.Now))

	cases := []struct {
		name  string
		token string
	}{
		{"no Authorization header at all", ""},
		{"the wrong token", "not-the-real-token"},
		{"a bearer that is only whitespace", " "},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			rec, _ := doGet(t, handler, tc.token)
			if rec.Code != http.StatusUnauthorized {
				t.Errorf("status = %d, want %d", rec.Code, http.StatusUnauthorized)
			}
		})
	}
	if claudeCalls != 0 || codexCalls != 0 {
		t.Errorf("an unauthorized request reached the readers: claude=%d codex=%d", claudeCalls, codexCalls)
	}
}

// The one bearer that matters gets through, and gets the providers back.
func a_correct_bearer_gets_200(t *testing.T) {
	var claudeCalls, codexCalls int
	readers := stubReaders(&claudeCalls, &codexCalls, official("claude", time.Now()), official("codex", time.Now()))
	handler := authorized("the-real-token", quotaHandler(config.Config{SuccessTTL: time.Minute, ErrorTTL: time.Second}, readers, &cache{}, time.Now))

	rec, body := doGet(t, handler, "the-real-token")

	if rec.Code != http.StatusOK {
		t.Fatalf("status = %d, want %d", rec.Code, http.StatusOK)
	}
	if len(body.Providers) != 2 {
		t.Errorf("providers = %d, want 2", len(body.Providers))
	}
	if body.Cached {
		t.Error("a first call must not report cached")
	}
}

// The cache exists to protect the vendor's endpoint from the núcleo's own polling cadence: a second
// call inside the TTL must be answered from the held reading, not a fresh one.
func a_second_call_within_ttl_returns_cached(t *testing.T) {
	var claudeCalls, codexCalls int
	readers := stubReaders(&claudeCalls, &codexCalls, official("claude", time.Now()), official("codex", time.Now()))
	cfg := config.Config{SuccessTTL: time.Minute, ErrorTTL: time.Second}
	c := &cache{}
	fixedNow := time.Unix(1_800_000_000, 0)
	handler := quotaHandler(cfg, readers, c, func() time.Time { return fixedNow })

	_, first := doGet(t, handler, "")
	_, second := doGet(t, handler, "")

	if first.Cached {
		t.Error("the first call must not be cached")
	}
	if !second.Cached {
		t.Error("the second call, inside the TTL, must be cached")
	}
	if claudeCalls != 1 || codexCalls != 1 {
		t.Errorf("expected exactly one upstream read each, got claude=%d codex=%d", claudeCalls, codexCalls)
	}
}

// A provider that is down should be retried on the shorter error cadence even while a sibling
// provider is happily within its success TTL: holding an Unmeasured reading for a full success
// window is how a transient failure becomes minutes of dashed rings that could have recovered.
func an_unmeasured_reading_uses_the_shorter_error_ttl(t *testing.T) {
	base := time.Unix(1_800_000_000, 0)
	current := base
	cfg := config.Config{SuccessTTL: time.Hour, ErrorTTL: 5 * time.Second}
	var claudeCalls, codexCalls int
	readers := stubReaders(
		&claudeCalls, &codexCalls,
		reading.Unavailable("claude", "no usable credential", base),
		official("codex", base),
	)
	c := &cache{}
	handler := quotaHandler(cfg, readers, c, func() time.Time { return current })

	_, first := doGet(t, handler, "")
	if first.Cached {
		t.Fatal("the first call must not be cached")
	}

	// Past the 5s error TTL but nowhere near the 1h success TTL: only the shorter TTL explains a
	// miss here.
	current = base.Add(6 * time.Second)
	_, second := doGet(t, handler, "")
	if second.Cached {
		t.Error("an unmeasured reading held past its error TTL was served from cache")
	}
	if claudeCalls != 2 {
		t.Errorf("claude reads = %d, want 2 — the error TTL should have expired the cache", claudeCalls)
	}

	// Immediately after that second read, still well inside its own fresh 5s window.
	current = current.Add(1 * time.Second)
	_, third := doGet(t, handler, "")
	if !third.Cached {
		t.Error("a call inside the error TTL was not served from cache")
	}
}

// An unreadable provider must go out as `"windows": []`, never `null`.
//
// reading.Unavailable leaves the slice nil, and encoding/json writes a nil slice as null. The núcleo
// reads that field as a list, and a null there failed its WHOLE decode — so one rate-limited
// provider turned the other provider's live figures into last-known ones (captured 2026-09-24:
// every fresh read "error decoding response body" while the usage endpoint answered 429). The wire
// is the one place every reader's answer passes through, so it is where a nil becomes empty.
func an_unreadable_provider_goes_out_with_an_empty_window_list(t *testing.T) {
	var claudeCalls, codexCalls int
	at := time.Now()
	readers := stubReaders(&claudeCalls, &codexCalls,
		reading.Unavailable("claude", "the usage endpoint answered 429", at), official("codex", at))
	handler := authorized("tok", quotaHandler(config.Config{SuccessTTL: time.Minute, ErrorTTL: time.Second}, readers, &cache{}, time.Now))

	rec, _ := doGet(t, handler, "tok")

	var raw struct {
		Providers []map[string]json.RawMessage `json:"providers"`
	}
	if err := json.Unmarshal(rec.Body.Bytes(), &raw); err != nil {
		t.Fatalf("decoding response: %v (body: %s)", err, rec.Body.String())
	}
	for _, provider := range raw.Providers {
		if got := string(provider["windows"]); got != "[]" {
			t.Errorf("provider %s: windows = %s, want [] (body: %s)", provider["provider"], got, rec.Body.String())
		}
	}
}

func TestServe(t *testing.T) {
	t.Run("a missing or wrong bearer gets 401", a_missing_or_wrong_bearer_gets_401)
	t.Run("a correct bearer gets 200", a_correct_bearer_gets_200)
	t.Run("a second call within TTL returns cached", a_second_call_within_ttl_returns_cached)
	t.Run("an unmeasured reading uses the shorter error TTL", an_unmeasured_reading_uses_the_shorter_error_ttl)
	t.Run("an unreadable provider goes out with an empty window list", an_unreadable_provider_goes_out_with_an_empty_window_list)
}
