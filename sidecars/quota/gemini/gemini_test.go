package gemini

import (
	"context"
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"nucleosquota/reading"
)

// The shape third-party readers captured from the daily host in 2026-09: one bucket per window,
// what is LEFT as a fraction, and a sentence about it that this package ignores.
const summaryShape = `{
  "buckets": [
    {"bucketId":"gemini-weekly","displayName":"Weekly Limit Remaining","remainingFraction":0.5004207,
     "resetTime":"2099-09-24T14:06:36Z","description":"weekly"},
    {"bucketId":"gemini-5h","displayName":"Five Hour Limit Remaining","remainingFraction":0.25,
     "resetTime":"2099-09-20T20:33:19Z","description":"five hour"},
    {"bucketId":"gemini-something-new","remainingFraction":0.1}
  ]
}`

const loadShape = `{"cloudaicompanionProject":"proj-123","currentTier":{"id":"g1-pro-tier"}}`

// What is left becomes what is used: 0.25 left of the 5-hour window is 75% gone. Forwarded
// untouched it would draw every ring inside out.
func remaining_becomes_used(t *testing.T) {
	got, present := readAgainst(t, loadShape, http.StatusOK, summaryShape, validToken(t))

	if !present || got.Fidelity != reading.Official {
		t.Fatalf("got %+v present=%v, want an official reading", got, present)
	}
	if len(got.Windows) != 2 {
		t.Fatalf("windows = %+v, want 5h and 7d", got.Windows)
	}
	if got.Windows[0].Name != "5h" || got.Windows[0].UsedFraction != 0.75 {
		t.Errorf("5h = %+v, want used 0.75", got.Windows[0])
	}
	if got.Windows[1].Name != "7d" || got.Windows[1].UsedFraction < 0.499 || got.Windows[1].UsedFraction > 0.5 {
		t.Errorf("7d = %+v, want used ~0.4996", got.Windows[1])
	}
	if got.Detail != "tier g1-pro-tier" {
		t.Errorf("detail = %q, want the tier", got.Detail)
	}
}

// Buckets nested under groups are read too: the summary has been seen carrying them both ways.
func grouped_buckets_are_read(t *testing.T) {
	body := `{"groups":[{"displayName":"Gemini","buckets":[{"bucketId":"gemini-5h","remainingFraction":1}]}]}`
	got, _ := readAgainst(t, loadShape, http.StatusOK, body, validToken(t))
	if len(got.Windows) != 1 || got.Windows[0].Name != "5h" || got.Windows[0].UsedFraction != 0 {
		t.Fatalf("windows = %+v, want one empty 5h window", got.Windows)
	}
}

// Most owners never signed in to Gemini. They get no ring at all, not a dashed one.
func no_credential_file_leaves_the_provider_out(t *testing.T) {
	r := New(2*time.Second, t.TempDir())
	if _, present := r.Read(context.Background(), time.Now()); present {
		t.Fatal("a machine with no Gemini sign-in must not report a gemini provider")
	}
}

// A free account is refused with SUBSCRIPTION_REQUIRED: nothing to measure, so nothing to draw.
func a_free_account_leaves_the_provider_out(t *testing.T) {
	body := `{"error":{"code":403,"status":"PERMISSION_DENIED","details":[{"reason":"SUBSCRIPTION_REQUIRED"}]}}`
	if _, present := readAgainst(t, loadShape, http.StatusForbidden, body, validToken(t)); present {
		t.Fatal("an account without a subscription must not report a gemini provider")
	}
}

// Signed in, but the token has run out: that is worth saying, because running gemini fixes it.
func an_expired_token_is_unmeasured_before_any_call(t *testing.T) {
	home := t.TempDir()
	writeCredentials(t, home, "tok", time.Now().Add(-2*time.Hour).UnixMilli())
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		t.Error("an expired token must not reach the endpoint")
	}))
	defer server.Close()

	r := New(2*time.Second, home)
	r.loadEndpoint, r.quotaEndpoint = server.URL, server.URL
	got, present := r.Read(context.Background(), time.Now())
	if !present || got.Fidelity != reading.Unmeasured || !strings.Contains(got.Detail, "run gemini") {
		t.Fatalf("got %+v present=%v, want unmeasured asking to run gemini", got, present)
	}
}

func no_answer_ever_carries_the_token(t *testing.T) {
	home := t.TempDir()
	writeCredentials(t, home, "secret-gemini-token", time.Now().Add(time.Hour).UnixMilli())
	for _, status := range []int{http.StatusUnauthorized, http.StatusInternalServerError} {
		got, _ := readAgainst(t, loadShape, status, `{"error":"secret-gemini-token"}`, home)
		raw, _ := json.Marshal(got)
		if strings.Contains(string(raw), "secret-gemini-token") {
			t.Fatalf("status %d: the answer carries the token: %s", status, raw)
		}
		if got.Fidelity != reading.Unmeasured {
			t.Errorf("status %d: fidelity = %q, want unmeasured", status, got.Fidelity)
		}
	}
}

// The quota is asked for the project loadCodeAssist named, with the token as a bearer header.
func the_project_and_token_travel_with_the_request(t *testing.T) {
	var project, auth string
	quota := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		var body map[string]string
		_ = json.NewDecoder(r.Body).Decode(&body)
		project, auth = body["project"], r.Header.Get("Authorization")
		_, _ = w.Write([]byte(summaryShape))
	}))
	defer quota.Close()
	load := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		_, _ = w.Write([]byte(`{"cloudaicompanionProject":{"id":"proj-obj"}}`))
	}))
	defer load.Close()

	r := New(2*time.Second, validToken(t))
	r.loadEndpoint, r.quotaEndpoint = load.URL, quota.URL
	r.getenv = func(string) string { return "" }
	r.Read(context.Background(), time.Now())
	if project != "proj-obj" || auth != "Bearer tok" {
		t.Fatalf("project = %q, auth = %q", project, auth)
	}
}

func TestGemini(t *testing.T) {
	t.Run("remaining becomes used", remaining_becomes_used)
	t.Run("grouped buckets are read", grouped_buckets_are_read)
	t.Run("no credential file leaves the provider out", no_credential_file_leaves_the_provider_out)
	t.Run("a free account leaves the provider out", a_free_account_leaves_the_provider_out)
	t.Run("an expired token is unmeasured before any call", an_expired_token_is_unmeasured_before_any_call)
	t.Run("no answer ever carries the token", no_answer_ever_carries_the_token)
	t.Run("the project and token travel with the request", the_project_and_token_travel_with_the_request)
}

func validToken(t *testing.T) string {
	t.Helper()
	home := t.TempDir()
	writeCredentials(t, home, "tok", time.Now().Add(time.Hour).UnixMilli())
	return home
}

func writeCredentials(t *testing.T, home, token string, expiry int64) {
	t.Helper()
	dir := filepath.Join(home, ".gemini")
	if err := os.MkdirAll(dir, 0o700); err != nil {
		t.Fatal(err)
	}
	raw, _ := json.Marshal(map[string]any{"access_token": token, "expiry_date": expiry, "refresh_token": "r"})
	if err := os.WriteFile(filepath.Join(dir, "oauth_creds.json"), raw, 0o600); err != nil {
		t.Fatal(err)
	}
}

// readAgainst answers loadCodeAssist with loadBody and the summary with status/body.
func readAgainst(t *testing.T, loadBody string, status int, body, home string) (reading.Provider, bool) {
	t.Helper()
	load := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		_, _ = w.Write([]byte(loadBody))
	}))
	defer load.Close()
	quota := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.WriteHeader(status)
		_, _ = w.Write([]byte(body))
	}))
	defer quota.Close()

	r := New(2*time.Second, home)
	r.loadEndpoint, r.quotaEndpoint = load.URL, quota.URL
	r.getenv = func(string) string { return "" }
	return r.Read(context.Background(), time.Now())
}
