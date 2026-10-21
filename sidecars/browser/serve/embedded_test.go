// §spec pilar-de-browser

package serve

import (
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"

	"nucleosbrowser/browser"
	"nucleosbrowser/profile"
)

func embeddedServer(t *testing.T, driver browser.Driver) *httptest.Server {
	t.Helper()
	mux := http.NewServeMux()
	embeddedRoutes(mux, token, driver)
	server := httptest.NewServer(mux)
	t.Cleanup(server.Close)
	return server
}

// bare hides every optional interface of the Fake, as a driver that implements none of them would.
type bare struct{ browser.Driver }

func embeddedPlacement() browser.Placement {
	return browser.Placement{
		Profile: profile.Ref{Kind: profile.Project, ID: "p1"},
		Origins: []string{"https://example.org"},
	}
}

func attachBody() browser.AttachRequest {
	return browser.AttachRequest{
		Project:   "p1",
		CDPHTTP:   "http://127.0.0.1:9222",
		Nonce:     "n1",
		URL:       "https://example.org/",
		Placement: embeddedPlacement(),
	}
}

func TestEmbeddedRoutesAnswer501WhenTheDriverCannot(t *testing.T) {
	server := embeddedServer(t, bare{&browser.Fake{}})
	for path, body := range map[string]any{
		"/embedded/prepare": browser.PrepareRequest{Project: "p1", Placement: embeddedPlacement()},
		"/embedded/attach":  attachBody(),
		"/embedded/forget":  forgetEmbeddedRequest{Project: "p1"},
	} {
		response := post(t, server, path, body, true)
		if response.StatusCode != http.StatusNotImplemented {
			t.Errorf("%s: got %d, want 501", path, response.StatusCode)
		}
	}
}

func TestEmbeddedRoutesRequireTheToken(t *testing.T) {
	server := embeddedServer(t, &browser.Fake{})
	for _, path := range []string{"/embedded/prepare", "/embedded/attach", "/embedded/forget"} {
		response := post(t, server, path, map[string]string{"project": "p1"}, false)
		if response.StatusCode != http.StatusUnauthorized {
			t.Errorf("%s: got %d, want 401", path, response.StatusCode)
		}
	}
}

func TestEmbeddedPrepareAnswersTheProxyAddress(t *testing.T) {
	fake := &browser.Fake{}
	server := embeddedServer(t, fake)
	response := post(t, server, "/embedded/prepare",
		browser.PrepareRequest{Project: "p1", Placement: embeddedPlacement()}, true)
	if response.StatusCode != http.StatusOK {
		t.Fatalf("got %d, want 200", response.StatusCode)
	}
	var reply browser.PrepareReply
	if err := json.Unmarshal([]byte(readBody(t, response)), &reply); err != nil {
		t.Fatal(err)
	}
	if reply.ProxyAddr == "" {
		t.Error("no proxy_addr")
	}
	if len(fake.Prepared) != 1 || fake.Prepared[0].Project != "p1" ||
		len(fake.Prepared[0].Placement.Origins) != 1 {
		t.Errorf("fake saw %+v", fake.Prepared)
	}
}

func TestEmbeddedAttachAnswersASessionAndRecordsEveryField(t *testing.T) {
	fake := &browser.Fake{}
	server := embeddedServer(t, fake)
	response := post(t, server, "/embedded/attach", attachBody(), true)
	if response.StatusCode != http.StatusOK {
		t.Fatalf("got %d, want 200", response.StatusCode)
	}
	var session browser.Session
	if err := json.Unmarshal([]byte(readBody(t, response)), &session); err != nil {
		t.Fatal(err)
	}
	if session.ID == "" || session.RequestedURL != "https://example.org/" {
		t.Errorf("session = %+v", session)
	}
	got := fake.Attached
	if len(got) != 1 || got[0].CDPHTTP != "http://127.0.0.1:9222" || got[0].Nonce != "n1" ||
		got[0].Project != "p1" || got[0].URL != "https://example.org/" {
		t.Errorf("fake saw %+v", got)
	}
}

func TestEmbeddedForgetAnswers204ForAnyProject(t *testing.T) {
	fake := &browser.Fake{}
	server := embeddedServer(t, fake)
	response := post(t, server, "/embedded/forget", forgetEmbeddedRequest{Project: "never-prepared"}, true)
	if response.StatusCode != http.StatusNoContent {
		t.Fatalf("got %d, want 204", response.StatusCode)
	}
	if len(fake.EmbeddedForgotten) != 1 || fake.EmbeddedForgotten[0] != "never-prepared" {
		t.Errorf("fake saw %v", fake.EmbeddedForgotten)
	}
}

func TestEmbeddedRoutesRefuseMissingFieldsAndMalformedJSON(t *testing.T) {
	server := embeddedServer(t, &browser.Fake{})

	missing := map[string]any{
		"/embedded/prepare": browser.PrepareRequest{Placement: embeddedPlacement()},
		"/embedded/forget":  forgetEmbeddedRequest{},
	}
	for path, body := range missing {
		if got := post(t, server, path, body, true).StatusCode; got != http.StatusBadRequest {
			t.Errorf("%s without project: got %d, want 400", path, got)
		}
	}
	if got := post(t, server, "/embedded/prepare", browser.PrepareRequest{Project: "p1"}, true).StatusCode; got != http.StatusBadRequest {
		t.Errorf("prepare without placement: got %d, want 400", got)
	}
	for _, field := range []string{"project", "cdp_http", "nonce", "url"} {
		body := attachBody()
		switch field {
		case "project":
			body.Project = ""
		case "cdp_http":
			body.CDPHTTP = ""
		case "nonce":
			body.Nonce = ""
		case "url":
			body.URL = ""
		}
		if got := post(t, server, "/embedded/attach", body, true).StatusCode; got != http.StatusBadRequest {
			t.Errorf("attach without %s: got %d, want 400", field, got)
		}
	}

	for _, path := range []string{"/embedded/prepare", "/embedded/attach", "/embedded/forget"} {
		request, err := http.NewRequest(http.MethodPost, server.URL+path, strings.NewReader("{not json"))
		if err != nil {
			t.Fatal(err)
		}
		request.Header.Set("Authorization", "Bearer "+token)
		response, err := http.DefaultClient.Do(request)
		if err != nil {
			t.Fatal(err)
		}
		response.Body.Close()
		if response.StatusCode != http.StatusBadRequest {
			t.Errorf("%s with malformed JSON: got %d, want 400", path, response.StatusCode)
		}
	}
}

func TestEmbeddedDetachAnswers204AndReattachAnswersTheChain(t *testing.T) {
	fake := &browser.Fake{EmbeddedChain: []string{"https://a/", "https://b/"}}
	server := embeddedServer(t, fake)
	session, err := fake.AttachEmbedded(t.Context(), attachBody())
	if err != nil {
		t.Fatal(err)
	}
	body := browser.EmbeddedSessionRequest{SessionID: session.ID}
	if got := post(t, server, "/embedded/detach", body, true).StatusCode; got != http.StatusNoContent {
		t.Fatalf("detach: got %d, want 204", got)
	}
	response := post(t, server, "/embedded/reattach", body, true)
	if response.StatusCode != http.StatusOK {
		t.Fatalf("reattach: got %d, want 200", response.StatusCode)
	}
	var returned browser.Returned
	if err := json.Unmarshal([]byte(readBody(t, response)), &returned); err != nil {
		t.Fatal(err)
	}
	if len(returned.Chain) != 2 || returned.Chain[0] != "https://a/" || returned.Chain[1] != "https://b/" {
		t.Errorf("chain = %v", returned.Chain)
	}
	if len(fake.EmbeddedDetached) != 1 || fake.EmbeddedDetached[0] != session.ID ||
		len(fake.EmbeddedReattached) != 1 || fake.EmbeddedReattached[0] != session.ID {
		t.Errorf("fake saw detach %v reattach %v", fake.EmbeddedDetached, fake.EmbeddedReattached)
	}
}

func TestEmbeddedReattachAnswersAnEmptyChainAsAnArrayNeverNull(t *testing.T) {
	fake := &browser.Fake{}
	server := embeddedServer(t, fake)
	session, err := fake.AttachEmbedded(t.Context(), attachBody())
	if err != nil {
		t.Fatal(err)
	}
	response := post(t, server, "/embedded/reattach", browser.EmbeddedSessionRequest{SessionID: session.ID}, true)
	if response.StatusCode != http.StatusOK {
		t.Fatalf("reattach: got %d, want 200", response.StatusCode)
	}
	raw := readBody(t, response)
	if !strings.Contains(raw, `"chain":[]`) || strings.Contains(raw, "null") {
		t.Errorf("body = %s, want an empty chain array", raw)
	}
}

func TestEmbeddedDetachAndReattachRefuseWhatTheyShould(t *testing.T) {
	for _, path := range []string{"/embedded/detach", "/embedded/reattach"} {
		body := browser.EmbeddedSessionRequest{SessionID: "s1"}
		if got := post(t, embeddedServer(t, bare{&browser.Fake{}}), path, body, true).StatusCode; got != http.StatusNotImplemented {
			t.Errorf("%s on a driver without Embedded: got %d, want 501", path, got)
		}
		server := embeddedServer(t, &browser.Fake{})
		if got := post(t, server, path, body, false).StatusCode; got != http.StatusUnauthorized {
			t.Errorf("%s without the token: got %d, want 401", path, got)
		}
		if got := post(t, server, path, browser.EmbeddedSessionRequest{}, true).StatusCode; got != http.StatusBadRequest {
			t.Errorf("%s with a blank session_id: got %d, want 400", path, got)
		}
		if got := post(t, server, path, browser.EmbeddedSessionRequest{SessionID: "nope"}, true).StatusCode; got != http.StatusNotFound {
			t.Errorf("%s with an unknown session: got %d, want 404", path, got)
		}
	}
}

func TestEmbeddedErrorsMapToTheirStatus(t *testing.T) {
	cases := map[string]struct {
		err  error
		want int
	}{
		"bad cdp address":    {browser.ErrBadCDPAddress, http.StatusBadRequest},
		"no such target":     {browser.ErrNoSuchTarget, http.StatusNotFound},
		"not prepared":       {browser.ErrNotPrepared, http.StatusConflict},
		"placement mismatch": {browser.ErrPlacementMismatch, http.StatusConflict},
		"policy changed":     {browser.ErrEmbeddedPolicyChanged, http.StatusConflict},
		"too many sessions":  {browser.ErrTooManySessions, http.StatusConflict},
		"fence not attached": {browser.ErrFenceNotAttached, http.StatusServiceUnavailable},
	}
	for name, tc := range cases {
		t.Run(name, func(t *testing.T) {
			server := embeddedServer(t, &browser.Fake{EmbeddedErr: tc.err})
			if got := post(t, server, "/embedded/attach", attachBody(), true).StatusCode; got != tc.want {
				t.Errorf("got %d, want %d", got, tc.want)
			}
		})
	}
}
