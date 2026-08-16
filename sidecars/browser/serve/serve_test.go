package serve

import (
	"bytes"
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"testing"

	"nucleosbrowser/browser"
	"nucleosbrowser/config"
)

const token = "test-token"

func testServer(t *testing.T, driver browser.Driver) *httptest.Server {
	t.Helper()
	mux := http.NewServeMux()
	mux.HandleFunc("/open", authorized(token, openHandler(driver)))
	mux.HandleFunc("/snapshot", authorized(token, snapshotHandler(driver)))
	mux.HandleFunc("/act", authorized(token, actHandler(driver)))
	mux.HandleFunc("/screenshot", authorized(token, screenshotHandler(driver)))
	mux.HandleFunc("/handoff", authorized(token, handoffHandler(driver)))
	mux.HandleFunc("/close", authorized(token, closeHandler(driver)))
	server := httptest.NewServer(mux)
	t.Cleanup(server.Close)
	return server
}

func post(t *testing.T, server *httptest.Server, path string, body any, auth bool) *http.Response {
	t.Helper()
	encoded, err := json.Marshal(body)
	if err != nil {
		t.Fatalf("marshal: %v", err)
	}
	request, err := http.NewRequest(http.MethodPost, server.URL+path, bytes.NewReader(encoded))
	if err != nil {
		t.Fatalf("request: %v", err)
	}
	if auth {
		request.Header.Set("Authorization", "Bearer "+token)
	}
	response, err := http.DefaultClient.Do(request)
	if err != nil {
		t.Fatalf("do: %v", err)
	}
	t.Cleanup(func() { response.Body.Close() })
	return response
}

// TestEveryRouteNeedsTheToken. This process drives browsers holding the owner's logged-in sessions;
// a route that forgot the check would hand them to any local process.
func TestEveryRouteNeedsTheToken(t *testing.T) {
	server := testServer(t, &browser.Fake{FenceAttached: true})
	for _, path := range []string{"/open", "/snapshot", "/act", "/screenshot", "/handoff", "/close"} {
		t.Run(path, func(t *testing.T) {
			response := post(t, server, path, map[string]string{"url": "https://example.org/"}, false)
			if response.StatusCode != http.StatusUnauthorized {
				t.Fatalf("without a token: got %d, want 401", response.StatusCode)
			}
		})
	}
}

// TestFenceNotAttachedIs503 pins the wire half of spec §6.2a. It must not be a 500: a crash invites
// a retry loop, and the thing being retried would be browsing without a fence.
func TestFenceNotAttachedIs503(t *testing.T) {
	server := testServer(t, &browser.Fake{}) // zero value: no fence
	response := post(t, server, "/open", OpenRequest{URL: "https://example.org/"}, true)
	if response.StatusCode != http.StatusServiceUnavailable {
		t.Fatalf("got %d, want 503", response.StatusCode)
	}
}

// TestRefusalIsTwoHundred is the contract property this package exists to protect. A fenced action
// is an ANSWER: 200, with the consequence named in the body.
func TestRefusalIsTwoHundred(t *testing.T) {
	driver := &browser.Fake{
		FenceAttached: true,
		Refuse:        &browser.Refusal{Consequence: browser.ConsequenceMethod, Detail: "POST /orders"},
	}
	server := testServer(t, driver)

	var session browser.Session
	response := post(t, server, "/open", OpenRequest{URL: "https://example.org/"}, true)
	if response.StatusCode != http.StatusOK {
		t.Fatalf("open: got %d", response.StatusCode)
	}
	if err := json.NewDecoder(response.Body).Decode(&session); err != nil {
		t.Fatalf("decode session: %v", err)
	}

	response = post(t, server, "/act", ActRequest{
		SessionID: string(session.ID),
		Kind:      "click",
		Ref:       "e1",
	}, true)
	if response.StatusCode != http.StatusOK {
		t.Fatalf("a refusal must be 200, got %d", response.StatusCode)
	}
	var result browser.ActResult
	if err := json.NewDecoder(response.Body).Decode(&result); err != nil {
		t.Fatalf("decode result: %v", err)
	}
	if result.Outcome != browser.OutcomeRefused {
		t.Fatalf("outcome: got %q", result.Outcome)
	}
	if result.Refusal == nil || result.Refusal.Consequence != browser.ConsequenceMethod {
		t.Fatalf("the consequence must reach the caller, got %+v", result.Refusal)
	}
}

// TestUnknownActionKindIsRefusedAtTheDoor. The vocabulary is closed (spec §6.2, consequence-free in
// v1); an unknown verb must not reach a driver that might interpret it generously.
func TestUnknownActionKindIsRefusedAtTheDoor(t *testing.T) {
	driver := &browser.Fake{FenceAttached: true}
	server := testServer(t, driver)

	var session browser.Session
	response := post(t, server, "/open", OpenRequest{URL: "https://example.org/"}, true)
	json.NewDecoder(response.Body).Decode(&session)

	response = post(t, server, "/act", ActRequest{
		SessionID: string(session.ID),
		Kind:      "submit",
		Ref:       "e1",
	}, true)
	if response.StatusCode != http.StatusBadRequest {
		t.Fatalf("got %d, want 400", response.StatusCode)
	}
	if len(driver.Actions) != 0 {
		t.Fatalf("the driver must never see it, got %+v", driver.Actions)
	}
}

func TestUnknownSessionIs404(t *testing.T) {
	server := testServer(t, &browser.Fake{FenceAttached: true})
	response := post(t, server, "/snapshot", SessionRequest{SessionID: "nope"}, true)
	if response.StatusCode != http.StatusNotFound {
		t.Fatalf("got %d, want 404", response.StatusCode)
	}
}

// TestOpenWireCarriesNoProfile mirrors the reflection guard in the browser package, one layer out:
// the wire type must not grow a way to choose the identity either.
func TestOpenWireCarriesNoProfile(t *testing.T) {
	driver := &browser.Fake{FenceAttached: true}
	server := testServer(t, driver)

	// A caller that tries anyway gets its extra field ignored, and the driver sees only the url.
	response := post(t, server, "/open", map[string]any{
		"url":     "https://example.org/",
		"profile": "project-42",
	}, true)
	if response.StatusCode != http.StatusOK {
		t.Fatalf("open: got %d", response.StatusCode)
	}
	if len(driver.Opened) != 1 {
		t.Fatalf("driver saw %d opens", len(driver.Opened))
	}
	if driver.Opened[0].URL != "https://example.org/" {
		t.Fatalf("url: got %q", driver.Opened[0].URL)
	}
}

func TestGetIsNotAllowed(t *testing.T) {
	server := testServer(t, &browser.Fake{FenceAttached: true})
	request, _ := http.NewRequest(http.MethodGet, server.URL+"/open", nil)
	request.Header.Set("Authorization", "Bearer "+token)
	response, err := http.DefaultClient.Do(request)
	if err != nil {
		t.Fatalf("do: %v", err)
	}
	defer response.Body.Close()
	if response.StatusCode != http.StatusMethodNotAllowed {
		t.Fatalf("got %d, want 405", response.StatusCode)
	}
}

func TestCloseThenSnapshotIsGone(t *testing.T) {
	server := testServer(t, &browser.Fake{FenceAttached: true})
	var session browser.Session
	response := post(t, server, "/open", OpenRequest{URL: "https://example.org/"}, true)
	json.NewDecoder(response.Body).Decode(&session)

	response = post(t, server, "/close", SessionRequest{SessionID: string(session.ID)}, true)
	if response.StatusCode != http.StatusNoContent {
		t.Fatalf("close: got %d, want 204", response.StatusCode)
	}
	response = post(t, server, "/snapshot", SessionRequest{SessionID: string(session.ID)}, true)
	if response.StatusCode != http.StatusNotFound {
		t.Fatalf("snapshot after close: got %d, want 404", response.StatusCode)
	}
}

func TestServeConfigIsLoopbackOnly(t *testing.T) {
	// Serve takes whatever config.Load produced; the loopback guarantee is config's. This asserts
	// the default the two agree on, so a change to one is visible from the other.
	if config.DefaultAddr != "127.0.0.1:8795" {
		t.Fatalf("default addr drifted: %q", config.DefaultAddr)
	}
}
