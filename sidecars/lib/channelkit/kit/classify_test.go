package kit

import (
	"context"
	"errors"
	"io/fs"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"

	"nucleoschannelkit/contract/fixtures"
)

func errorFixture(t *testing.T, name string) []byte {
	t.Helper()
	b, err := fs.ReadFile(fixtures.FS(), "errors/"+name)
	if err != nil {
		t.Fatalf("read fixture %s: %v", name, err)
	}
	return b
}

func TestClassifyTable(t *testing.T) {
	badRequest := errorFixture(t, "bad-request.json")
	tooLarge := errorFixture(t, "media-too-large.json")
	notLeased := errorFixture(t, "not-leased.json")
	unknownID := []byte(`{"error":"unknown_id"}`)
	none := []byte(nil)

	cases := []struct {
		name     string
		endpoint Endpoint
		status   int
		body     []byte
		want     Class
	}{
		{"events 400", EndpointEvents, 400, badRequest, SkipEvent},
		{"events 413", EndpointEvents, 413, tooLarge, SkipEvent},
		{"events 422", EndpointEvents, 422, badRequest, SkipEvent},
		{"events 408", EndpointEvents, 408, none, Retry},
		{"events 429", EndpointEvents, 429, none, Retry},
		{"events 500", EndpointEvents, 500, none, Retry},
		{"events 503", EndpointEvents, 503, none, Retry},
		{"events 200", EndpointEvents, 200, none, OK},
		{"events 202", EndpointEvents, 202, none, OK},

		{"ack 409 not_leased", EndpointAck, 409, notLeased, StopRenewing},
		{"ack 404 unknown_id", EndpointAck, 404, unknownID, GiveUp},
		{"ack 400", EndpointAck, 400, badRequest, GiveUp},
		{"ack 403", EndpointAck, 403, none, Fatal},
		{"ack 429", EndpointAck, 429, none, Retry},
		{"ack 502", EndpointAck, 502, none, Retry},
		{"ack 200", EndpointAck, 200, none, OK},

		{"renew 409 not_leased", EndpointRenew, 409, notLeased, StopRenewing},
		{"renew 404", EndpointRenew, 404, unknownID, GiveUp},
		{"renew 500", EndpointRenew, 500, none, Retry},
		{"renew 200", EndpointRenew, 200, none, OK},

		{"media 413", EndpointMedia, 413, tooLarge, SendAsUnsupported},
		{"media 404", EndpointMedia, 404, none, NotFound},
		{"media 403", EndpointMedia, 403, none, Fatal},
		{"media 429", EndpointMedia, 429, none, Retry},

		{"outbox 400 is not an event skip", EndpointOutbox, 400, badRequest, Fatal},
		{"outbox 413 is not an event skip", EndpointOutbox, 413, tooLarge, Fatal},
		{"outbox 500", EndpointOutbox, 500, none, Retry},
		{"hello 400", EndpointHello, 400, badRequest, Fatal},
		{"hello 422", EndpointHello, 422, badRequest, Fatal},
	}
	for _, c := range cases {
		t.Run(c.name, func(t *testing.T) {
			if got := Classify(c.endpoint, c.status, c.body); got != c.want {
				t.Errorf("Classify(%v, %d) = %v, want %v", c.endpoint, c.status, got, c.want)
			}
		})
	}
}

func TestClassifyEveryEndpoint(t *testing.T) {
	contractVersion := errorFixture(t, "contract-version.json")
	mismatch := errorFixture(t, "channel-mismatch.json")
	notLeased := errorFixture(t, "not-leased.json")
	endpoints := []Endpoint{EndpointHello, EndpointEvents, EndpointOutbox, EndpointAck, EndpointRenew, EndpointMedia}
	for _, ep := range endpoints {
		if got := Classify(ep, 401, nil); got != Fatal {
			t.Errorf("endpoint %v 401 = %v, want Fatal", ep, got)
		}
		if got := Classify(ep, 401, []byte(`not json`)); got != Fatal {
			t.Errorf("endpoint %v 401 junk body = %v, want Fatal", ep, got)
		}
		if got := Classify(ep, 409, contractVersion); got != Fatal {
			t.Errorf("endpoint %v 409 contract_version = %v, want Fatal", ep, got)
		}
		if got := Classify(ep, 403, mismatch); got != Fatal {
			t.Errorf("endpoint %v 403 channel_mismatch = %v, want Fatal", ep, got)
		}
		if got := Classify(ep, 204, nil); got != OK {
			t.Errorf("endpoint %v 204 = %v, want OK", ep, got)
		}
		if got := Classify(ep, 429, nil); got != Retry {
			t.Errorf("endpoint %v 429 = %v, want Retry", ep, got)
		}
		if got := Classify(ep, 503, nil); got != Retry {
			t.Errorf("endpoint %v 503 = %v, want Retry", ep, got)
		}
		if got := Classify(ep, 0, nil); got != Retry {
			t.Errorf("endpoint %v status 0 (network error) = %v, want Retry", ep, got)
		}
		if ep != EndpointAck && ep != EndpointRenew {
			if got := Classify(ep, 409, notLeased); got == StopRenewing {
				t.Errorf("endpoint %v 409 not_leased must not stop renewing", ep)
			}
		}
		if ep != EndpointEvents {
			for _, st := range []int{400, 413, 422} {
				if got := Classify(ep, st, nil); got == SkipEvent {
					t.Errorf("endpoint %v status %d = SkipEvent, only events may skip", ep, st)
				}
			}
		}
	}
}

func TestClassifyUnparsableConflictIsFatal(t *testing.T) {
	if got := Classify(EndpointAck, 409, []byte("<html>")); got != Fatal {
		t.Errorf("ack 409 with junk body = %v, want Fatal", got)
	}
}

func TestClientSendsBearerToken(t *testing.T) {
	var gotAuth, gotPath string
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		gotAuth = r.Header.Get("Authorization")
		gotPath = r.URL.Path
		w.WriteHeader(200)
		_, _ = w.Write([]byte(`{}`))
	}))
	defer srv.Close()

	c := NewClient(Config{BaseURL: srv.URL + "/", Channel: "telegram", Token: "s3cret-token"})
	body, status, err := c.do(context.Background(), http.MethodGet, "/channels/telegram/outbox", nil, "")
	if err != nil || status != 200 || string(body) != `{}` {
		t.Fatalf("do = %q, %d, %v", body, status, err)
	}
	if gotAuth != "Bearer s3cret-token" {
		t.Errorf("Authorization = %q", gotAuth)
	}
	if gotPath != "/channels/telegram/outbox" {
		t.Errorf("path = %q (trailing slash of BaseURL must not double up)", gotPath)
	}
}

func TestClientDefaultHTTPHasNoGlobalTimeout(t *testing.T) {
	c := NewClient(Config{BaseURL: "http://127.0.0.1:1", Token: "t"})
	if c.http == nil || c.http.Timeout != 0 {
		t.Errorf("default http client must exist with Timeout 0, got %+v", c.http)
	}
	if c.clock == nil {
		t.Error("default clock must be set")
	}
}

func TestClientRedactsTokenInErrors(t *testing.T) {
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {}))
	url := srv.URL
	srv.Close() // connection refused from here on

	const token = "tk-very-secret"
	c := NewClient(Config{BaseURL: url, Token: token})
	_, status, err := c.do(context.Background(), http.MethodGet, "/x", nil, "")
	if err == nil {
		t.Fatal("expected a network error")
	}
	if status != 0 {
		t.Errorf("status = %d, want 0", status)
	}
	if strings.Contains(err.Error(), token) {
		t.Errorf("error leaks the token: %v", err)
	}
	if Classify(EndpointEvents, status, nil) != Retry {
		t.Error("network error must classify as Retry")
	}

	// A token embedded in the base URL path must be redacted too.
	c2 := NewClient(Config{BaseURL: url + "/" + token, Token: token})
	_, _, err = c2.do(context.Background(), http.MethodGet, "/x", nil, "")
	if err == nil || strings.Contains(err.Error(), token) {
		t.Errorf("error leaks the token via URL: %v", err)
	}
}

func TestClientContextCancelUnwraps(t *testing.T) {
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {}))
	defer srv.Close()
	ctx, cancel := context.WithCancel(context.Background())
	cancel()
	c := NewClient(Config{BaseURL: srv.URL, Token: "t"})
	_, _, err := c.do(ctx, http.MethodGet, "/x", nil, "")
	if !errors.Is(err, context.Canceled) {
		t.Errorf("err = %v, want context.Canceled in chain", err)
	}
}
