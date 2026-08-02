package serve

import (
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"
	"time"

	"nucleosweb/fetch"
	"nucleosweb/search"
)

const token = "the-daemon-token"

func handler(provider search.Provider) http.Handler {
	mux := http.NewServeMux()
	mux.HandleFunc("/search", authorized(token, searchHandler(provider)))
	mux.HandleFunc("/fetch", authorized(token, fetchHandler(fetch.New(2*time.Second, 1<<20))))
	return mux
}

func post(t *testing.T, provider search.Provider, path, body, bearer string) *httptest.ResponseRecorder {
	t.Helper()
	request := httptest.NewRequest(http.MethodPost, path, strings.NewReader(body))
	if bearer != "" {
		request.Header.Set("Authorization", "Bearer "+bearer)
	}
	recorder := httptest.NewRecorder()
	handler(provider).ServeHTTP(recorder, request)
	return recorder
}

// The token is the only thing between this process and anything else on the machine that can open a
// socket. Every near-miss shape, because a prefix comparison passes several of them.
func TestEveryRouteRefusesAWrongToken(t *testing.T) {
	for _, path := range []string{"/search", "/fetch"} {
		for _, bearer := range []string{"", "wrong", "the-daemon", "the-daemon-tokens"} {
			got := post(t, &search.Fake{}, path, `{"query":"x","url":"https://example.com"}`, bearer).Code
			if got != http.StatusUnauthorized {
				t.Errorf("%s with bearer %q returned %d, want 401", path, bearer, got)
			}
		}
	}
}

func TestSearchReturnsDestinationsAndNoContent(t *testing.T) {
	provider := &search.Fake{Results: []search.Result{
		{Title: "Tokio", URL: "https://docs.rs/tokio", Snippet: "An async runtime"},
	}}

	recorder := post(t, provider, "/search", `{"query":"tokio","limit":3}`, token)
	if recorder.Code != http.StatusOK {
		t.Fatalf("search returned %d: %s", recorder.Code, recorder.Body.String())
	}

	var response SearchResponse
	if err := json.Unmarshal(recorder.Body.Bytes(), &response); err != nil {
		t.Fatalf("unreadable response: %v", err)
	}
	if len(response.Results) != 1 || response.Results[0].URL != "https://docs.rs/tokio" {
		t.Fatalf("results = %+v", response.Results)
	}
	// The contract of spec §3.2: a search hands back destinations, never content. If a field ever
	// carries page text, the trust decision stops happening before the fetch.
	if strings.Contains(recorder.Body.String(), "markdown") {
		t.Error("a search response carried content — trust can no longer be decided before fetching")
	}
}

func TestSearchRefusesAnEmptyQuery(t *testing.T) {
	for _, body := range []string{`{"query":""}`, `{"query":"   "}`, `{}`} {
		if got := post(t, &search.Fake{}, "/search", body, token).Code; got != http.StatusBadRequest {
			t.Errorf("body %s returned %d, want 400", body, got)
		}
	}
}

// Not configured is not broken. 503 keeps the daemon's health readout able to tell an owner to go
// and set a key, rather than to go and read a log.
func TestSearchReportsAnUnconfiguredProviderSeparatelyFromAFailure(t *testing.T) {
	unavailable := search.Unavailable{}
	if got := post(t, unavailable, "/search", `{"query":"x"}`, token).Code; got != http.StatusServiceUnavailable {
		t.Errorf("an unconfigured provider returned %d, want 503", got)
	}

	broken := &search.Fake{Err: http.ErrHandlerTimeout}
	if got := post(t, broken, "/search", `{"query":"x"}`, token).Code; got != http.StatusBadGateway {
		t.Errorf("a failing provider returned %d, want 502", got)
	}
}

// The seam of spec §3.5. When this stops being 501, the threat model section has to be rewritten
// BEFORE, not after — so a change here is meant to make somebody read that sentence.
func TestRenderIsNotImplemented(t *testing.T) {
	recorder := post(t, &search.Fake{}, "/fetch", `{"url":"https://example.com","render":true}`, token)
	if recorder.Code != http.StatusNotImplemented {
		t.Fatalf("render:true returned %d, want 501", recorder.Code)
	}
}

// The most security-relevant path through this handler: a destination the guard refuses must come
// back as a policy refusal, not as a gateway error a caller would sensibly retry.
func TestABlockedDestinationIsForbiddenAndNotAGatewayError(t *testing.T) {
	for _, url := range []string{
		`http://127.0.0.1:8791/kill`,
		`http://[::1]:8791/kill`,
		`file:///c:/windows/win.ini`,
		`http://169.254.169.254/latest/meta-data/`,
	} {
		recorder := post(t, &search.Fake{}, "/fetch", `{"url":"`+url+`"}`, token)
		if recorder.Code != http.StatusForbidden {
			t.Errorf("fetching %s returned %d, want 403", url, recorder.Code)
		}
	}
}

func TestRoutesRefuseNonPostMethods(t *testing.T) {
	for _, path := range []string{"/search", "/fetch"} {
		request := httptest.NewRequest(http.MethodGet, path, nil)
		request.Header.Set("Authorization", "Bearer "+token)
		recorder := httptest.NewRecorder()
		handler(&search.Fake{}).ServeHTTP(recorder, request)
		if recorder.Code != http.StatusMethodNotAllowed {
			t.Errorf("GET %s returned %d, want 405", path, recorder.Code)
		}
	}
}
