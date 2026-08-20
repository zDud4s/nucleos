package chrome

import (
	"context"
	"encoding/json"
	"net/http"
	"strings"
	"testing"
)

// TestTheCorsRuleIsTheBrowsersOwn.
//
// Not an approximation of it. The ferry makes the request that a page's own `fetch` would have made,
// so the question of who may READ the answer has to be answered the way the browser answers it —
// otherwise the fence is either narrower than a browser, which leaves working pages blank, or wider,
// which hands a page data no browser would give it.
//
// The row that matters most is the last one. A wildcard together with credentials is the one pair
// CORS does not have, and it does not have it because "any page may read this with the visitor's
// session" is the shape of every credentialed-CORS hole there has been.
func TestTheCorsRuleIsTheBrowsersOwn(t *testing.T) {
	const page = "https://app.example.com"

	for _, one := range []struct {
		name         string
		allow        string
		credentials  string
		credentialed bool
		read         bool
	}{
		{name: "nothing said", read: false},
		{name: "open to everyone", allow: "*", read: true},
		{name: "open to this page", allow: page, read: true},
		{name: "open to a different page", allow: "https://other.example.com", read: false},
		{name: "credentialed and named and allowed", allow: page, credentials: "true", credentialed: true, read: true},
		{name: "credentialed but not allowed", allow: page, credentialed: true, read: false},
		{name: "credentialed against a wildcard", allow: "*", credentials: "true", credentialed: true, read: false},
	} {
		t.Run(one.name, func(t *testing.T) {
			header := http.Header{}
			if one.allow != "" {
				header.Set("Access-Control-Allow-Origin", one.allow)
			}
			if one.credentials != "" {
				header.Set("Access-Control-Allow-Credentials", one.credentials)
			}

			err := corsAllows(header, page, one.credentialed)
			if one.read && err != nil {
				t.Fatalf("a browser would have read this: %v", err)
			}
			if !one.read && err == nil {
				t.Fatal("a browser would have refused this")
			}
			if err != nil && strings.TrimSpace(err.Error()) == "" {
				t.Error("a refusal with nothing in it is the silence this whole layer exists to end")
			}
		})
	}
}

// TestACookieGoesBackIntoTheProfileAsItWasSent.
//
// The ferry makes its request outside the browser, so what the server sets has to be put back by
// hand — and SameSite was being dropped. Chromium treats an unspecified one as Lax, so a cookie
// issued as SameSite=None came back narrower than it left, and the profile quietly stopped agreeing
// with the server about its own session.
//
// Nothing fails at the time. The symptom arrives much later and somewhere else, as a cross-site
// request that no longer carries a login nobody logged out of.
func TestACookieGoesBackIntoTheProfileAsItWasSent(t *testing.T) {
	for _, one := range []struct {
		name string
		mode http.SameSite
		want any
	}{
		{name: "none", mode: http.SameSiteNoneMode, want: "None"},
		{name: "lax", mode: http.SameSiteLaxMode, want: "Lax"},
		{name: "strict", mode: http.SameSiteStrictMode, want: "Strict"},
		// Not stated is not the same as stating a default, and inventing one here would be this bug
		// with the sign flipped.
		{name: "unstated", mode: http.SameSiteDefaultMode, want: nil},
	} {
		t.Run(one.name, func(t *testing.T) {
			fake, driver := connected(t)
			driver.keepCookies(context.Background(), "S1", "https://example.org/api", []*http.Cookie{
				{Name: "session", Value: "abc", SameSite: one.mode, Secure: true, HttpOnly: true},
			})

			var set map[string]any
			for _, call := range fake.Calls() {
				if call.Method != "Network.setCookies" {
					continue
				}
				var params struct {
					Cookies []map[string]any `json:"cookies"`
				}
				if err := json.Unmarshal(call.Params, &params); err != nil || len(params.Cookies) == 0 {
					continue
				}
				set = params.Cookies[0]
			}
			if set == nil {
				t.Fatalf("the cookie was never put back; calls were %v", fake.Methods())
			}
			if got := set["sameSite"]; got != one.want {
				t.Errorf("the server said %v and the profile was given %v", one.want, got)
			}
		})
	}
}

// TestAStreamIsRecognisedByWhatTheServerCalledIt.
//
// The parse is the point. `text/event-stream; charset=utf-8` is the same declaration as the bare
// type, and a substring check would be one header parameter away from missing it — which is a
// refusal that arrives thirty seconds later as a timeout instead of at once as a rule.
func TestAStreamIsRecognisedByWhatTheServerCalledIt(t *testing.T) {
	for _, one := range []struct {
		header string
		stream bool
	}{
		{header: "text/event-stream", stream: true},
		{header: "text/event-stream; charset=utf-8", stream: true},
		{header: "TEXT/EVENT-STREAM", stream: true},
		{header: " text/event-stream ", stream: true},
		{header: "text/html; charset=utf-8", stream: false},
		{header: "application/json", stream: false},
		{header: "", stream: false},
		{header: "nonsense", stream: false},
	} {
		if got := streaming(one.header); got != one.stream {
			t.Errorf("%q was read as stream=%v", one.header, got)
		}
	}
}
