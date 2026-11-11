package claude

import (
	"context"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"
)

const fakeToken = "sk-ant-oat01-fake"

const tokenPayload = `{"five_hour":{"utilization":10.0},"seven_day":{"utilization":20.0}}`

// A token reader sends that token and never touches a credentials file.
func TestNewForTokenSendsTheTokenWithoutAFile(t *testing.T) {
	var auth string
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		auth = r.Header.Get("Authorization")
		_, _ = w.Write([]byte(tokenPayload))
	}))
	defer srv.Close()
	r := NewForToken(time.Second, fakeToken)
	r.endpoint = srv.URL

	got := r.Read(context.Background(), time.Now())

	if auth != "Bearer "+fakeToken {
		t.Errorf("Authorization = %q", auth)
	}
	if len(got.Windows) != 2 {
		t.Errorf("windows = %+v, want 2", got.Windows)
	}
}

// 401 with a token is reported like an invalid file token, without the token in the text.
func TestNewForTokenRefusedIsReportedWithoutTheToken(t *testing.T) {
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.WriteHeader(http.StatusUnauthorized)
	}))
	defer srv.Close()
	r := NewForToken(time.Second, fakeToken)
	r.endpoint = srv.URL

	got := r.Read(context.Background(), time.Now())

	if !strings.Contains(got.Detail, "refused the credential (401)") {
		t.Errorf("detail = %q", got.Detail)
	}
	if strings.Contains(got.Detail, fakeToken) {
		t.Errorf("detail leaks the token: %q", got.Detail)
	}
}

// A setup-token carries only the inference scope, so the usage endpoint may refuse it with 403.
// That is not a dead token: the detail says its usage is not readable, and the reading stays
// unmeasured.
func TestNewForTokenForbiddenSaysUsageIsNotReadable(t *testing.T) {
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.WriteHeader(http.StatusForbidden)
	}))
	defer srv.Close()
	r := NewForToken(time.Second, fakeToken)
	r.endpoint = srv.URL

	got := r.Read(context.Background(), time.Now())

	if !strings.Contains(got.Detail, "setup-token") || !strings.Contains(got.Detail, "403") {
		t.Errorf("detail = %q", got.Detail)
	}
	if strings.Contains(got.Detail, fakeToken) {
		t.Errorf("detail leaks the token: %q", got.Detail)
	}
	if len(got.Windows) != 0 {
		t.Errorf("windows = %+v, want none", got.Windows)
	}
}

// A dir reader keeps the plain refusal text on 403.
func TestDirReaderForbiddenKeepsThePlainRefusal(t *testing.T) {
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.WriteHeader(http.StatusForbidden)
	}))
	defer srv.Close()
	dir := t.TempDir()
	creds := `{"claudeAiOauth":{"accessToken":"tok","expiresAt":0}}`
	if err := os.WriteFile(filepath.Join(dir, ".credentials.json"), []byte(creds), 0o600); err != nil {
		t.Fatal(err)
	}
	r := NewForDir(time.Second, dir)
	r.endpoint = srv.URL

	got := r.Read(context.Background(), time.Now())

	if !strings.Contains(got.Detail, "refused the credential (403)") || strings.Contains(got.Detail, "setup-token") {
		t.Errorf("detail = %q", got.Detail)
	}
}

// Without a token the dir reader is unchanged.
func TestDirReaderStillUsesTheFileWhenNoToken(t *testing.T) {
	got := NewForDir(time.Second, filepath.Join(t.TempDir(), "none")).Read(context.Background(), time.Now())
	if !strings.Contains(got.Detail, "has not signed in") {
		t.Errorf("detail = %q", got.Detail)
	}
}
