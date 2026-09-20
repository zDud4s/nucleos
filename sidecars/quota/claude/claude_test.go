package claude

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

// The payload captured from the live endpoint on 2026-09-19, keeping every structural surprise:
// `utilization` as a PERCENTAGE in a float field despite its name, a populated code-named window
// whose reset is null, and the sixteen experiment slots that come and go without notice.
const capturedShape = `{
  "five_hour":  {"utilization": 54.0, "resets_at": "2026-09-19T16:40:00Z",
                 "limit_dollars": null, "used_dollars": null, "locked_reason": null},
  "seven_day":  {"utilization": 46.0, "resets_at": "2026-09-24T00:00:00Z",
                 "limit_dollars": null, "used_dollars": null, "locked_reason": null},
  "seven_day_opus": null, "seven_day_sonnet": null, "seven_day_cowork": null,
  "tangelo": null, "iguana_necktie": null, "omelette_promotional": null,
  "nimbus_quill": {"utilization": 0.02, "resets_at": null},
  "cinder_cove": null, "copper_kite": null, "harbor_lantern": null,
  "wattle_ember": null, "amber_ladder": null, "juniper_tide": null,
  "cedar_ember": null, "amber_gauge": null,
  "limits": [{"kind":"session","group":"session","percent":54,"severity":"normal",
              "resets_at":"2026-09-19T16:40:00Z","scope":null,"is_active":true}],
  "spend": {"used": {"amount_minor": 0, "currency": "USD", "exponent": 2},
            "percent": 0, "severity": "normal", "enabled": false},
  "member_dashboard_available": false,
  "seven_day_breakdown": {"as_of":"2026-09-19T02:00:00Z","window_started_at":"2026-09-12T00:00:00Z",
                          "rows":[{"key":"claude_code","display_name":"Claude Code","percent":18}]}
}`

// The regression this package shipped and had to be caught by running it: `utilization` is named
// like a fraction and is a PERCENTAGE. The first version forwarded it untouched, the clamp turned
// 54.0 into 1, and both rings drew full while the real figures were 54% and 46%. The bug survived a
// green suite because the fixture had been written with 0.31 in it — the assumption was tested
// against itself.
//
// The values below are the live endpoint's own, measured 2026-09-19: utilization 54.0/46.0 with
// limits[].percent 54/46 in the same payload, which is what proves the unit.
func a_percentage_named_utilization_becomes_a_fraction(t *testing.T) {
	got := readAgainst(t, http.StatusOK, capturedShape, validToken(t))

	if got.Fidelity != reading.Official {
		t.Fatalf("fidelity = %q, want %q", got.Fidelity, reading.Official)
	}
	if len(got.Windows) != 2 {
		t.Fatalf("windows = %d, want 2 (5h and 7d)", len(got.Windows))
	}
	if got.Windows[0].Name != "5h" || got.Windows[0].UsedFraction != 0.54 {
		t.Errorf("5h = %+v, want used_fraction 0.54", got.Windows[0])
	}
	if got.Windows[1].Name != "7d" || got.Windows[1].UsedFraction != 0.46 {
		t.Errorf("7d = %+v, want used_fraction 0.46", got.Windows[1])
	}
}

// The guard against the clamp hiding the next unit change: a reading that is merely high must stay
// distinguishable from one that is at the cap. Under the old code every value above 1 collapsed to
// a full ring, so a contract change looked exactly like an exhausted quota.
func a_high_reading_is_not_silently_rounded_to_full(t *testing.T) {
	body := `{"five_hour":{"utilization":99.0,"resets_at":null},"seven_day":{"utilization":3.0,"resets_at":null}}`
	got := readAgainst(t, http.StatusOK, body, validToken(t))

	if len(got.Windows) != 2 {
		t.Fatalf("windows = %d, want 2", len(got.Windows))
	}
	if got.Windows[0].UsedFraction != 0.99 {
		t.Errorf("5h = %v, want 0.99 — 99%% is not the same fact as being at the cap", got.Windows[0].UsedFraction)
	}
	if got.Windows[1].UsedFraction != 0.03 {
		t.Errorf("7d = %v, want 0.03", got.Windows[1].UsedFraction)
	}
}

// Sixteen code-named keys were present in the capture and will not be the same sixteen next month.
// A parser that needs to know them breaks on the vendor's schedule rather than on ours.
func the_code_named_keys_are_ignored_entirely(t *testing.T) {
	got := readAgainst(t, http.StatusOK, capturedShape, validToken(t))

	for _, w := range got.Windows {
		if w.Name != "5h" && w.Name != "7d" {
			t.Errorf("an experiment slot reached the notch as window %q", w.Name)
		}
	}
	if len(got.Windows) != 2 {
		t.Fatalf("windows = %d, want exactly the two this design draws", len(got.Windows))
	}
}

// The capture proved a window can carry a utilization and no reset. Whatever draws this must be
// handed a nil rather than a zero time, which would read as 1970 and mark everything stale.
func a_window_without_a_reset_does_not_crash_the_reader(t *testing.T) {
	body := `{"five_hour":{"utilization":50.0,"resets_at":null},"seven_day":{"utilization":10.0,"resets_at":null}}`
	got := readAgainst(t, http.StatusOK, body, validToken(t))

	if len(got.Windows) != 2 {
		t.Fatalf("windows = %d, want 2", len(got.Windows))
	}
	for _, w := range got.Windows {
		if w.ResetsAt != nil {
			t.Errorf("window %q invented a reset: %v", w.Name, w.ResetsAt)
		}
		if w.Stale {
			t.Errorf("window %q with no reset must not be stale", w.Name)
		}
	}
}

// An expired token is checked against the clock before the call, so the owner is told to sign in
// rather than shown a 401 that reads like the vendor revoking access.
func an_expired_token_is_refused_before_the_call(t *testing.T) {
	home := t.TempDir()
	writeCredentials(t, home, "tok", time.Now().Add(-2*time.Hour).UnixMilli())

	reached := false
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		reached = true
	}))
	defer server.Close()

	r := New(2*time.Second, home)
	r.endpoint = server.URL
	got := r.Read(context.Background(), time.Now())

	if reached {
		t.Error("an expired credential still went out to the vendor")
	}
	if got.Fidelity != reading.Unmeasured {
		t.Fatalf("fidelity = %q, want %q", got.Fidelity, reading.Unmeasured)
	}
	if !strings.Contains(got.Detail, "expired") {
		t.Errorf("detail = %q, want it to say the credential expired", got.Detail)
	}
}

// The single most important property in this package: nothing it reports ever carries the token.
// A feed row, a tooltip and a log line all end up somewhere the owner did not choose.
func no_answer_ever_carries_the_token(t *testing.T) {
	const secret = "sk-ant-oat01-NEVER-LEAK-THIS"
	home := t.TempDir()
	writeCredentials(t, home, secret, time.Now().Add(time.Hour).UnixMilli())

	for _, tc := range []struct {
		name   string
		status int
		body   string
	}{
		{"refused", http.StatusUnauthorized, `{}`},
		{"broken", http.StatusInternalServerError, `nope`},
		{"garbage", http.StatusOK, `not json at all`},
		{"empty", http.StatusOK, `{}`},
	} {
		server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
			w.WriteHeader(tc.status)
			_, _ = w.Write([]byte(tc.body))
		}))
		r := New(2*time.Second, home)
		r.endpoint = server.URL
		got := r.Read(context.Background(), time.Now())
		server.Close()

		blob, _ := json.Marshal(got)
		if strings.Contains(string(blob), secret) {
			t.Fatalf("%s: the token reached the reported reading: %s", tc.name, blob)
		}
		if got.Fidelity != reading.Unmeasured {
			t.Errorf("%s: fidelity = %q, want %q", tc.name, got.Fidelity, reading.Unmeasured)
		}
	}
}

// A 200 that carries neither window is the vendor changing the shape under us. Reporting it as
// `official` with an empty window list would let a brake believe a reading that does not exist.
func a_response_without_either_window_is_unmeasured(t *testing.T) {
	got := readAgainst(t, http.StatusOK, `{"limits":[],"spend":{}}`, validToken(t))

	if got.Fidelity != reading.Unmeasured {
		t.Fatalf("fidelity = %q, want %q", got.Fidelity, reading.Unmeasured)
	}
	if len(got.Windows) != 0 {
		t.Errorf("an unmeasured reading must carry no windows, got %+v", got.Windows)
	}
}

// The bearer must actually be sent, or this whole package is an elaborate way to get a 401.
func the_credential_travels_as_a_bearer_header(t *testing.T) {
	home := t.TempDir()
	writeCredentials(t, home, "tok-abc", time.Now().Add(time.Hour).UnixMilli())

	var seen string
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		seen = r.Header.Get("Authorization")
		_, _ = w.Write([]byte(capturedShape))
	}))
	defer server.Close()

	r := New(2*time.Second, home)
	r.endpoint = server.URL
	r.Read(context.Background(), time.Now())

	if seen != "Bearer tok-abc" {
		t.Errorf("Authorization = %q, want %q", seen, "Bearer tok-abc")
	}
}

func TestClaude(t *testing.T) {
	t.Run("a percentage named utilization becomes a fraction", a_percentage_named_utilization_becomes_a_fraction)
	t.Run("a high reading is not silently rounded to full", a_high_reading_is_not_silently_rounded_to_full)
	t.Run("the code-named keys are ignored entirely", the_code_named_keys_are_ignored_entirely)
	t.Run("a window without a reset does not crash the reader", a_window_without_a_reset_does_not_crash_the_reader)
	t.Run("an expired token is refused before the call", an_expired_token_is_refused_before_the_call)
	t.Run("no answer ever carries the token", no_answer_ever_carries_the_token)
	t.Run("a response without either window is unmeasured", a_response_without_either_window_is_unmeasured)
	t.Run("the credential travels as a bearer header", the_credential_travels_as_a_bearer_header)
}

func validToken(t *testing.T) string {
	t.Helper()
	home := t.TempDir()
	writeCredentials(t, home, "tok", time.Now().Add(time.Hour).UnixMilli())
	return home
}

func readAgainst(t *testing.T, status int, body, home string) reading.Provider {
	t.Helper()
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.WriteHeader(status)
		_, _ = w.Write([]byte(body))
	}))
	defer server.Close()

	r := New(2*time.Second, home)
	r.endpoint = server.URL
	return r.Read(context.Background(), time.Now())
}

func writeCredentials(t *testing.T, home, token string, expiresAt int64) {
	t.Helper()
	dir := filepath.Join(home, ".claude")
	if err := os.MkdirAll(dir, 0o755); err != nil {
		t.Fatalf("mkdir: %v", err)
	}
	blob := map[string]any{"claudeAiOauth": map[string]any{
		"accessToken": token, "expiresAt": expiresAt, "rateLimitTier": "default_claude_max_5x",
	}}
	raw, _ := json.Marshal(blob)
	if err := os.WriteFile(filepath.Join(dir, ".credentials.json"), raw, 0o600); err != nil {
		t.Fatalf("write: %v", err)
	}
}
